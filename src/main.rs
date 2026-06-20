// Hide the console window in release builds (GUI app); keep it in debug for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod claude;
mod config;
mod mt;
mod nllb;
mod osc;
mod service;
mod state;
mod translate;
mod typhoon;
mod ui;
mod vad;
mod worker;

use anyhow::Result;
use config::Config;
use state::Shared;
use std::sync::Arc;

fn main() -> Result<()> {
    let cfg = Config::load_default()?;

    // `--headless` keeps the original console-only pipeline (no window/tray).
    if std::env::args().any(|a| a == "--headless") {
        println!("vrc-translate (Typhoon ASR) — headless mode\n");
        return worker::run_headless(cfg);
    }

    let shared = Arc::new(Shared::new());

    // Auto-launch the Python Typhoon service. The controller owns the process and
    // lets the UI Start/Kill it; dropping it (on exit) stops Python.
    let port = parse_port(&cfg.typhoon_url);
    let ctl = service::ServiceCtl::new("cpu", port);

    // Background poller that publishes live server status into `shared` for the UI.
    {
        let shared = shared.clone();
        std::thread::spawn(move || service::monitor(shared, port));
    }

    // The pipeline runs off the main thread; eframe must own the main thread.
    {
        let shared = shared.clone();
        std::thread::spawn(move || worker::run(cfg, shared));
    }

    // Blocks until the user quits the window; then `ctl` drops → stops Python.
    ui::run(shared, ctl)
}

/// Extract the port from a `http://host:port` URL (defaults to 8765).
fn parse_port(url: &str) -> u16 {
    url.trim_end_matches('/')
        .rsplit(':')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8765)
}
