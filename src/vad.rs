use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use voice_activity_detector::VoiceActivityDetector;

const CHUNK: usize = 512; // Silero v5 chunk for 16 kHz = 32 ms
const SAMPLE_RATE: i64 = 16_000;
const SPEECH_THRESHOLD: f32 = 0.5;
const END_SILENCE_CHUNKS: usize = 12; // ~384 ms pause ends an utterance (snappier endpointing)
const MIN_SPEECH_CHUNKS: usize = 8; // ~250 ms, ignore coughs/clicks
const PREROLL_CHUNKS: usize = 6; // ~190 ms kept before speech onset
const MAX_UTTER_SAMPLES: usize = 20 * 16_000; // hard cap 20 s

/// Streaming voice-activity segmenter. Feed 16 kHz mono samples; get back
/// complete utterances whenever you pause.
pub struct Segmenter {
    vad: VoiceActivityDetector,
    pending: Vec<f32>,
    preroll: VecDeque<f32>,
    current: Vec<f32>,
    in_speech: bool,
    silence_chunks: usize,
    speech_chunks: usize,
}

impl Segmenter {
    pub fn new() -> Result<Segmenter> {
        let vad = VoiceActivityDetector::builder()
            .chunk_size(CHUNK)
            .sample_rate(SAMPLE_RATE)
            .build()
            .map_err(|e| anyhow!("building Silero VAD: {e}"))?;
        Ok(Segmenter {
            vad,
            pending: Vec::new(),
            preroll: VecDeque::new(),
            current: Vec::new(),
            in_speech: false,
            silence_chunks: 0,
            speech_chunks: 0,
        })
    }

    /// Returns any utterances that completed within this batch of samples.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        self.pending.extend_from_slice(samples);
        while self.pending.len() >= CHUNK {
            let chunk: Vec<f32> = self.pending.drain(..CHUNK).collect();
            if let Some(utt) = self.process_chunk(&chunk) {
                out.push(utt);
            }
        }
        out
    }

    fn process_chunk(&mut self, chunk: &[f32]) -> Option<Vec<f32>> {
        let prob = self.vad.predict(chunk.iter().copied());
        let speech = prob >= SPEECH_THRESHOLD;

        if !self.in_speech {
            // Keep a rolling pre-roll so we don't clip the first phoneme.
            for &s in chunk {
                self.preroll.push_back(s);
            }
            let cap = PREROLL_CHUNKS * CHUNK;
            while self.preroll.len() > cap {
                self.preroll.pop_front();
            }
            if speech {
                self.in_speech = true;
                self.current = self.preroll.drain(..).collect();
                self.current.extend_from_slice(chunk);
                self.speech_chunks = 1;
                self.silence_chunks = 0;
            }
            None
        } else {
            self.current.extend_from_slice(chunk);
            if speech {
                self.speech_chunks += 1;
                self.silence_chunks = 0;
            } else {
                self.silence_chunks += 1;
            }
            if self.silence_chunks >= END_SILENCE_CHUNKS
                || self.current.len() >= MAX_UTTER_SAMPLES
            {
                self.finalize()
            } else {
                None
            }
        }
    }

    fn finalize(&mut self) -> Option<Vec<f32>> {
        let enough = self.speech_chunks >= MIN_SPEECH_CHUNKS;
        let mut utt = std::mem::take(&mut self.current);
        // Trim most of the trailing silence, keeping ~3 chunks of decay.
        if self.silence_chunks > 3 {
            let trim = (self.silence_chunks - 3) * CHUNK;
            let keep = utt.len().saturating_sub(trim);
            utt.truncate(keep);
        }
        self.in_speech = false;
        self.silence_chunks = 0;
        self.speech_chunks = 0;
        self.preroll.clear();
        self.vad.reset();

        if enough {
            Some(utt)
        } else {
            None
        }
    }
}
