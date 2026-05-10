// src/app.rs – YDM main egui application
//
// Pages: Queue | Completed | History | Settings
// Persistent tray icon (Windows)

use crate::api::ApiServer;
use crate::browser;
use crate::config;
use crate::queue::QueueManager;
use crate::types::{Config, DownloadStatus};
use eframe::egui;
use egui::{Color32, FontId, RichText, Stroke, Vec2};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

// ─── Colours ──────────────────────────────────────────────────────────────────

const C_BG:         Color32 = Color32::from_rgb(13,  17,  23);
const C_SURFACE:    Color32 = Color32::from_rgb(22,  27,  34);
const C_CARD:       Color32 = Color32::from_rgb(30,  37,  46);
const C_BORDER:     Color32 = Color32::from_rgb(48,  54,  61);
const C_TEXT:       Color32 = Color32::from_rgb(230, 237, 243);
const C_TEXT_DIM:   Color32 = Color32::from_rgb(139, 148, 158);
const C_ACCENT:     Color32 = Color32::from_rgb(88,  166, 255);
const C_SUCCESS:    Color32 = Color32::from_rgb(63,  185, 80);
const C_WARN:       Color32 = Color32::from_rgb(210, 153, 34);
const C_DANGER:     Color32 = Color32::from_rgb(248, 81,  73);

// ─── Page enum ────────────────────────────────────────────────────────────────

#[derive(PartialEq, Eq, Clone, Copy)]
enum Page {
    Queue,
    Completed,
    History,
    Settings,
}

// ─── Add-download dialog state ────────────────────────────────────────────────

struct AddDialog {
    open:     bool,
    url:      String,
    filename: String,
    error:    Option<String>,
}

impl Default for AddDialog {
    fn default() -> Self {
        Self { open: false, url: String::new(), filename: String::new(), error: None }
    }
}

// ─── Main app ─────────────────────────────────────────────────────────────────

pub struct YdmApp {
    page:       Page,
    queue:      Arc<Mutex<QueueManager>>,
    event_rx:   mpsc::UnboundedReceiver<crate::types::DownloadEvent>,
    cfg:        Config,
    add_dialog: AddDialog,
    log_filter: String,     // search filter for logs page
    search:     String,     // global search
    _api:       Option<ApiServer>,
    // browser integration report (shown once in settings)
    integration_report: Option<browser::IntegrationReport>,
    status_bar_msg: Option<String>,
}

