//! The audio → STT → translate → OSC pipeline, running on a background thread
//! and publishing results into `Shared` for the UI. This is the same flow that
//! used to live in `main`, plus health reporting and a mic-level meter feed.

use crate::audio;
use crate::config::Config;
use crate::mt::Translator;
use crate::osc::Vrc;
use crate::state::{Health, Shared};
use crate::typhoon::Typhoon;
use crate::vad::Segmenter;
use anyhow::Result;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How many recent Thai utterances to feed DeepL as `context`.
const CONTEXT_UTTERANCES: usize = 3;

/// Run the pipeline forever (until the audio stream ends / process exits),
/// reporting status into `shared`. Component setup failures are surfaced in the
/// status line rather than panicking.
pub fn run(cfg: Config, shared: Arc<Shared>) {
    shared.update(|s| {
        s.status = "connecting to Typhoon service…".into();
        s.stt = Health::Connecting;
    });

    let stt = match connect_stt(&cfg.typhoon_url, &shared) {
        Some(t) => {
            shared.update(|s| s.stt = Health::Ok);
            t
        }
        None => {
            shared.update(|s| {
                s.stt = Health::Down;
                s.status = "Typhoon service unreachable".into();
            });
            return;
        }
    };

    let mt = match Translator::from_config(&cfg) {
        Ok(t) => {
            eprintln!("Translator: {}", t.name());
            shared.update(|s| {
                s.engine = t.name().to_string();
                s.model = t.model().unwrap_or("").to_string();
            });
            t
        }
        Err(e) => {
            shared.update(|s| {
                s.tr = Health::Down;
                s.status = "translator config error".into();
            });
            shared.log(format!("translator: {e}"));
            eprintln!("translator init error: {e}");
            return;
        }
    };

    let vrc = match Vrc::new(&cfg.osc_addr) {
        Ok(v) => {
            shared.update(|s| s.osc = Health::Ok);
            v
        }
        Err(e) => {
            shared.update(|s| {
                s.osc = Health::Down;
                s.status = "OSC socket error".into();
            });
            shared.log(format!("OSC: {e}"));
            return;
        }
    };

    let src = cfg.deepl_source();

    let (stream, rx, device_rate) = match audio::start_capture() {
        Ok(t) => {
            shared.update(|s| s.mic = Health::Ok);
            t
        }
        Err(e) => {
            shared.update(|s| {
                s.mic = Health::Down;
                s.status = "no microphone".into();
            });
            shared.log(format!("mic: {e}"));
            return;
        }
    };

    let mut resampler = match audio::StreamResampler::new(device_rate) {
        Ok(r) => r,
        Err(e) => {
            shared.update(|s| s.status = format!("resampler error: {e}"));
            return;
        }
    };
    let mut segmenter = match Segmenter::new() {
        Ok(s) => s,
        Err(e) => {
            shared.update(|s| s.status = format!("VAD error: {e}"));
            return;
        }
    };

    shared.update(|s| {
        s.status = "listening — speak Thai".into();
        s.tr = Health::Ok;
    });
    eprintln!("Listening. Speak {}, see {}/{} in VRChat.", src, cfg.target_primary, cfg.target_secondary);

    // Rolling window of recent Thai utterances, passed to DeepL as context.
    let mut history: VecDeque<String> = VecDeque::new();

    for batch in rx {
        // Feed the level meter even while muted, so the UI still reacts to voice.
        if !batch.is_empty() {
            let sum: f32 = batch.iter().map(|x| x * x).sum();
            shared.set_mic_level((sum / batch.len() as f32).sqrt());
        }
        if shared.muted.load(Ordering::Relaxed) {
            continue;
        }

        let s16 = resampler.push(&batch);
        if s16.is_empty() {
            continue;
        }
        for utterance in segmenter.push(&s16) {
            handle(&utterance, &stt, &mt, &vrc, &cfg, &src, &shared, &mut history);
        }
    }

    drop(stream);
}

/// Wait for the Typhoon service to come up and finish loading its model.
///
/// On a cold start we auto-launch the service, so the worker races the ~10-20s
/// model load. `/health` returns 503 (or the connection is refused) until the
/// model is ready, both of which fail `Typhoon::new`; we retry on a ~90s budget
/// rather than giving up on the first probe.
fn connect_stt(url: &str, shared: &Arc<Shared>) -> Option<Typhoon> {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut announced = false;
    loop {
        match Typhoon::new(url) {
            Ok(t) => return Some(t),
            Err(e) if Instant::now() < deadline => {
                if !announced {
                    shared.update(|s| s.status = "loading Typhoon model…".into());
                    announced = true;
                }
                std::thread::sleep(Duration::from_secs(1));
                let _ = &e; // retrying; detail only matters if we run out of time
            }
            Err(e) => {
                shared.log(format!("STT: {e}"));
                eprintln!("STT init error: {e}");
                return None;
            }
        }
    }
}

/// Headless entry point: run the pipeline with throwaway shared state, keeping
/// the original console-only behavior (`--headless`).
pub fn run_headless(cfg: Config) -> Result<()> {
    run(cfg, Arc::new(Shared::new()));
    Ok(())
}

