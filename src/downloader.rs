// src/downloader.rs – multi-segment HTTP/HTTPS download engine
//
// Strategy:
//   1. HEAD request to get Content-Length and check Accept-Ranges.
//   2. If server supports ranges AND file > 4 MB: split into N segments,
//      download in parallel, reassemble in order.
//   3. If no ranges support: single-stream fallback.
//   4. Resume: each segment tracks its own byte offset in a .ydm_tmp file.

use crate::types::{DownloadEvent, DownloadItem, DownloadStatus};
use anyhow::{anyhow, Context, Result};
use futures::future::join_all;
use reqwest::{header, Client};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::mpsc;

const MIN_SEGMENT_SIZE: u64 = 4 * 1024 * 1024; // 4 MB minimum per segment

// ─── Public entry point ───────────────────────────────────────────────────────

pub async fn run_download(
    item:      DownloadItem,
    client:    Client,
    event_tx:  mpsc::UnboundedSender<DownloadEvent>,
    cancel:    Arc<AtomicBool>,
    pause:     Arc<AtomicBool>,
    speed_cap: Arc<AtomicU64>, // bytes/sec cap; 0 = unlimited
) -> Result<()> {
    let id = item.id.clone();

    // Notify connecting
    let _ = event_tx.send(DownloadEvent::StatusChange {
        id:     id.clone(),
        status: DownloadStatus::Connecting,
    });

    // HEAD probe
    let probe = probe_url(&client, &item.url).await?;

    let _ = event_tx.send(DownloadEvent::Log {
        id:  id.clone(),
        msg: format!(
            "Size: {} bytes, Ranges: {}",
            probe.content_length.unwrap_or(0),
            probe.accept_ranges
        ),
    });

    // Decide strategy
    let segments = if probe.accept_ranges
        && probe.content_length.unwrap_or(0) > MIN_SEGMENT_SIZE
    {
        item.segments.max(1)
    } else {
        1
    };

    let save_path = PathBuf::from(&item.save_path).join(&item.filename);

    if segments == 1 {
        download_single(
            &client,
            &item.url,
            &save_path,
            &id,
            event_tx.clone(),
            cancel,
            pause,
            speed_cap,
        )
        .await?;
    } else {
        download_segmented(
            &client,
            &item.url,
            &save_path,
            probe.content_length.unwrap(),
            segments,
            &id,
            event_tx.clone(),
            cancel,
            pause,
            speed_cap,
        )
        .await?;
    }

    Ok(())
}

// ─── URL probe ────────────────────────────────────────────────────────────────

struct ProbeResult {
    content_length: Option<u64>,
    accept_ranges:  bool,
    filename_hint:  Option<String>,
}

async fn probe_url(client: &Client, url: &str) -> Result<ProbeResult> {
    let resp = client
        .head(url)
        .header(header::USER_AGENT, "YDM/1.0")
        .send()
        .await
        .context("HEAD request failed")?;

    let content_length = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let accept_ranges = resp
        .headers()
        .get(header::ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "bytes")
        .unwrap_or(false);

    let filename_hint = resp
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .find(|p| p.trim().starts_with("filename="))
                .map(|p| {
                    p.trim()
                        .trim_start_matches("filename=")
                        .trim_matches('"')
                        .to_string()
                })
        });

    Ok(ProbeResult {
        content_length,
        accept_ranges,
        filename_hint,
    })
}

// ─── Single-stream download ───────────────────────────────────────────────────

async fn download_single(
    client:    &Client,
    url:       &str,
    save_path: &Path,
    id:        &str,
    event_tx:  mpsc::UnboundedSender<DownloadEvent>,
    cancel:    Arc<AtomicBool>,
    pause:     Arc<AtomicBool>,
    speed_cap: Arc<AtomicU64>,
) -> Result<()> {
    let resp = client
        .get(url)
        .header(header::USER_AGENT, "YDM/1.0")
        .send()
        .await?;

    let total = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    let _ = event_tx.send(DownloadEvent::StatusChange {
        id:     id.to_string(),
        status: DownloadStatus::Downloading,
    });

    let file  = File::create(save_path).await?;
    let mut writer = BufWriter::new(file);
    let mut downloaded: u64 = 0;
    let mut stream = resp.bytes_stream();
    let mut last_report = Instant::now();
    let mut bytes_since_report: u64 = 0;

    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("Cancelled"));
        }
        while pause.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        let chunk = chunk?;
        writer.write_all(&chunk).await?;
        downloaded        += chunk.len() as u64;
        bytes_since_report += chunk.len() as u64;

        // Speed cap
        let cap = speed_cap.load(Ordering::Relaxed);
        if cap > 0 {
            let expected_ms = (chunk.len() as u64 * 1000) / cap;
            if expected_ms > 0 {
                tokio::time::sleep(Duration::from_millis(expected_ms)).await;
            }
        }

        // Report every 250 ms
        let elapsed = last_report.elapsed();
        if elapsed >= Duration::from_millis(250) {
            let speed = (bytes_since_report as f64 / elapsed.as_secs_f64()) as u64;
            let eta   = if speed > 0 && total > downloaded {
                (total - downloaded) / speed
            } else {
                0
            };
            let _ = event_tx.send(DownloadEvent::Progress {
                id:         id.to_string(),
                downloaded,
                total,
                speed,
                eta,
            });
            last_report        = Instant::now();
            bytes_since_report = 0;
        }
    }

    writer.flush().await?;

    let _ = event_tx.send(DownloadEvent::Progress {
        id:         id.to_string(),
        downloaded,
        total,
        speed:      0,
        eta:        0,
    });

    Ok(())
}

// ─── Segmented download ───────────────────────────────────────────────────────

