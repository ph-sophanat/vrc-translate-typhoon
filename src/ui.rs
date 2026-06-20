//! Native status window (egui/eframe) + system-tray icon.
//!
//! eframe owns the main thread; the audio pipeline runs on the worker thread and
//! publishes into `Shared`, which we read once per frame here. Closing the window
//! hides it to the tray; "Quit" (button or tray menu) actually exits, which drops
//! the `Service` guard back in `main` and stops the Python service.

use crate::service::ServiceCtl;
use crate::state::{Health, ServerState, Shared};
use anyhow::Result;
use eframe::egui::{self, Color32, RichText};
use std::sync::Arc;
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

// Anthropic-ish minimal palette.
const IVORY: Color32 = Color32::from_rgb(0xFA, 0xF9, 0xF5);
const IVORY2: Color32 = Color32::from_rgb(0xF0, 0xEE, 0xE6);
const CHAR: Color32 = Color32::from_rgb(0x19, 0x19, 0x19);
const CLAY: Color32 = Color32::from_rgb(0xCC, 0x78, 0x5C);
const GREY: Color32 = Color32::from_rgb(0x73, 0x72, 0x6C);
const OKG: Color32 = Color32::from_rgb(0x5C, 0x8A, 0x5C);
const RED: Color32 = Color32::from_rgb(0xB4, 0x45, 0x3A);
const TAN: Color32 = Color32::from_rgb(0xC9, 0x96, 0x6C);
const HAIR: Color32 = Color32::from_rgb(0xE5, 0xE2, 0xDA);

