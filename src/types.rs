// src/types.rs – shared types across all YDM modules

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::Arc;
use uuid::Uuid;

// ─── Download status ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadStatus {
    Queued,
    Connecting,
    Downloading,
    Merging,   // ffmpeg mux in progress
    Paused,
    Completed,
    Failed(String),
    Cancelled,
}

impl DownloadStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Connecting | Self::Downloading | Self::Merging)
    }

    pub fn is_done(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed(_) | Self::Cancelled)
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Queued      => "Queued",
            Self::Connecting  => "Connecting",
            Self::Downloading => "Downloading",
            Self::Merging     => "Merging",
            Self::Paused      => "Paused",
            Self::Completed   => "Completed",
            Self::Failed(_)   => "Failed",
            Self::Cancelled   => "Cancelled",
        }
    }

    pub fn color(&self) -> egui::Color32 {
        match self {
            Self::Queued      => egui::Color32::from_rgb(150, 150, 160),
            Self::Connecting  => egui::Color32::from_rgb(100, 160, 255),
            Self::Downloading => egui::Color32::from_rgb(80, 200, 140),
            Self::Merging     => egui::Color32::from_rgb(200, 160, 80),
            Self::Paused      => egui::Color32::from_rgb(200, 180, 80),
            Self::Completed   => egui::Color32::from_rgb(60, 210, 120),
            Self::Failed(_)   => egui::Color32::from_rgb(220, 80, 80),
            Self::Cancelled   => egui::Color32::from_rgb(140, 140, 150),
        }
    }
}

// ─── Download category ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Category {
    Video,
    Audio,
    Document,
    Compressed,
    Other,
}

impl Category {
    pub fn from_url(url: &str) -> Self {
        let lower = url.to_lowercase();
        let ext = lower.split('?').next().unwrap_or("").split('.').last().unwrap_or("");
        match ext {
            "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" | "m4v" => Self::Video,
            "mp3" | "aac" | "flac" | "wav" | "ogg" | "m4a" | "opus"        => Self::Audio,
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" => Self::Document,
            "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz"             => Self::Compressed,
            _                                                                 => Self::Other,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Video      => "Video",
            Self::Audio      => "Audio",
            Self::Document   => "Document",
            Self::Compressed => "Compressed",
            Self::Other      => "Other",
        }
    }

    pub fn icon(&self) -> &str {
        match self {
            Self::Video      => "🎬",
            Self::Audio      => "🎵",
            Self::Document   => "📄",
            Self::Compressed => "📦",
            Self::Other      => "📁",
        }
    }
}

// ─── Download item ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadItem {
    pub id:           String,
    pub url:          String,
    pub filename:     String,
    pub save_path:    String,
    pub category:     Category,
    pub status:       DownloadStatus,
    pub total_bytes:  u64,
    pub downloaded:   u64,
    pub speed_bps:    u64,   // bytes per second (last sample)
    pub eta_secs:     u64,
    pub segments:     usize,
    pub added_at:     DateTime<Utc>,
    pub finished_at:  Option<DateTime<Utc>>,
    pub error:        Option<String>,
    pub is_ytdlp:     bool,   // true = routed through yt-dlp
    pub format:       Option<String>, // e.g. "bestvideo+bestaudio"
}

impl DownloadItem {
    pub fn new(url: String, filename: String, save_path: String) -> Self {
        let category = Category::from_url(&url);
        Self {
            id:          Uuid::new_v4().to_string(),
            url,
            filename,
            save_path,
            category,
            status:      DownloadStatus::Queued,
            total_bytes: 0,
            downloaded:  0,
            speed_bps:   0,
            eta_secs:    0,
            segments:    16,
            added_at:    Utc::now(),
            finished_at: None,
            error:       None,
            is_ytdlp:    false,
            format:      None,
        }
    }

    pub fn progress(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.downloaded as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
    }

    pub fn human_size(&self) -> String {
        use humansize::{format_size, BINARY};
        if self.total_bytes == 0 {
            return "—".to_string();
        }
        format_size(self.total_bytes, BINARY)
    }

    pub fn human_downloaded(&self) -> String {
        use humansize::{format_size, BINARY};
        format_size(self.downloaded, BINARY)
    }

    pub fn human_speed(&self) -> String {
        use humansize::{format_size, BINARY};
        if self.speed_bps == 0 {
            return "—".to_string();
        }
        format!("{}/s", format_size(self.speed_bps, BINARY))
    }

    pub fn human_eta(&self) -> String {
        if self.eta_secs == 0 {
            return "—".to_string();
        }
        if self.eta_secs < 60 {
            return format!("{}s", self.eta_secs);
        }
        if self.eta_secs < 3600 {
            return format!("{}m {}s", self.eta_secs / 60, self.eta_secs % 60);
        }
        format!("{}h {}m", self.eta_secs / 3600, (self.eta_secs % 3600) / 60)
    }
}

// ─── Shared atomic progress (used between download threads and UI) ─────────────

#[derive(Debug, Default)]
pub struct AtomicProgress {
    pub downloaded: AtomicU64,
    pub total:      AtomicU64,
    pub speed:      AtomicU64,
    pub segments:   AtomicUsize,
}

pub type SharedProgress = Arc<AtomicProgress>;

// ─── Messages sent from download tasks to the queue manager ──────────────────

#[derive(Debug)]
pub enum DownloadEvent {
    Progress {
        id:         String,
        downloaded: u64,
        total:      u64,
        speed:      u64,
        eta:        u64,
    },
    StatusChange {
        id:     String,
        status: DownloadStatus,
    },
    Log {
        id:  String,
        msg: String,
    },
}

// ─── API request from browser extension ──────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ApiDownloadRequest {
    pub url:      String,
    pub filename: Option<String>,
    pub referrer: Option<String>,
}

// ─── Config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub save_dir:         String,
    pub max_concurrent:   usize,
    pub segments:         usize,
    pub speed_limit_kbps: u64,   // 0 = unlimited
    pub dark_mode:        bool,
    pub minimize_to_tray: bool,
    pub run_at_startup:   bool,
    pub api_port:         u16,
    pub api_token:        String,
    pub ytdlp_path:       String,
    pub ffmpeg_path:      String,
    pub ytdlp_format:     String,
}

impl Default for Config {
    fn default() -> Self {
        let save_dir = dirs::download_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .to_string_lossy()
            .to_string();

        Self {
            save_dir,
            max_concurrent:   3,
            segments:         16,
            speed_limit_kbps: 0,
            dark_mode:        true,
            minimize_to_tray: true,
            run_at_startup:   false,
            api_port:         9876,
            api_token:        Uuid::new_v4().to_string(),
            ytdlp_path:       "yt-dlp.exe".to_string(),
            ffmpeg_path:      "ffmpeg.exe".to_string(),
            ytdlp_format:     "bestvideo+bestaudio/best".to_string(),
        }
    }
}