impl YdmApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        setup_style(&cc.egui_ctx);

        let cfg = config::load();
        let mut qm = QueueManager::new(cfg.clone());
        let rx = qm.take_event_rx().expect("event_rx taken twice");

        let queue = Arc::new(Mutex::new(qm));

        // Start API server
        let api = ApiServer::start(
            cfg.api_port,
            cfg.api_token.clone(),
            queue.clone(),
        )
        .map_err(|e| tracing::error!("API server failed: {e}"))
        .ok();

        // Write token file for extension
        let _ = crate::api::write_token_file(&cfg.api_token);

        // Silent browser integration
        let exe_dir = std::env::current_exe()
            .unwrap_or_default()
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();

        let integration_report = if !browser::integration_done() {
            let report = browser::run_integration(&exe_dir);
            browser::mark_integration_done();
            Some(report)
        } else {
            None
        };

        Self {
            page:       Page::Queue,
            queue,
            event_rx:   rx,
            cfg,
            add_dialog: AddDialog::default(),
            log_filter: String::new(),
            search:     String::new(),
            _api:       api,
            integration_report,
            status_bar_msg: None,
        }
    }

    // ── Process incoming download events ──────────────────────────────────────

    fn drain_events(&mut self) {
        let q = self.queue.lock().unwrap();
        q.process_events(&mut self.event_rx);
    }

    // ── Top bar ───────────────────────────────────────────────────────────────

    fn draw_topbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Logo + name
            ui.add_space(8.0);
            ui.label(
                RichText::new("⬇ YDM")
                    .color(C_ACCENT)
                    .font(FontId::proportional(20.0))
                    .strong(),
            );
            ui.add_space(16.0);

            // Nav tabs
            for (label, p) in [
                ("Queue",     Page::Queue),
                ("Completed", Page::Completed),
                ("History",   Page::History),
                ("Settings",  Page::Settings),
            ] {
                let selected = self.page == p;
                let text = RichText::new(label)
                    .color(if selected { C_ACCENT } else { C_TEXT_DIM })
                    .font(FontId::proportional(13.0));
                if ui.selectable_label(selected, text).clicked() {
                    self.page = p;
                }
                ui.add_space(4.0);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Add download button
                let add_btn = egui::Button::new(
                    RichText::new("＋ Add Download").color(Color32::WHITE).font(FontId::proportional(13.0))
                )
                .fill(C_ACCENT)
                .rounding(6.0);

                if ui.add(add_btn).clicked() {
                    self.add_dialog.open = true;
                }
                ui.add_space(8.0);

                // Search
                let search_hint = egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search…")
                    .desired_width(160.0)
                    .font(FontId::proportional(12.0));
                ui.add(search_hint);
                ui.add_space(8.0);
            });
        });
    }

    // ── Queue page ────────────────────────────────────────────────────────────

    fn draw_queue_page(&mut self, ui: &mut egui::Ui) {
        let items: Vec<_> = {
            let q     = self.queue.lock().unwrap();
            let items = q.items.lock().unwrap();
            items
                .iter()
                .filter(|i| {
                    self.search.is_empty()
                        || i.filename.to_lowercase().contains(&self.search.to_lowercase())
                        || i.url.to_lowercase().contains(&self.search.to_lowercase())
                })
                .cloned()
                .collect()
        };

        if items.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("No active downloads\nClick '＋ Add Download' to start")
                        .color(C_TEXT_DIM)
                        .font(FontId::proportional(14.0)),
                );
            });
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for item in &items {
                self.draw_download_card(ui, item);
                ui.add_space(4.0);
            }
        });
    }

    fn draw_download_card(&self, ui: &mut egui::Ui, item: &crate::types::DownloadItem) {
        let available = ui.available_width();

        egui::Frame::none()
            .fill(C_CARD)
            .rounding(8.0)
            .stroke(Stroke::new(1.0, C_BORDER))
            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
            .show(ui, |ui| {
                ui.set_width(available - 2.0);

                // Row 1: filename + status + category
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(item.category.icon())
                            .font(FontId::proportional(16.0)),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(&item.filename)
                            .color(C_TEXT)
                            .font(FontId::proportional(13.0))
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Status badge
                        let status_text = RichText::new(item.status.label())
                            .color(item.status.color())
                            .font(FontId::proportional(11.0))
                            .strong();
                        ui.label(status_text);
                        ui.add_space(8.0);

                        // Control buttons
                        if item.status.is_active() {
                            if ui.small_button("⏸").on_hover_text("Pause").clicked() {
                                self.queue.lock().unwrap().pause(&item.id);
                            }
                            if ui.small_button("✕").on_hover_text("Cancel").clicked() {
                                self.queue.lock().unwrap().cancel(&item.id);
                            }
                        } else if item.status == DownloadStatus::Paused {
                            if ui.small_button("▶").on_hover_text("Resume").clicked() {
                                self.queue.lock().unwrap().resume(&item.id);
                            }
                            if ui.small_button("✕").on_hover_text("Cancel").clicked() {
                                self.queue.lock().unwrap().cancel(&item.id);
                            }
                        } else if matches!(item.status, DownloadStatus::Failed(_)) {
                            if ui.small_button("↺").on_hover_text("Retry").clicked() {
                                self.queue.lock().unwrap().retry(&item.id);
                            }
                        }
                        if ui.small_button("🗑").on_hover_text("Remove").clicked() {
                            self.queue.lock().unwrap().remove(&item.id);
                        }
                    });
                });

                ui.add_space(4.0);

                // Row 2: progress bar
                let progress = item.progress();
                let bar_rect = ui.available_rect_before_wrap();
                let bar_h    = 6.0;
                let bar_rect = egui::Rect::from_min_size(
                    bar_rect.min,
                    Vec2::new(bar_rect.width(), bar_h),
                );
                ui.allocate_rect(bar_rect, egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(bar_rect, 3.0, C_BORDER);
                if progress > 0.0 {
                    let filled = egui::Rect::from_min_size(
                        bar_rect.min,
                        Vec2::new(bar_rect.width() * progress, bar_h),
                    );
                    painter.rect_filled(filled, 3.0, C_ACCENT);
                }

                ui.add_space(4.0);

                // Row 3: stats
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{:.1}%", progress * 100.0))
                            .color(C_ACCENT)
                            .font(FontId::proportional(11.0)),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("{} / {}", item.human_downloaded(), item.human_size()))
                            .color(C_TEXT_DIM)
                            .font(FontId::proportional(11.0)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if item.eta_secs > 0 {
                            ui.label(
                                RichText::new(format!("ETA {}", item.human_eta()))
                                    .color(C_TEXT_DIM)
                                    .font(FontId::proportional(11.0)),
                            );
                            ui.add_space(8.0);
                        }
                        if item.speed_bps > 0 {
                            ui.label(
                                RichText::new(item.human_speed())
                                    .color(C_SUCCESS)
                                    .font(FontId::proportional(11.0))
                                    .strong(),
                            );
                        }
                    });
                });

                // Show error if failed
                if let DownloadStatus::Failed(ref err) = item.status {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("Error: {}", err))
                            .color(C_DANGER)
                            .font(FontId::proportional(11.0)),
                    );
                }

                // URL (dimmed, truncated)
                ui.add_space(2.0);
                let url_display = if item.url.len() > 80 {
                    format!("{}…", &item.url[..80])
                } else {
                    item.url.clone()
                };
                ui.label(
                    RichText::new(url_display)
                        .color(C_TEXT_DIM)
                        .font(FontId::proportional(10.0)),
                );
            });
    }

    // ── Completed page ────────────────────────────────────────────────────────

    fn draw_completed_page(&mut self, ui: &mut egui::Ui) {
        let items: Vec<_> = {
            let q = self.queue.lock().unwrap();
            let h = q.history.lock().unwrap();
            h.iter()
                .filter(|i| i.status == DownloadStatus::Completed)
                .filter(|i| {
                    self.search.is_empty()
                        || i.filename.to_lowercase().contains(&self.search.to_lowercase())
                })
                .cloned()
                .collect()
        };

        if items.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("No completed downloads yet")
                        .color(C_TEXT_DIM)
                        .font(FontId::proportional(14.0)),
                );
            });
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for item in &items {
                egui::Frame::none()
                    .fill(C_CARD)
                    .rounding(8.0)
                    .stroke(Stroke::new(1.0, C_BORDER))
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(item.category.icon()).font(FontId::proportional(16.0)));
                            ui.add_space(4.0);
                            ui.label(RichText::new(&item.filename).color(C_TEXT).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("📂").on_hover_text("Open folder").clicked() {
                                    let _ = open::that(&item.save_path);
                                }
                                ui.label(
                                    RichText::new(item.human_size())
                                        .color(C_TEXT_DIM)
                                        .font(FontId::proportional(11.0)),
                                );
                                ui.add_space(8.0);
                                if let Some(finished) = item.finished_at {
                                    ui.label(
                                        RichText::new(finished.format("%Y-%m-%d %H:%M").to_string())
                                            .color(C_TEXT_DIM)
                                            .font(FontId::proportional(11.0)),
                                    );
                                }
                            });
                        });
                    });
                ui.add_space(4.0);
            }
        });
    }

    // ── History page ──────────────────────────────────────────────────────────

    fn draw_history_page(&mut self, ui: &mut egui::Ui) {
        let items: Vec<_> = {
            let q = self.queue.lock().unwrap();
            let h = q.history.lock().unwrap();
            h.iter()
                .filter(|i| {
                    self.search.is_empty()
                        || i.filename.to_lowercase().contains(&self.search.to_lowercase())
                        || i.url.to_lowercase().contains(&self.search.to_lowercase())
                })
                .cloned()
                .collect()
        };

        if items.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("No history").color(C_TEXT_DIM).font(FontId::proportional(14.0)));
            });
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for item in &items {
                egui::Frame::none()
                    .fill(C_CARD)
                    .rounding(8.0)
                    .stroke(Stroke::new(1.0, C_BORDER))
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(item.category.icon()).font(FontId::proportional(14.0)));
                            ui.add_space(4.0);
                            ui.label(RichText::new(&item.filename).color(C_TEXT).font(FontId::proportional(12.0)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let status_col = match &item.status {
                                    DownloadStatus::Completed      => C_SUCCESS,
                                    DownloadStatus::Failed(_)      => C_DANGER,
                                    DownloadStatus::Cancelled      => C_TEXT_DIM,
                                    _                              => C_TEXT_DIM,
                                };
                                ui.label(
                                    RichText::new(item.status.label())
                                        .color(status_col)
                                        .font(FontId::proportional(11.0)),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new(item.human_size())
                                        .color(C_TEXT_DIM)
                                        .font(FontId::proportional(11.0)),
                                );
                            });
                        });
                    });
                ui.add_space(2.0);
            }
        });
    }

    // ── Settings page ─────────────────────────────────────────────────────────

    fn draw_settings_page(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.set_width(ui.available_width());

            self.settings_section(ui, "Download", |ui, cfg| {
                // Save directory
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Save folder:").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                    ui.add(egui::TextEdit::singleline(&mut cfg.save_dir).desired_width(300.0));
                    if ui.small_button("Browse").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            cfg.save_dir = path.to_string_lossy().to_string();
                        }
                    }
                });
                ui.add_space(6.0);

                // Concurrent downloads
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Concurrent downloads:").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                    ui.add(egui::Slider::new(&mut cfg.max_concurrent, 1..=10).text(""));
                });
                ui.add_space(6.0);

                // Segments
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Segments per download:").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                    ui.add(egui::Slider::new(&mut cfg.segments, 1..=32).text(""));
                });
                ui.add_space(6.0);

                // Speed limit
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Speed limit (KB/s, 0=unlimited):").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                    ui.add(egui::DragValue::new(&mut cfg.speed_limit_kbps).speed(10.0).suffix(" KB/s"));
                });
            });

            ui.add_space(12.0);

            self.settings_section(ui, "Video (yt-dlp)", |ui, cfg| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("yt-dlp path:").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                    ui.add(egui::TextEdit::singleline(&mut cfg.ytdlp_path).desired_width(260.0));
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("ffmpeg path:").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                    ui.add(egui::TextEdit::singleline(&mut cfg.ffmpeg_path).desired_width(260.0));
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Format:").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                    ui.add(egui::TextEdit::singleline(&mut cfg.ytdlp_format).desired_width(260.0));
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("e.g. bestvideo+bestaudio/best  |  bestvideo[height<=1080]+bestaudio")
                        .color(C_TEXT_DIM)
                        .font(FontId::proportional(10.0)),
                );
            });

            ui.add_space(12.0);

            self.settings_section(ui, "Application", |ui, cfg| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Minimize to tray:").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                    ui.checkbox(&mut cfg.minimize_to_tray, "");
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Run at Windows startup:").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                    let was = cfg.run_at_startup;
                    ui.checkbox(&mut cfg.run_at_startup, "");
                    if cfg.run_at_startup != was {
                        let _ = config::set_startup(cfg.run_at_startup);
                    }
                });
            });

            ui.add_space(12.0);

            self.settings_section(ui, "API & Browser Extension", |ui, cfg| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("API port:").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                    ui.add(egui::DragValue::new(&mut cfg.api_port).speed(1.0));
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Token file:").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                    ui.label(
                        RichText::new(crate::api::token_file_path())
                            .color(C_TEXT_DIM)
                            .font(FontId::proportional(11.0)),
                    );
                });
                ui.add_space(10.0);

                // Browser extension status
                ui.label(
                    RichText::new("Browser Extension Status")
                        .color(C_TEXT)
                        .font(FontId::proportional(13.0))
                        .strong(),
                );
                ui.add_space(6.0);

                let statuses = browser::check_all_extensions(browser::EXTENSION_ID);
                if statuses.is_empty() {
                    ui.label(RichText::new("No supported browsers detected.").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                } else {
                    for st in &statuses {
                        ui.horizontal(|ui| {
                            let (icon, col) = if st.is_installed() {
                                ("✓", C_SUCCESS)
                            } else {
                                ("✗", C_DANGER)
                            };
                            ui.label(RichText::new(icon).color(col).font(FontId::proportional(13.0)));
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(st.browser.name)
                                    .color(C_TEXT)
                                    .font(FontId::proportional(12.0)),
                            );
                            if !st.is_installed() {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("Repair").clicked() {
                                        browser::reset_integration_sentinel();
                                        let exe_dir = std::env::current_exe()
                                            .unwrap_or_default()
                                            .parent()
                                            .unwrap_or(std::path::Path::new("."))
                                            .to_path_buf();
                                        let _ = browser::run_integration(&exe_dir);
                                        browser::mark_integration_done();
                                    }
                                });
                            }
                        });
                        ui.add_space(2.0);
                    }
                }
            });

            ui.add_space(20.0);

            // Save button
            if ui.add(
                egui::Button::new(RichText::new("Save Settings").color(Color32::WHITE))
                    .fill(C_ACCENT)
                    .rounding(6.0)
                    .min_size(Vec2::new(140.0, 32.0)),
            ).clicked() {
                let _ = config::save(&self.cfg);
                // Propagate new config to queue manager
                {
                    let q   = self.queue.lock().unwrap();
                    let mut qcfg = q.cfg.lock().unwrap();
                    *qcfg = self.cfg.clone();
                }
                self.status_bar_msg = Some("Settings saved.".to_string());
            }
        });
    }

    fn settings_section<F>(&mut self, ui: &mut egui::Ui, title: &str, mut body: F)
    where
        F: FnMut(&mut egui::Ui, &mut Config),
    {
        egui::Frame::none()
            .fill(C_CARD)
            .rounding(8.0)
            .stroke(Stroke::new(1.0, C_BORDER))
            .inner_margin(egui::Margin::symmetric(16.0, 12.0))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(title)
                        .color(C_ACCENT)
                        .font(FontId::proportional(13.0))
                        .strong(),
                );
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                body(ui, &mut self.cfg);
            });
    }

    // ── Add download dialog ───────────────────────────────────────────────────

    fn draw_add_dialog(&mut self, ctx: &egui::Context) {
        if !self.add_dialog.open { return; }

        egui::Window::new("Add Download")
            .collapsible(false)
            .resizable(false)
            .min_width(480.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(8.0);

                ui.label(RichText::new("URL").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                let url_edit = egui::TextEdit::singleline(&mut self.add_dialog.url)
                    .desired_width(440.0)
                    .hint_text("https://…");
                ui.add(url_edit);

                ui.add_space(8.0);

                ui.label(RichText::new("Filename (optional)").color(C_TEXT_DIM).font(FontId::proportional(12.0)));
                let fn_edit = egui::TextEdit::singleline(&mut self.add_dialog.filename)
                    .desired_width(440.0)
                    .hint_text("Leave blank to auto-detect");
                ui.add(fn_edit);

                if let Some(ref err) = self.add_dialog.error.clone() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(err).color(C_DANGER).font(FontId::proportional(11.0)));
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let ok_btn = egui::Button::new(
                        RichText::new("Download").color(Color32::WHITE).font(FontId::proportional(13.0))
                    )
                    .fill(C_ACCENT)
                    .rounding(6.0)
                    .min_size(Vec2::new(100.0, 30.0));

                    if ui.add(ok_btn).clicked() {
                        let url = self.add_dialog.url.trim().to_string();
                        if url.is_empty() {
                            self.add_dialog.error = Some("URL cannot be empty.".to_string());
                        } else {
                            let fname = if self.add_dialog.filename.trim().is_empty() {
                                None
                            } else {
                                Some(self.add_dialog.filename.trim().to_string())
                            };
                            self.queue.lock().unwrap().add(url, fname, None);
                            self.add_dialog = AddDialog::default();
                            self.page = Page::Queue;
                        }
                    }

                    ui.add_space(8.0);

                    if ui.button("Cancel").clicked() {
                        self.add_dialog = AddDialog::default();
                    }
                });
                ui.add_space(4.0);
            });
    }

    // ── Status bar ────────────────────────────────────────────────────────────

    fn draw_statusbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let q       = self.queue.lock().unwrap();
            let items   = q.items.lock().unwrap();
            let active  = items.iter().filter(|i| i.status.is_active()).count();
            let queued  = items.iter().filter(|i| i.status == DownloadStatus::Queued).count();
            drop(items);
            drop(q);

            ui.label(
                RichText::new(format!("Active: {}  Queued: {}", active, queued))
                    .color(C_TEXT_DIM)
                    .font(FontId::proportional(11.0)),
            );

            if let Some(ref msg) = self.status_bar_msg.clone() {
                ui.add_space(16.0);
                ui.label(RichText::new(msg).color(C_SUCCESS).font(FontId::proportional(11.0)));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("YDM v1.0.0")
                        .color(C_TEXT_DIM)
                        .font(FontId::proportional(10.0)),
                );
            });
        });
    }
}