pub fn run(shared: Arc<Shared>, ctl: Arc<ServiceCtl>) -> Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([440.0, 600.0])
            .with_min_inner_size([380.0, 480.0])
            .with_title("vrc-translate"),
        ..Default::default()
    };
    eframe::run_native(
        "vrc-translate",
        opts,
        Box::new(|cc| Ok(Box::new(App::new(cc, shared, ctl)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe failed: {e}"))
}

struct App {
    shared: Arc<Shared>,
    ctl: Arc<ServiceCtl>,
    _tray: Option<TrayIcon>,
    show_id: Option<MenuId>,
    mute_id: Option<MenuId>,
    quit_id: Option<MenuId>,
    visible: bool,
    quitting: bool,
    meter: f32, // smoothed mic level
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, shared: Arc<Shared>, ctl: Arc<ServiceCtl>) -> App {
        install_fonts(&cc.egui_ctx);

        let mut v = egui::Visuals::light();
        v.panel_fill = IVORY;
        v.window_fill = IVORY;
        v.override_text_color = Some(CHAR);
        cc.egui_ctx.set_visuals(v);

        let (tray, show_id, mute_id, quit_id) = match build_tray() {
            Ok((t, a, b, c)) => (Some(t), Some(a), Some(b), Some(c)),
            Err(e) => {
                eprintln!("[tray] disabled: {e}");
                (None, None, None, None)
            }
        };

        App {
            shared,
            ctl,
            _tray: tray,
            show_id,
            mute_id,
            quit_id,
            visible: true,
            quitting: false,
            meter: 0.0,
        }
    }

    fn toggle_visible(&mut self, ctx: &egui::Context) {
        self.visible = !self.visible;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(self.visible));
        if self.visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Tray menu clicks.
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if self.show_id.as_ref() == Some(&ev.id) {
                self.toggle_visible(ctx);
            } else if self.mute_id.as_ref() == Some(&ev.id) {
                self.shared.toggle_mute();
            } else if self.quit_id.as_ref() == Some(&ev.id) {
                self.quit(ctx);
            }
        }
        // Drain tray pointer events (we don't act on them, but keep the queue empty).
        while TrayIconEvent::receiver().try_recv().is_ok() {}

        // Close button → hide to tray instead of quitting.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.visible = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        let snap = self.shared.snapshot();
        let muted = self.shared.is_muted();

        // Smooth the meter so it doesn't jitter; scale RMS into a 0..1 bar.
        let level = (self.shared.mic_level() * 4.0).clamp(0.0, 1.0);
        self.meter = self.meter * 0.6 + level * 0.4;

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("vrc-translate").size(22.0).color(CHAR).strong());
                ui.add_space(6.0);
                ui.label(RichText::new("Typhoon ASR").size(12.0).color(CLAY));
            });
            ui.label(RichText::new(&snap.status).size(12.0).color(GREY));
            hairline(ui);
            ui.add_space(6.0);

            // Health dots — the translate dot is labeled with the active backend.
            let mt_label = if snap.engine.is_empty() { "Translate" } else { snap.engine.as_str() };
            ui.horizontal_wrapped(|ui| {
                dot(ui, "Mic", snap.mic);
                ui.add_space(12.0);
                dot(ui, "STT", snap.stt);
                ui.add_space(12.0);
                dot(ui, mt_label, snap.tr);
                ui.add_space(12.0);
                dot(ui, "VRChat", snap.osc);
            });
            ui.add_space(4.0);

            // Pipeline / model line.
            let mt_detail = if !self.shared.translate_enabled() {
                "Thai only (no translation)".to_string()
            } else if snap.engine.is_empty() {
                "starting…".to_string()
            } else if snap.model.is_empty() {
                snap.engine.clone()
            } else {
                format!("{} · {}", snap.engine, snap.model)
            };
            ui.label(
                RichText::new(format!("Typhoon ASR  →  {mt_detail}"))
                    .size(11.0)
                    .color(GREY),
            );
            ui.add_space(8.0);

            // Server status + manual Start/Kill. The status comes from the
            // background monitor; the buttons run off-thread so the UI never
            // blocks on the (slow) spawn/kill calls.
            let (stext, scolor) = match snap.server {
                ServerState::Running => ("Server running", OKG),
                ServerState::Loading => ("Server loading…", TAN),
                ServerState::Stopped => ("Server stopped", RED),
                ServerState::Foreign => ("Other service on port", RED),
                ServerState::Unknown => ("Server …", GREY),
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(scolor));
                ui.label(RichText::new(stext).size(12.0).color(GREY));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let up = matches!(snap.server, ServerState::Running | ServerState::Loading);
                if ui
                    .add_enabled(!up, egui::Button::new("Start server").rounding(6.0))
                    .clicked()
                {
                    let ctl = self.ctl.clone();
                    std::thread::spawn(move || ctl.start());
                }
                if ui
                    .add_enabled(up, egui::Button::new("Kill server").rounding(6.0))
                    .clicked()
                {
                    let ctl = self.ctl.clone();
                    std::thread::spawn(move || ctl.kill());
                }
            });
            ui.add_space(10.0);

            // Mic level meter.
            ui.label(RichText::new("INPUT LEVEL").size(10.0).color(GREY));
            let bar = egui::ProgressBar::new(self.meter)
                .desired_height(8.0)
                .fill(if muted { GREY } else { CLAY });
            ui.add(bar);
            ui.add_space(10.0);

            // Mute toggle.
            let label = if muted { "MUTED  —  click to go live" } else { "LIVE  —  click to mute" };
            let btn = egui::Button::new(RichText::new(label).size(15.0).color(Color32::WHITE))
                .fill(if muted { RED } else { OKG })
                .min_size(egui::vec2(ui.available_width(), 38.0))
                .rounding(8.0);
            if ui.add(btn).clicked() {
                self.shared.toggle_mute();
            }
            ui.add_space(6.0);

            // Translate on/off (Thai-only mode).
            let translating = self.shared.translate_enabled();
            let tlabel = if translating {
                "Translating  →  tap for Thai-only"
            } else {
                "Thai only (ไม่แปล)  →  tap to translate"
            };
            let tbtn = egui::Button::new(
                RichText::new(tlabel)
                    .size(13.0)
                    .color(if translating { CHAR } else { Color32::WHITE }),
            )
            .fill(if translating { IVORY2 } else { CLAY })
            .stroke(egui::Stroke::new(1.0, HAIR))
            .min_size(egui::vec2(ui.available_width(), 30.0))
            .rounding(8.0);
            if ui.add(tbtn).clicked() {
                self.shared.toggle_translate();
            }
            ui.add_space(12.0);

            // Latest result card.
            card(ui, |ui| {
                if snap.thai.is_empty() {
                    ui.label(RichText::new("Waiting for speech…").italics().color(GREY));
                } else {
                    ui.label(RichText::new("TH").size(10.0).color(GREY));
                    ui.label(RichText::new(&snap.thai).size(16.0).color(CHAR));
                    ui.add_space(8.0);
                    ui.label(RichText::new(&snap.primary).size(18.0).color(CLAY).strong());
                    ui.label(RichText::new(&snap.secondary).size(15.0).color(CHAR));
                }
            });
            ui.add_space(6.0);

            // Latency line.
            if snap.total_ms > 0 {
                ui.label(
                    RichText::new(format!(
                        "transcribe {} ms · translate {} ms · total {} ms · {} utterances",
                        snap.stt_ms, snap.tr_ms, snap.total_ms, snap.utterances
                    ))
                    .size(11.0)
                    .color(GREY),
                );
            }

            // Recent log.
            ui.add_space(6.0);
            egui::CollapsingHeader::new(RichText::new("Activity").size(12.0).color(GREY))
                .default_open(false)
                .show(ui, |ui| {
                    for line in snap.log.iter().rev() {
                        ui.label(RichText::new(line).size(11.0).color(GREY));
                    }
                });

            // Footer.
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Quit").clicked() {
                        self.quit(ctx);
                    }
                    ui.label(
                        RichText::new("✕ closes to tray").size(10.0).color(GREY),
                    );
                });
                hairline(ui);
            });
        });

        // Keep the meter and transcript live without burning CPU.
        ctx.request_repaint_after(Duration::from_millis(120));
    }
}

