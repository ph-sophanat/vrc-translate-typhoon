//! Thread-shared application state: the worker thread writes, the UI reads.
//!
//! Hot, high-frequency values (mute flag, mic level) are lock-free atomics so
//! the audio callback path never blocks on the UI. The richer snapshot (last
//! transcript, translations, latencies, health) sits behind a short-lived Mutex
//! that's only touched once per utterance and once per UI frame.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

/// Component status, rendered as a colored dot in the UI.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Health {
    #[default]
    Connecting,
    Ok,
    Warn,
    Down,
}

/// Live status of the Typhoon Python service, polled by a background monitor and
/// shown (with Start/Kill controls) in the UI.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerState {
    /// Not yet probed.
    #[default]
    Unknown,
    /// Nothing is listening on the port.
    Stopped,
    /// Up, but still loading the model (health says not ready).
    Loading,
    /// Up and ready to transcribe.
    Running,
    /// Something else is on the port (not the Typhoon service).
    Foreign,
}

/// A copy of the displayable state, taken once per UI frame.
#[derive(Default, Clone)]
pub struct Snapshot {
    pub thai: String,
    pub primary: String,   // e.g. JA
    pub secondary: String, // e.g. EN
    pub stt_ms: u32,
    pub tr_ms: u32,
    pub total_ms: u32,
    pub mic: Health,
    pub stt: Health,
    pub tr: Health,
    pub osc: Health,
    /// Live status of the Python ASR service (see the monitor in `service.rs`).
    pub server: ServerState,
    pub status: String,
    pub utterances: u32,
    pub log: Vec<String>,
    /// Active translation backend, e.g. "DeepL" or "Claude".
    pub engine: String,
    /// Specific model id when applicable (Claude), else empty.
    pub model: String,
}

pub struct Shared {
    pub muted: AtomicBool,
    /// When false, skip translation and send the recognized Thai straight to the
    /// chatbox ("Thai-only" mode).
    translate: AtomicBool,
    mic_level: AtomicU32, // f32 bits — RMS of the latest audio batch (0..~0.3)
    inner: Mutex<Snapshot>,
}

impl Shared {
    pub fn new() -> Shared {
        Shared {
            muted: AtomicBool::new(false),
            translate: AtomicBool::new(true),
            mic_level: AtomicU32::new(0),
            inner: Mutex::new(Snapshot {
                status: "starting…".into(),
                ..Default::default()
            }),
        }
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Flip the mute flag; returns the new state.
    pub fn toggle_mute(&self) -> bool {
        let next = !self.is_muted();
        self.muted.store(next, Ordering::Relaxed);
        next
    }

    /// Whether translation is on (false = Thai-only mode).
    pub fn translate_enabled(&self) -> bool {
        self.translate.load(Ordering::Relaxed)
    }

    /// Flip translate on/off; returns the new state.
    pub fn toggle_translate(&self) -> bool {
        let next = !self.translate_enabled();
        self.translate.store(next, Ordering::Relaxed);
        next
    }

    pub fn set_mic_level(&self, v: f32) {
        self.mic_level.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn mic_level(&self) -> f32 {
        f32::from_bits(self.mic_level.load(Ordering::Relaxed))
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner.lock().unwrap().clone()
    }

    pub fn update(&self, f: impl FnOnce(&mut Snapshot)) {
        f(&mut self.inner.lock().unwrap());
    }

    /// Append a line to the rolling log (keeps the last 8).
    pub fn log(&self, line: impl Into<String>) {
        let mut g = self.inner.lock().unwrap();
        g.log.push(line.into());
        let n = g.log.len();
        if n > 8 {
            g.log.drain(0..n - 8);
        }
    }
}

impl Default for Shared {
    fn default() -> Self {
        Shared::new()
    }
}