fn handle(
    samples: &[f32],
    stt: &Typhoon,
    mt: &Translator,
    vrc: &Vrc,
    cfg: &Config,
    src: &str,
    shared: &Arc<Shared>,
    history: &mut VecDeque<String>,
) {
    let secs = samples.len() as f32 / 16_000.0;
    let t_total = Instant::now();

    let _ = vrc.typing(true);

    let t_stt = Instant::now();
    let thai = match stt.transcribe(samples) {
        Ok(t) => {
            shared.update(|s| s.stt = Health::Ok);
            t
        }
        Err(e) => {
            shared.update(|s| s.stt = Health::Warn);
            shared.log(format!("transcribe error: {e}"));
            eprintln!("transcribe error: {e}");
            let _ = vrc.typing(false);
            return;
        }
    };
    let stt_ms = t_stt.elapsed().as_millis() as u32;

    if thai.is_empty() {
        let _ = vrc.typing(false);
        return;
    }
    eprintln!("[{:.1}s] {}: {}", secs, src, thai);

    // Thai-only mode: skip translation, send the recognized Thai to the chatbox.
    if !shared.translate_enabled() {
        let osc_ok = vrc.chatbox(&thai).is_ok();
        let _ = vrc.typing(false);
        let total = t_total.elapsed().as_millis() as u32;
        shared.update(|s| {
            s.osc = if osc_ok { Health::Ok } else { Health::Warn };
            s.thai = thai.clone();
            s.primary = thai.clone(); // shown prominently in the result card
            s.secondary = String::new();
            s.stt_ms = stt_ms;
            s.tr_ms = 0;
            s.total_ms = total;
            s.utterances += 1;
        });
        let idx = shared.snapshot().utterances;
        eprintln!("  (no-translate) sent Thai → {total}ms");
        append_log(cfg, &thai, &thai, "", secs, stt_ms, 0, idx);
        history.push_back(thai);
        while history.len() > CONTEXT_UTTERANCES {
            history.pop_front();
        }
        return;
    }

    // Recent utterances (in Thai) as DeepL context — disambiguates dropped
    // subjects and idioms. Built before we push the current line.
    let context: Option<String> = if history.is_empty() {
        None
    } else {
        Some(history.iter().cloned().collect::<Vec<_>>().join(" "))
    };
    let ctx = context.as_deref();

    let t_tr = Instant::now();
    let (primary, secondary) =
        mt.translate_pair(&thai, src, &cfg.target_primary, &cfg.target_secondary, ctx);
    let tr_ms = t_tr.elapsed().as_millis() as u32;
    let translated_ok = !primary.is_empty() || !secondary.is_empty();
    shared.update(|s| s.tr = if translated_ok { Health::Ok } else { Health::Warn });

    let chatbox = format!("{}\n{}", primary, secondary);
    let osc_ok = vrc.chatbox(&chatbox).is_ok();
    shared.update(|s| s.osc = if osc_ok { Health::Ok } else { Health::Warn });
    let _ = vrc.typing(false);

    let total = t_total.elapsed().as_millis() as u32;
    shared.update(|s| {
        s.thai = thai.clone();
        s.primary = primary.clone();
        s.secondary = secondary.clone();
        s.stt_ms = stt_ms;
        s.tr_ms = tr_ms;
        s.total_ms = total;
        s.utterances += 1;
    });
    let idx = shared.snapshot().utterances;

    // Console: full side-by-side so quality can be eyeballed in a terminal.
    eprintln!("       {}: {}", cfg.target_primary, primary);
    eprintln!("       {}: {}", cfg.target_secondary, secondary);
    eprintln!(
        "  \u{23f1} audio {:.1}s | transcribe {}ms | translate(2x\u{2225}) {}ms | total {}ms",
        secs, stt_ms, tr_ms, total
    );

    // Persistent log for offline review (which layer is wrong: ASR vs MT?).
    append_log(cfg, &thai, &primary, &secondary, secs, stt_ms, tr_ms, idx);

    // Add this utterance to the rolling context window for the next translation.
    history.push_back(thai);
    while history.len() > CONTEXT_UTTERANCES {
        history.pop_front();
    }
}

/// Append one utterance as a readable TH/primary/secondary block to
/// `translation_log.txt` (in the working dir). Lets us review transcription vs
/// translation quality side by side after a live session.
fn append_log(
    cfg: &Config,
    thai: &str,
    primary: &str,
    secondary: &str,
    secs: f32,
    stt_ms: u32,
    tr_ms: u32,
    idx: u32,
) {
    use std::io::Write;
    let block = format!(
        "#{idx}  audio {secs:.1}s | stt {stt_ms}ms | tr {tr_ms}ms\nTH: {thai}\n{}: {primary}\n{}: {secondary}\n\n",
        cfg.target_primary, cfg.target_secondary,
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("translation_log.txt")
    {
        let _ = f.write_all(block.as_bytes());
    }
}