// ─── eframe app impl ─────────────────────────────────────────────────────────

impl eframe::App for YdmApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process download events every frame
        self.drain_events();

        // Request repaint every 500 ms so speed/ETA stay live
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

        // Top bar
        egui::TopBottomPanel::top("topbar")
            .exact_height(44.0)
            .frame(egui::Frame::none().fill(C_SURFACE).stroke(Stroke::new(1.0, C_BORDER)))
            .show(ctx, |ui| {
                ui.centered_and_justified(|ui| {
                    self.draw_topbar(ui);
                });
            });

        // Status bar
        egui::TopBottomPanel::bottom("statusbar")
            .exact_height(24.0)
            .frame(egui::Frame::none().fill(C_SURFACE).stroke(Stroke::new(1.0, C_BORDER)))
            .show(ctx, |ui| {
                ui.add_space(4.0);
                self.draw_statusbar(ui);
            });

        // Main content
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(C_BG).inner_margin(egui::Margin::same(12.0)))
            .show(ctx, |ui| {
                match self.page {
                    Page::Queue     => self.draw_queue_page(ui),
                    Page::Completed => self.draw_completed_page(ui),
                    Page::History   => self.draw_history_page(ui),
                    Page::Settings  => self.draw_settings_page(ui),
                }
            });

        // Add dialog overlay
        self.draw_add_dialog(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.queue.lock().unwrap().cancel_all();
        let _ = config::save(&self.cfg);
        let q = self.queue.lock().unwrap();
        let h = q.history.lock().unwrap();
        let _ = config::save_history(&h);
    }
}

