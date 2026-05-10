// src/queue.rs – download queue manager
//
// Owns all DownloadItems, drives the downloader/ytdlp engines,
// enforces concurrency limits, and forwards events to the UI.

use crate::config;
use crate::downloader;
use crate::types::{Config, DownloadEvent, DownloadItem, DownloadStatus};
use crate::ytdlp;
use anyhow::Result;
use reqwest::Client;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use uuid::Uuid;

// ─── Per-download control handles ────────────────────────────────────────────

struct DownloadHandle {
    cancel: Arc<AtomicBool>,
    pause:  Arc<AtomicBool>,
}

// ─── Queue manager ────────────────────────────────────────────────────────────

pub struct QueueManager {
    pub items:    Arc<Mutex<Vec<DownloadItem>>>,
    pub history:  Arc<Mutex<Vec<DownloadItem>>>,
    pub logs:     Arc<Mutex<HashMap<String, Vec<String>>>>,
    handles:      Arc<Mutex<HashMap<String, DownloadHandle>>>,
    active_count: Arc<AtomicU64>,
    event_tx:     mpsc::UnboundedSender<DownloadEvent>,
    event_rx:     Option<mpsc::UnboundedReceiver<DownloadEvent>>,
    client:       Client,
    pub cfg:      Arc<Mutex<Config>>,
}

impl QueueManager {
    pub fn new(cfg: Config) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = Client::builder()
            .user_agent("YDM/1.0")
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .pool_max_idle_per_host(32)
            .build()
            .expect("Failed to build HTTP client");

        let history = config::load_history();

