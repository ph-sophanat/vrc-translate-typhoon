use anyhow::{Context, Result};
use std::time::Duration;

/// Client for the local Typhoon ASR service (service/server.py).
/// Each VAD-segmented utterance is sent as a 16 kHz mono WAV; the service
/// returns the Thai transcript. Typhoon is a streaming transducer, so unlike
/// Whisper it doesn't hallucinate / loop on short clips — no dedup needed.
pub struct Typhoon {
    client: reqwest::blocking::Client,
    url: String,
}

impl Typhoon {
    pub fn new(url: &str) -> Result<Typhoon> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("building http client")?;
        let t = Typhoon {
            client,
            url: url.trim_end_matches('/').to_string(),
        };
        // Fail fast with a clear message if the Python service isn't up yet.
        let body = t
            .client
            .get(format!("{}/health", t.url))
            .timeout(Duration::from_secs(120)) // first call may still be warming the model
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| {
                format!(
                    "Typhoon service not reachable at {} — start it first:\n  \
                     cd service; .venv\\Scripts\\python server.py",
                    t.url
                )
            })?
            .text()
            .unwrap_or_default();

        // Make sure it's actually the Thai Typhoon service and not another
        // project's ASR service squatting on the same port (e.g. vrc-jp-scribe),
        // which would silently transcribe Thai speech with the wrong model.
        if !body.contains("typhoon") {
            anyhow::bail!(
                "the service on {} is NOT the Typhoon (Thai) ASR service — another ASR \
                 service may be running on this port (e.g. vrc-jp-scribe). Stop it, or set \
                 a different typhoon_url. (health said: {})",
                t.url,
                body.trim()
            );
        }
        Ok(t)
    }

    /// Transcribe 16 kHz mono f32 samples to Thai text.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let wav = pcm_to_wav(samples, 16_000);
        let resp = self
            .client
            .post(format!("{}/transcribe", self.url))
            .header("Content-Type", "audio/wav")
            .body(wav)
            .send()
            .context("posting audio to typhoon service")?;
        let status = resp.status();
        let body = resp.text().context("reading typhoon response")?;
        if !status.is_success() {
            anyhow::bail!("typhoon service {}: {}", status, body);
        }
        let v: serde_json::Value = serde_json::from_str(&body).context("parsing typhoon response")?;
        Ok(v["text"].as_str().unwrap_or("").trim().to_string())
    }
}

/// Encode 16 kHz mono f32 samples as a 16-bit PCM WAV (no external crate).
fn pcm_to_wav(samples: &[f32], sr: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut buf = Vec::with_capacity(44 + samples.len() * 2);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // format = PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // channels = mono
    buf.extend_from_slice(&sr.to_le_bytes());
    buf.extend_from_slice(&(sr * 2).to_le_bytes()); // byte rate
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}