// ─── Fonts & style setup ──────────────────────────────────────────────────────

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // egui ships with a built-in proportional font; nothing extra needed.
    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals.dark_mode            = true;
    style.visuals.window_fill          = C_SURFACE;
    style.visuals.panel_fill           = C_BG;
    style.visuals.faint_bg_color       = C_CARD;
    style.visuals.extreme_bg_color     = C_BG;
    style.visuals.code_bg_color        = C_CARD;
    style.visuals.override_text_color  = Some(C_TEXT);
    style.visuals.selection.bg_fill    = C_ACCENT.linear_multiply(0.4);
    style.visuals.widgets.noninteractive.bg_fill  = C_CARD;
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, C_TEXT_DIM);
    style.visuals.widgets.inactive.bg_fill         = C_CARD;
    style.visuals.widgets.inactive.fg_stroke       = Stroke::new(1.0, C_TEXT_DIM);
    style.visuals.widgets.hovered.bg_fill          = C_ACCENT.linear_multiply(0.15);
    style.visuals.widgets.hovered.fg_stroke        = Stroke::new(1.0, C_ACCENT);
    style.visuals.widgets.active.bg_fill           = C_ACCENT.linear_multiply(0.25);
    style.visuals.widgets.active.fg_stroke         = Stroke::new(1.0, C_ACCENT);
    style.spacing.item_spacing                     = Vec2::new(8.0, 6.0);
    style.spacing.button_padding                   = Vec2::new(10.0, 5.0);
    ctx.set_style(style);
}