        Self {
            items:        Arc::new(Mutex::new(vec![])),
            history:      Arc::new(Mutex::new(history)),
            logs:         Arc::new(Mutex::new(HashMap::new())),
            handles:      Arc::new(Mutex::new(HashMap::new())),
            active_count: Arc::new(AtomicU64::new(0)),
            event_tx:     tx,
            event_rx:     Some(rx),
            client,
            cfg:          Arc::new(Mutex::new(cfg)),
        }
    }

    // ── Add a new download ────────────────────────────────────────────────────

    pub fn add(&self, url: String, filename: Option<String>, referrer: Option<String>) -> String {
        let fname = filename.unwrap_or_else(|| guess_filename(&url));
        let save_path = {
            let cfg = self.cfg.lock().unwrap();
            cfg.save_dir.clone()
        };
        let is_ytdlp = ytdlp::needs_ytdlp(&url);
        let mut item = DownloadItem::new(url, fname, save_path);
        item.is_ytdlp = is_ytdlp;
        {
            let cfg = self.cfg.lock().unwrap();
            item.segments  = cfg.segments;
            item.format    = Some(cfg.ytdlp_format.clone());
        }
        let id = item.id.clone();
        self.items.lock().unwrap().push(item);
        self.try_start_next();
        id
    }

    // ── Try to start queued downloads up to concurrency limit ─────────────────

    pub fn try_start_next(&self) {
        let max = {
            let cfg = self.cfg.lock().unwrap();
            cfg.max_concurrent as u64
        };

        loop {
            let active = self.active_count.load(Ordering::Relaxed);
            if active >= max {
                break;
            }

            // Find next queued item
            let item = {
                let items = self.items.lock().unwrap();
                items
                    .iter()
                    .find(|i| i.status == DownloadStatus::Queued)
                    .cloned()
            };

            let Some(item) = item else { break };

            self.active_count.fetch_add(1, Ordering::Relaxed);
            self.start_download(item);
        }
    }

    // ── Start a single download task ──────────────────────────────────────────

    fn start_download(&self, item: DownloadItem) {
        let id      = item.id.clone();
        let cancel  = Arc::new(AtomicBool::new(false));
        let pause   = Arc::new(AtomicBool::new(false));
        let speed_cap = {
            let cfg   = self.cfg.lock().unwrap();
            let kbps  = cfg.speed_limit_kbps;
            Arc::new(AtomicU64::new(if kbps > 0 { kbps * 1024 } else { 0 }))
        };

        self.handles.lock().unwrap().insert(
            id.clone(),
            DownloadHandle {
                cancel: cancel.clone(),
                pause:  pause.clone(),
            },
        );

        let event_tx     = self.event_tx.clone();
        let client       = self.client.clone();
        let active_count = self.active_count.clone();
        let items_arc    = self.items.clone();
        let history_arc  = self.history.clone();
        let handles_arc  = self.handles.clone();
        let id2          = id.clone();

        tokio::spawn(async move {
            let result = if item.is_ytdlp {
                ytdlp::run_ytdlp_download(item.clone(), event_tx.clone(), cancel).await
            } else {
                downloader::run_download(
                    item.clone(), client, event_tx.clone(), cancel, pause, speed_cap,
                )
                .await
            };

            // Send final status
            let final_status = match result {
                Ok(_)  => DownloadStatus::Completed,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("Cancelled") {
                        DownloadStatus::Cancelled
                    } else {
                        DownloadStatus::Failed(msg)
                    }
                }
            };

            let _ = event_tx.send(DownloadEvent::StatusChange {
                id:     id2.clone(),
                status: final_status.clone(),
            });

            // Move to history if completed
            if final_status == DownloadStatus::Completed {
                let mut items   = items_arc.lock().unwrap();
                let mut history = history_arc.lock().unwrap();
                if let Some(pos) = items.iter().position(|i| i.id == id2) {
                    let mut finished = items.remove(pos);
                    finished.status      = DownloadStatus::Completed;
                    finished.finished_at = Some(chrono::Utc::now());
                    history.insert(0, finished);
                    history.truncate(500);
                    let _ = config::save_history(&history);
                }
            }

            handles_arc.lock().unwrap().remove(&id2);
            active_count.fetch_sub(1, Ordering::Relaxed);
        });
    }

    // ── Control actions ───────────────────────────────────────────────────────

    pub fn pause(&self, id: &str) {
        if let Some(h) = self.handles.lock().unwrap().get(id) {
            h.pause.store(true, Ordering::Relaxed);
        }
        self.set_status(id, DownloadStatus::Paused);
    }

    pub fn resume(&self, id: &str) {
        // Un-pause if handle exists
        if let Some(h) = self.handles.lock().unwrap().get(id) {
            h.pause.store(false, Ordering::Relaxed);
            self.set_status(id, DownloadStatus::Downloading);
            return;
        }
        // Otherwise re-queue
        self.set_status(id, DownloadStatus::Queued);
        self.try_start_next();
    }

    pub fn cancel(&self, id: &str) {
        if let Some(h) = self.handles.lock().unwrap().get(id) {
            h.cancel.store(true, Ordering::Relaxed);
        }
        self.set_status(id, DownloadStatus::Cancelled);
    }

    pub fn remove(&self, id: &str) {
        self.cancel(id);
        let mut items = self.items.lock().unwrap();
        items.retain(|i| i.id != id);
    }

    pub fn retry(&self, id: &str) {
        let item = {
            let mut items = self.items.lock().unwrap();
            if let Some(i) = items.iter_mut().find(|i| i.id == id) {
                i.status     = DownloadStatus::Queued;
                i.downloaded = 0;
                i.error      = None;
                Some(i.clone())
            } else {
                None
            }
        };
        if item.is_some() {
            self.try_start_next();
        }
    }

    pub fn cancel_all(&self) {
        let ids: Vec<String> = self.items.lock().unwrap()
            .iter()
            .map(|i| i.id.clone())
            .collect();
        for id in ids {
            self.cancel(&id);
        }
    }

    // ── Event processing (called from UI thread each frame) ───────────────────

    pub fn take_event_rx(&mut self) -> Option<mpsc::UnboundedReceiver<DownloadEvent>> {
        self.event_rx.take()
    }

    pub fn process_events(&self, rx: &mut mpsc::UnboundedReceiver<DownloadEvent>) {
        while let Ok(event) = rx.try_recv() {
            match event {
                DownloadEvent::Progress { id, downloaded, total, speed, eta } => {
                    let mut items = self.items.lock().unwrap();
                    if let Some(item) = items.iter_mut().find(|i| i.id == id) {
                        item.downloaded  = downloaded;
                        item.total_bytes = total;
                        item.speed_bps   = speed;
                        item.eta_secs    = eta;
                    }
                }
                DownloadEvent::StatusChange { id, status } => {
                    let mut items = self.items.lock().unwrap();
                    if let Some(item) = items.iter_mut().find(|i| i.id == id) {
                        if let DownloadStatus::Failed(ref msg) = status {
                            item.error = Some(msg.clone());
                        }
                        item.status = status;
                    }
                    drop(items);
                    self.try_start_next();
                }
                DownloadEvent::Log { id, msg } => {
                    let mut logs = self.logs.lock().unwrap();
                    let entry    = logs.entry(id).or_default();
                    entry.push(msg);
                    if entry.len() > 500 {
                        entry.remove(0);
                    }
                }
            }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn set_status(&self, id: &str, status: DownloadStatus) {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            item.status = status;
        }
    }
}

fn guess_filename(url: &str) -> String {
    url.split('?')
        .next()
        .unwrap_or(url)
        .split('/')
        .last()
        .filter(|s| !s.is_empty())
        .map(|s| urlencoding_decode(s))
        .unwrap_or_else(|| format!("download_{}.bin", &Uuid::new_v4().to_string()[..8]))
}

fn urlencoding_decode(s: &str) -> String {
    // Simple percent-decode
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next().unwrap_or('0');
            let h2 = chars.next().unwrap_or('0');
            if let Ok(b) = u8::from_str_radix(&format!("{}{}", h1, h2), 16) {
                out.push(b as char);
                continue;
            }
        }
        out.push(c);
    }
    out
}
