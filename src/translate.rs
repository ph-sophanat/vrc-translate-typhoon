use anyhow::{Context, Result};
use reqwest::blocking::Client;
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

#[derive(Deserialize)]
struct DeeplResponse {
    translations: Vec<DeeplTranslation>,
}

#[derive(Deserialize)]
struct DeeplTranslation {
    text: String,
}

pub struct DeepL {
    client: Client,
    key: Arc<String>,
    base: &'static str,
}

impl DeepL {
    pub fn new(key: &str) -> DeepL {
        // Free-tier keys end in ":fx" and use the api-free host.
        let base = if key.trim_end().ends_with(":fx") {
            "https://api-free.deepl.com"
        } else {
            "https://api.deepl.com"
        };
        // Persistent pool so we reuse warm TCP+TLS connections instead of paying
        // the ~650ms Asia->EU handshake on every utterance. tcp_keepalive stops
        // the OS dropping idle sockets; the heartbeat below stops DeepL's server
        // from closing them (it drops idle HTTP connections in ~10-30s).
        let client = Client::builder()
            .pool_idle_timeout(Duration::from_secs(120))
            .pool_max_idle_per_host(4)
            .tcp_keepalive(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| Client::new());

        let deepl = DeepL {
            client,
            key: Arc::new(key.to_string()),
            base,
        };
        deepl.start_keepalive();
        deepl
    }

    /// Translate `text` from `source` (e.g. "TH") to `target` (e.g. "JA", "EN-US").
    ///
    /// `context` is optional surrounding text in the SOURCE language (e.g. the
    /// previous utterances). DeepL doesn't translate it, but uses it to
    /// disambiguate pronouns/idioms/topic — Thai drops subjects constantly, so
    /// per-sentence translation without context misreads who/what is meant.
    pub fn translate(
        &self,
        text: &str,
        source: &str,
        target: &str,
        context: Option<&str>,
    ) -> Result<String> {
        let url = format!("{}/v2/translate", self.base);
        let mut body = serde_json::json!({
            "text": [text],
            "source_lang": source,
            "target_lang": target,
        });
        if let Some(ctx) = context.filter(|c| !c.trim().is_empty()) {
            body["context"] = serde_json::Value::String(ctx.to_string());
        }
        // Casual register for VRChat chat. DeepL only supports `formality` for a
        // subset of targets (JA yes, EN no) — sending it for EN returns a 400.
        if formality_supported(target) {
            body["formality"] = serde_json::Value::String("prefer_less".into());
        }

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, format!("DeepL-Auth-Key {}", self.key))
            .json(&body)
            .send()
            .context("sending DeepL request")?
            .error_for_status()
            .context("DeepL returned an error status")?;

        let body: DeeplResponse = resp.json().context("parsing DeepL response")?;
        let translated = body
            .translations
            .into_iter()
            .next()
            .map(|t| t.text)
            .unwrap_or_default();
        Ok(translated)
    }

    /// Cheap GET that exercises a pooled connection without spending translation
    /// quota (/v2/usage is free). Used to keep connections warm.
    fn ping(client: &Client, base: &str, key: &str) {
        let _ = client
            .get(format!("{base}/v2/usage"))
            .header(AUTHORIZATION, format!("DeepL-Auth-Key {key}"))
            .send();
    }

    /// Background thread that keeps two connections warm: pings immediately
    /// (pre-warm before the first utterance), then every 8s — well under the
    /// server's ~10-30s idle-close window. Two parallel pings so BOTH pooled
    /// connections (used by the parallel JA+EN translate) stay hot.
    fn start_keepalive(&self) {
        let client = self.client.clone();
        let key = self.key.clone();
        let base = self.base;
        std::thread::spawn(move || loop {
            let c2 = client.clone();
            let k2 = key.clone();
            let h = std::thread::spawn(move || Self::ping(&c2, base, &k2));
            Self::ping(&client, base, &key);
            let _ = h.join();
            std::thread::sleep(Duration::from_secs(8));
        });
    }
}

/// DeepL only accepts the `formality` parameter for these target languages;
/// sending it for others (e.g. EN) returns a 400. Match on the base code so
/// regional variants like "PT-BR" / "EN-US" resolve correctly.
fn formality_supported(target: &str) -> bool {
    let t = target.to_uppercase();
    let base = t.split('-').next().unwrap_or(&t);
    matches!(base, "DE" | "ES" | "FR" | "IT" | "JA" | "NL" | "PL" | "PT" | "RU")
}