fn dot(ui: &mut egui::Ui, label: &str, h: Health) {
    let color = match h {
        Health::Ok => OKG,
        Health::Connecting => TAN,
        Health::Warn => CLAY,
        Health::Down => RED,
    };
    ui.label(RichText::new("●").color(color));
    ui.label(RichText::new(label).size(12.0).color(GREY));
}

fn hairline(ui: &mut egui::Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, HAIR);
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(IVORY2)
        .stroke(egui::Stroke::new(1.0, HAIR))
        .rounding(6.0)
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// egui's bundled fonts cover neither Thai nor Japanese. Load Windows system
/// fonts as fallbacks (per-glyph) so transcripts and translations render.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut names: Vec<String> = Vec::new();

    // (key, candidate file paths) — first that loads wins for that role.
    let candidates: [(&str, &[&str]); 3] = [
        ("jp", &[r"C:\Windows\Fonts\YuGothR.ttc", r"C:\Windows\Fonts\meiryo.ttc", r"C:\Windows\Fonts\msgothic.ttc"]),
        ("thai", &[r"C:\Windows\Fonts\tahoma.ttf", r"C:\Windows\Fonts\LeelaUIb.ttf"]),
        ("latin", &[r"C:\Windows\Fonts\segoeui.ttf"]),
    ];

    for (key, paths) in candidates {
        for path in paths {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert(key.to_string(), egui::FontData::from_owned(bytes));
                names.push(key.to_string());
                break;
            }
        }
    }

    // Prepend our fonts (highest priority first: jp, thai, latin) so egui falls
    // back across them before its bundled Latin font.
    if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        for name in names.iter().rev() {
            fam.insert(0, name.clone());
        }
    }
    if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        for name in names.iter().rev() {
            fam.insert(0, name.clone());
        }
    }
    ctx.set_fonts(fonts);
}

fn build_tray() -> Result<(TrayIcon, MenuId, MenuId, MenuId)> {
    let menu = Menu::new();
    let show = MenuItem::new("Show / hide window", true, None);
    let mute = MenuItem::new("Toggle mute", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[&show, &mute, &PredefinedMenuItem::separator(), &quit])
        .map_err(|e| anyhow::anyhow!("tray menu: {e}"))?;

    let tray = TrayIconBuilder::new()
        .with_tooltip("vrc-translate — Thai → JA/EN")
        .with_menu(Box::new(menu))
        .with_icon(make_icon())
        .build()
        .map_err(|e| anyhow::anyhow!("tray icon: {e}"))?;

    Ok((tray, show.id().clone(), mute.id().clone(), quit.id().clone()))
}

/// A simple clay-colored filled circle, generated at runtime (no asset file).
fn make_icon() -> Icon {
    let size: u32 = 32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let c = size as f32 / 2.0;
    let r = c - 1.0;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - c;
            let dy = y as f32 + 0.5 - c;
            if dx * dx + dy * dy <= r * r {
                let i = ((y * size + x) * 4) as usize;
                rgba[i] = 0xCC;
                rgba[i + 1] = 0x78;
                rgba[i + 2] = 0x5C;
                rgba[i + 3] = 0xFF;
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("building tray icon")
}
