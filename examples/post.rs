// Exercises the real Typhoon client request path (pcm_to_wav + reqwest) without
// a mic, to reproduce/diagnose the "service returned an error" failure.
// Run: cargo run --release --example post
#[path = "../src/typhoon.rs"]
mod typhoon;
use typhoon::Typhoon;

fn main() -> anyhow::Result<()> {
    let t = Typhoon::new("http://127.0.0.1:8765")?;
    // 2s of 220 Hz tone (non-silent, so the request body is realistic)
    let n = 32_000usize;
    let samples: Vec<f32> = (0..n)
        .map(|i| 0.2 * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / 16_000.0).sin())
        .collect();
    match t.transcribe(&samples) {
        Ok(text) => println!("OK: {text:?}"),
        Err(e) => println!("ERR: {e:#}"),
    }
    Ok(())
}
