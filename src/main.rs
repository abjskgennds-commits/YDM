// src/main.rs – YDM entry point

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod app;
mod browser;
mod config;
mod downloader;
mod queue;
mod types;
mod ytdlp;

use anyhow::Result;
use eframe::egui;

fn main() -> Result<()> {
    // Init tracing (logs to stdout in debug, silent in release)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ydm=info")),
        )
        .init();

    // Start tokio runtime for async tasks
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .build()?;

    let _guard = rt.enter();

    // Parse CLI args
    let args: Vec<String> = std::env::args().collect();
    let start_minimized = args.iter().any(|a| a == "--tray");

    // eframe native options
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("YDM – Download Manager")
            .with_inner_size([900.0, 620.0])
            .with_min_inner_size([640.0, 420.0])
            .with_icon(load_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "YDM",
        options,
        Box::new(|cc| Box::new(app::YdmApp::new(cc))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    Ok(())
}

fn load_icon() -> egui::IconData {
    // Embedded 32x32 RGBA icon – blue arrow-down on dark background.
    // Replace with your own icon by providing a 32x32 RGBA byte array.
    let size = 32usize;
    let mut rgba = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let i = (y * size + x) * 4;
            // Dark background
            rgba[i]     = 13;
            rgba[i + 1] = 17;
            rgba[i + 2] = 23;
            rgba[i + 3] = 255;
            // Draw a simple down-arrow in accent blue
            let cx = size / 2;
            let cy = size / 2;
            let in_shaft = x >= cx - 3 && x <= cx + 3 && y >= 6 && y <= cy + 4;
            let in_head  = y >= cy + 4
                && y <= cy + 10
                && x >= cx.saturating_sub(y - cy - 4 + 3)
                && x <= cx + (y - cy - 4 + 3);
            if in_shaft || in_head {
                rgba[i]     = 88;
                rgba[i + 1] = 166;
                rgba[i + 2] = 255;
                rgba[i + 3] = 255;
            }
        }
    }
    egui::IconData { rgba, width: size as u32, height: size as u32 }
}
