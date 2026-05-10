// src/ytdlp.rs – yt-dlp / ffmpeg subprocess management

use crate::types::{DownloadEvent, DownloadItem, DownloadStatus};
use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

// ─── Check if a URL requires yt-dlp ──────────────────────────────────────────

pub fn needs_ytdlp(url: &str) -> bool {
    let lower = url.to_lowercase();
    // Common streaming platforms
    [
        "youtube.com", "youtu.be",
        "vimeo.com",
        "twitch.tv",
        "dailymotion.com",
        "tiktok.com",
        "instagram.com",
        "facebook.com", "fb.watch",
        "twitter.com", "x.com",
        "reddit.com",
        "soundcloud.com",
        "bandcamp.com",
        "bilibili.com",
        "nicovideo.jp",
        "rumble.com",
        "odysee.com",
        "bitchute.com",
        "streamable.com",
        "gfycat.com",
        "imgur.com",
    ]
    .iter()
    .any(|h| lower.contains(h))
}

// ─── Auto-download yt-dlp and ffmpeg if missing ───────────────────────────────

pub async fn ensure_tools(ytdlp_path: &str, ffmpeg_path: &str) -> Result<()> {
    let ytdlp  = PathBuf::from(ytdlp_path);
    let ffmpeg = PathBuf::from(ffmpeg_path);

    if !ytdlp.exists() {
        download_ytdlp(&ytdlp).await?;
    }
    if !ffmpeg.exists() {
        download_ffmpeg(&ffmpeg).await?;
    }
    Ok(())
}

async fn download_ytdlp(dest: &PathBuf) -> Result<()> {
    let url = if cfg!(target_os = "windows") {
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"
    } else {
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp"
    };

    tracing::info!("Downloading yt-dlp from {}", url);

    let client = reqwest::Client::builder()
        .user_agent("YDM/1.0")
        .build()?;

    let bytes = client.get(url).send().await?.bytes().await?;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, &bytes)?;

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }

    tracing::info!("yt-dlp downloaded to {:?}", dest);
    Ok(())
}

async fn download_ffmpeg(dest: &PathBuf) -> Result<()> {
    // We download the static ffmpeg build
    #[cfg(target_os = "windows")]
    let url = "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/ffmpeg-master-latest-win64-gpl.zip";

    #[cfg(not(target_os = "windows"))]
    let url = "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz";

    tracing::info!("ffmpeg auto-download not yet implemented – please place ffmpeg.exe next to YDM");
    tracing::info!("Download from: {}", url);
    // Full extraction is complex – log location for manual install
    // Production version would unzip and extract here
    Ok(())
}

// ─── yt-dlp download task ─────────────────────────────────────────────────────

pub async fn run_ytdlp_download(
    item:     DownloadItem,
    event_tx: mpsc::UnboundedSender<DownloadEvent>,
    cancel:   Arc<AtomicBool>,
) -> Result<()> {
    let id = item.id.clone();

    let _ = event_tx.send(DownloadEvent::StatusChange {
        id:     id.clone(),
        status: DownloadStatus::Connecting,
    });

    let save_dir  = PathBuf::from(&item.save_path);
    let format    = item.format.as_deref().unwrap_or("bestvideo+bestaudio/best");
    let ytdlp     = &item.url; // reuse url field

    // Build yt-dlp command
    let mut cmd = Command::new("yt-dlp");
    cmd.args([
        "--no-playlist",
        "--format", format,
        "--merge-output-format", "mp4",
        "--output", &format!("{}/%(title)s.%(ext)s", save_dir.to_string_lossy()),
        "--progress",
        "--newline",
        "--no-colors",
        "--ffmpeg-location", ".",
        &item.url,
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);

    let _ = event_tx.send(DownloadEvent::StatusChange {
        id:     id.clone(),
        status: DownloadStatus::Downloading,
    });

    let mut child = cmd.spawn().map_err(|e| anyhow!("Failed to start yt-dlp: {e}"))?;

    let stdout = child.stdout.take().ok_or(anyhow!("No stdout"))?;
    let stderr = child.stderr.take().ok_or(anyhow!("No stderr"))?;

    let tx1   = event_tx.clone();
    let id1   = id.clone();
    let tx2   = event_tx.clone();
    let id2   = id.clone();
    let canc1 = cancel.clone();

    // Parse stdout (progress lines)
    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if canc1.load(Ordering::Relaxed) { break; }
            parse_ytdlp_progress(&line, &id1, &tx1);
        }
    });

    // Pipe stderr to log
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if !line.is_empty() {
                let _ = tx2.send(DownloadEvent::Log {
                    id:  id2.clone(),
                    msg: line,
                });
            }
        }
    });

    // Wait for completion or cancel
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill().await;
            let _ = event_tx.send(DownloadEvent::StatusChange {
                id:     id.clone(),
                status: DownloadStatus::Cancelled,
            });
            return Err(anyhow!("Cancelled"));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    break;
                } else {
                    return Err(anyhow!("yt-dlp exited with status: {}", status));
                }
            }
            Ok(None) => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(e) => return Err(anyhow!("yt-dlp wait error: {e}")),
        }
    }

    stdout_task.abort();
    stderr_task.abort();

    Ok(())
}

// ─── Parse yt-dlp progress output ────────────────────────────────────────────
// yt-dlp --newline outputs lines like:
//   [download]   5.3% of   45.23MiB at    2.34MiB/s ETA 00:18
//   [download] 100% of   45.23MiB in 00:19

fn parse_ytdlp_progress(
    line:     &str,
    id:       &str,
    event_tx: &mpsc::UnboundedSender<DownloadEvent>,
) {
    if !line.contains("[download]") {
        return;
    }

    // Try to extract percentage
    let pct = line
        .split_whitespace()
        .find(|t| t.ends_with('%'))
        .and_then(|t| t.trim_end_matches('%').parse::<f64>().ok());

    // Extract speed
    let speed_bps = line
        .split("at")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .map(parse_speed)
        .unwrap_or(0);

    // Extract ETA
    let eta_secs = line
        .split("ETA")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .map(parse_eta)
        .unwrap_or(0);

    if let Some(pct) = pct {
        // Synthesise progress event from percentage
        // We use 1_000_000 as a "virtual" total so we can express progress
        let total      = 1_000_000u64;
        let downloaded = (pct / 100.0 * total as f64) as u64;
        let _ = event_tx.send(DownloadEvent::Progress {
            id:         id.to_string(),
            downloaded,
            total,
            speed:      speed_bps,
            eta:        eta_secs,
        });
    }

    // Log the raw line too
    let _ = event_tx.send(DownloadEvent::Log {
        id:  id.to_string(),
        msg: line.to_string(),
    });
}

fn parse_speed(s: &str) -> u64 {
    let s = s.trim();
    if s.ends_with("GiB/s") {
        return (s.trim_end_matches("GiB/s").trim().parse::<f64>().unwrap_or(0.0) * 1_073_741_824.0) as u64;
    }
    if s.ends_with("MiB/s") {
        return (s.trim_end_matches("MiB/s").trim().parse::<f64>().unwrap_or(0.0) * 1_048_576.0) as u64;
    }
    if s.ends_with("KiB/s") {
        return (s.trim_end_matches("KiB/s").trim().parse::<f64>().unwrap_or(0.0) * 1024.0) as u64;
    }
    0
}

fn parse_eta(s: &str) -> u64 {
    // Format: MM:SS or HH:MM:SS
    let parts: Vec<u64> = s.split(':').filter_map(|p| p.parse().ok()).collect();
    match parts.as_slice() {
        [m, s]    => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _         => 0,
    }
}
