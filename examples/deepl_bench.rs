// Measures where DeepL latency goes: cold (first) request vs warm (reused
// connection) vs after an idle gap (does the pooled connection go cold between
// utterances?). Run: cargo run --release --example deepl_bench
#[path = "../src/config.rs"]
mod config;
#[path = "../src/translate.rs"]
mod translate;

use config::Config;
use std::time::{Duration, Instant};
use translate::DeepL;

fn time_one(d: &DeepL, src: &str, text: &str, label: &str) {
    let t = Instant::now();
    match d.translate(text, src, "JA", None) {
        Ok(_) => println!("{label:<22} {} ms", t.elapsed().as_millis()),
        Err(e) => println!("{label:<22} ERR {e}"),
    }
}

fn main() -> anyhow::Result<()> {
    let cfg = Config::load("config.toml")?;
    let d = DeepL::new(&cfg.deepl_key);
    let src = cfg.deepl_source();
    let text = "สวัสดีครับ วันนี้อากาศดีมากเลย";

    time_one(&d, &src, text, "cold (1st)");
    time_one(&d, &src, text, "warm (immediate)");
    time_one(&d, &src, text, "warm (immediate)");

    for gap in [3u64, 10, 30] {
        std::thread::sleep(Duration::from_secs(gap));
        time_one(&d, &src, text, &format!("after {gap}s idle"));
    }

    // the app's real pattern: JA + EN in parallel
    let t = Instant::now();
    std::thread::scope(|s| {
        let h = s.spawn(|| d.translate(text, &src, "JA", None));
        let _ = d.translate(text, &src, "EN-US", None);
        let _ = h.join();
    });
    println!("{:<22} {} ms", "parallel JA+EN", t.elapsed().as_millis());
    Ok(())
}