async fn download_segmented(
    client:    &Client,
    url:       &str,
    save_path: &Path,
    total:     u64,
    segments:  usize,
    id:        &str,
    event_tx:  mpsc::UnboundedSender<DownloadEvent>,
    cancel:    Arc<AtomicBool>,
    pause:     Arc<AtomicBool>,
    speed_cap: Arc<AtomicU64>,
) -> Result<()> {
    let _ = event_tx.send(DownloadEvent::StatusChange {
        id:     id.to_string(),
        status: DownloadStatus::Downloading,
    });

    let segment_size = total / segments as u64;
    let tmp_dir = save_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!(".ydm_{}", id));
    tokio::fs::create_dir_all(&tmp_dir).await?;

    // Shared counters
    let downloaded_total = Arc::new(AtomicU64::new(0));
    let speed_total      = Arc::new(AtomicU64::new(0));

    // Build segment tasks
    let mut tasks = Vec::new();
    for i in 0..segments {
        let start = i as u64 * segment_size;
        let end   = if i == segments - 1 {
            total - 1
        } else {
            start + segment_size - 1
        };

        let client2     = client.clone();
        let url2        = url.to_string();
        let tmp_file    = tmp_dir.join(format!("seg_{:04}", i));
        let dl_counter  = downloaded_total.clone();
        let spd_counter = speed_total.clone();
        let cancel2     = cancel.clone();
        let pause2      = pause.clone();
        let speed_cap2  = speed_cap.clone();
        let tx2         = event_tx.clone();
        let id2         = id.to_string();

        tasks.push(tokio::spawn(async move {
            download_segment(
                &client2, &url2, &tmp_file, start, end,
                dl_counter, spd_counter, cancel2, pause2, speed_cap2,
                tx2, id2, i,
            )
            .await
        }));
    }

    // Progress reporter
    let dl_ref   = downloaded_total.clone();
    let spd_ref  = speed_total.clone();
    let id_str   = id.to_string();
    let tx_ref   = event_tx.clone();
    let cancel_r = cancel.clone();
    let reporter = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if cancel_r.load(Ordering::Relaxed) {
                break;
            }
            let dl    = dl_ref.load(Ordering::Relaxed);
            let speed = spd_ref.swap(0, Ordering::Relaxed) * 4; // per-250ms → per-sec
            let eta   = if speed > 0 && total > dl { (total - dl) / speed } else { 0 };
            let _ = tx_ref.send(DownloadEvent::Progress {
                id:         id_str.clone(),
                downloaded: dl,
                total,
                speed,
                eta,
            });
            if dl >= total { break; }
        }
    });

    // Wait for all segments
    let results = join_all(tasks).await;
    reporter.abort();

    for r in results {
        r.map_err(|e| anyhow!("Segment task panicked: {e}"))??;
    }

    if cancel.load(Ordering::Relaxed) {
        return Err(anyhow!("Cancelled"));
    }

    // Reassemble
    let _ = event_tx.send(DownloadEvent::Log {
        id:  id.to_string(),
        msg: "Reassembling segments…".to_string(),
    });

    let mut out = File::create(save_path).await?;
    for i in 0..segments {
        let tmp_file = tmp_dir.join(format!("seg_{:04}", i));
        let data     = tokio::fs::read(&tmp_file).await?;
        out.write_all(&data).await?;
    }
    out.flush().await?;

    // Cleanup temp dir
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

    Ok(())
}

async fn download_segment(
    client:     &Client,
    url:        &str,
    tmp_file:   &Path,
    start:      u64,
    end:        u64,
    dl_counter: Arc<AtomicU64>,
    spd_counter:Arc<AtomicU64>,
    cancel:     Arc<AtomicBool>,
    pause:      Arc<AtomicBool>,
    speed_cap:  Arc<AtomicU64>,
    event_tx:   mpsc::UnboundedSender<DownloadEvent>,
    id:         String,
    seg_idx:    usize,
) -> Result<()> {
    // Resume: check how many bytes already written
    let already = if tmp_file.exists() {
        tokio::fs::metadata(tmp_file).await?.len()
    } else {
        0
    };

    let actual_start = start + already;
    if actual_start > end {
        // Already complete
        dl_counter.fetch_add(end - start + 1, Ordering::Relaxed);
        return Ok(());
    }

    dl_counter.fetch_add(already, Ordering::Relaxed);

    let range_header = format!("bytes={}-{}", actual_start, end);
    let resp = client
        .get(url)
        .header(header::USER_AGENT, "YDM/1.0")
        .header(header::RANGE, &range_header)
        .send()
        .await
        .with_context(|| format!("Segment {} GET failed", seg_idx))?;

    // Append mode if resuming
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(already > 0)
        .write(already == 0)
        .open(tmp_file)
        .await?;
    let mut writer = BufWriter::new(file);

    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut last_speed_sample = Instant::now();
    let mut bytes_since_sample: u64 = 0;

    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            writer.flush().await?;
            return Err(anyhow!("Cancelled"));
        }
        while pause.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        let chunk = chunk?;
        writer.write_all(&chunk).await?;
        let len = chunk.len() as u64;
        dl_counter.fetch_add(len, Ordering::Relaxed);
        bytes_since_sample += len;

        // Speed cap
        let cap = speed_cap.load(Ordering::Relaxed);
        if cap > 0 {
            let expected_ms = (len * 1000) / cap;
            if expected_ms > 0 {
                tokio::time::sleep(Duration::from_millis(expected_ms)).await;
            }
        }

        // Contribute to shared speed counter every 250ms
        if last_speed_sample.elapsed() >= Duration::from_millis(250) {
            spd_counter.fetch_add(bytes_since_sample, Ordering::Relaxed);
            bytes_since_sample  = 0;
            last_speed_sample   = Instant::now();
        }
    }

    writer.flush().await?;
    Ok(())
}
