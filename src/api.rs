// src/api.rs – localhost REST API consumed by the browser extension
//
// Endpoints:
//   GET  /ping                         → 200 OK  (extension heartbeat)
//   POST /download  { url, filename }  → 200 { id }
//   GET  /status/:id                   → 200 { ...item }
//   GET  /queue                        → 200 [ ...items ]
//
// All requests must carry:
//   Authorization: Bearer <token>
//
// The token is written to %APPDATA%\YDM\token.txt on startup so the
// extension can read it.

use crate::queue::QueueManager;
use crate::types::ApiDownloadRequest;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

// ─── Token file ───────────────────────────────────────────────────────────────

pub fn write_token_file(token: &str) -> Result<()> {
    let mut path = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    path.push("YDM");
    std::fs::create_dir_all(&path)?;
    path.push("token.txt");
    std::fs::write(&path, token)?;
    tracing::info!("API token written to {:?}", path);
    Ok(())
}

pub fn token_file_path() -> String {
    let mut path = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    path.push("YDM");
    path.push("token.txt");
    path.to_string_lossy().to_string()
}

// ─── Server ───────────────────────────────────────────────────────────────────

pub struct ApiServer {
    _handle: thread::JoinHandle<()>,
}

impl ApiServer {
    pub fn start(port: u16, token: String, queue: Arc<Mutex<QueueManager>>) -> Result<Self> {
        let listener = TcpListener::bind(format!("127.0.0.1:{}", port))?;
        tracing::info!("API server listening on 127.0.0.1:{}", port);

        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let token2 = token.clone();
                        let queue2 = queue.clone();
                        thread::spawn(move || {
                            if let Err(e) = handle_connection(stream, &token2, &queue2) {
                                tracing::debug!("API connection error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("API accept error: {e}");
                    }
                }
            }
        });

        Ok(Self { _handle: handle })
    }
}

// ─── Request handler ──────────────────────────────────────────────────────────

fn handle_connection(
    mut stream: TcpStream,
    token:      &str,
    queue:      &Arc<Mutex<QueueManager>>,
) -> Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;

    let mut reader = BufReader::new(stream.try_clone()?);

    // Read request line
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let request_line = request_line.trim().to_string();

    // Read headers
    let mut headers: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim();
        if line.is_empty() { break; }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_lowercase();
            let val = v.trim().to_string();
            if key == "content-length" {
                content_length = val.parse().unwrap_or(0);
            }
            headers.insert(key, val);
        }
    }

    // Read body
    let mut body = String::new();
    if content_length > 0 {
        let mut buf = vec![0u8; content_length.min(65536)];
        use std::io::Read;
        reader.read_exact(&mut buf)?;
        body = String::from_utf8_lossy(&buf).to_string();
    }

    // Parse method + path
    let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return respond(&mut stream, 400, "text/plain", b"Bad Request");
    }
    let method = parts[0];
    let path   = parts[1];

    // CORS pre-flight
    if method == "OPTIONS" {
        return respond_cors(&mut stream);
    }

    // Auth check (skip for ping)
    if path != "/ping" {
        let auth = headers.get("authorization").map(|s| s.as_str()).unwrap_or("");
        let expected = format!("Bearer {}", token);
        if auth != expected {
            return respond(&mut stream, 401, "application/json",
                           b"{\"error\":\"Unauthorized\"}");
        }
    }

    // Route
    match (method, path) {
        ("GET",  "/ping")     => respond_json(&mut stream, 200, &json!({"status":"ok","app":"YDM"})),
        ("GET",  "/queue")    => {
            let q     = queue.lock().unwrap();
            let items = q.items.lock().unwrap();
            let data  = serde_json::to_value(&*items)?;
            respond_json(&mut stream, 200, &data)
        }
        ("POST", "/download") => {
            let req: ApiDownloadRequest = serde_json::from_str(&body)
                .map_err(|e| anyhow::anyhow!("Bad request body: {e}"))?;
            let q  = queue.lock().unwrap();
            let id = q.add(req.url, req.filename, req.referrer);
            respond_json(&mut stream, 200, &json!({"id": id, "status": "queued"}))
        }
        ("GET", path) if path.starts_with("/status/") => {
            let id    = &path["/status/".len()..];
            let q     = queue.lock().unwrap();
            let items = q.items.lock().unwrap();
            if let Some(item) = items.iter().find(|i| i.id == id) {
                let data = serde_json::to_value(item)?;
                respond_json(&mut stream, 200, &data)
            } else {
                respond_json(&mut stream, 404, &json!({"error":"Not found"}))
            }
        }
        _ => respond(&mut stream, 404, "text/plain", b"Not Found"),
    }
}

fn respond(stream: &mut TcpStream, code: u16, ct: &str, body: &[u8]) -> Result<()> {
    let status = match code {
        200 => "200 OK",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        404 => "404 Not Found",
        _   => "500 Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\nConnection: close\r\n\r\n",
        status, ct, body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn respond_json(stream: &mut TcpStream, code: u16, data: &Value) -> Result<()> {
    let body = serde_json::to_vec(data)?;
    respond(stream, code, "application/json", &body)
}

fn respond_cors(stream: &mut TcpStream) -> Result<()> {
    let response =
        "HTTP/1.1 204 No Content\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Authorization, Content-Type\r\n\
         Connection: close\r\n\r\n";
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}
