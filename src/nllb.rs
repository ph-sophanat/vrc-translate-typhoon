//! Local NLLB-200 translation backend — a free, fully-offline alternative to
//! DeepL/Claude. Translation runs inside the same Python service as the Typhoon
//! ASR (service/server.py exposes `/translate`), so there's no extra process and
//! no API key. One request returns both target languages. Quality is more literal
//! than Claude/DeepL — useful as a zero-cost A/B baseline.

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use std::time::Duration;

pub struct Nllb {
    client: Client,
    url: String,
    model: String,
}

impl Nllb {
    /// `url` is the Typhoon service base (it hosts `/translate` too); `model` is
    /// the HuggingFace id (e.g. "facebook/nllb-200-distilled-600M").
    pub fn new(url: &str, model: &str) -> Nllb {
        let client = Client::builder()
            // The first call may download a ~2.4GB model; warm calls are ~1-3s.
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| Client::new());
        let n = Nllb {
            client,
            url: url.trim_end_matches('/').to_string(),
            model: model.to_string(),
        };
        n.warm();
        n
    }

    /// The configured NLLB model id.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Trigger model load in the background so the first real utterance isn't
    /// stuck waiting on the one-time download/load.
    fn warm(&self) {
        let client = self.client.clone();
        let url = self.url.clone();
        let model = self.model.clone();
        std::thread::spawn(move || {
            eprintln!("[nllb] warming {model} (first run downloads ~2.4GB)…");
            let body = serde_json::json!({
                "text": "สวัสดี", "source": "th", "targets": ["EN"], "model": model,
            });
            match client.post(format!("{url}/translate")).json(&body).send() {
                Ok(r) if r.status().is_success() => eprintln!("[nllb] model ready"),
                Ok(r) => eprintln!("[nllb] warm failed: HTTP {}", r.status()),
                Err(e) => eprintln!("[nllb] warm failed: {e}"),
            }
        });
    }

    /// Translate `text` into both targets in one request, returning
    /// (primary, secondary). NLLB is sentence-level, so `context` isn't used.
    pub fn translate_pair(
        &self,
        text: &str,
        source: &str,
        primary: &str,
        secondary: &str,
    ) -> Result<(String, String)> {
        let body = serde_json::json!({
            "text": text,
            "source": source,
            "targets": [primary, secondary],
            "model": self.model,
        });
        let resp = self
            .client
            .post(format!("{}/translate", self.url))
            .json(&body)
            .send()
            .context("sending NLLB request")?;
        let status = resp.status();
        let raw = resp.text().context("reading NLLB response")?;
        if !status.is_success() {
            anyhow::bail!("NLLB service {}: {}", status, raw);
        }
        let v: serde_json::Value = serde_json::from_str(&raw).context("parsing NLLB response")?;
        let arr = v["translations"].as_array().cloned().unwrap_or_default();
        let get = |i: usize| {
            arr.get(i)
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };
        Ok((get(0), get(1)))
    }
}
