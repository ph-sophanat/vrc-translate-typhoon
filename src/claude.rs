//! Claude (Anthropic) translation backend — an alternative to DeepL that
//! understands casual/idiomatic Thai far better. A single Messages API call
//! returns BOTH target languages, with the recent conversation as context and a
//! system prompt that pins a casual VRChat register.
//!
//! Rust has no official Anthropic SDK, so this talks to `POST /v1/messages` over
//! raw HTTP (reqwest), per the Anthropic API guidance for unsupported languages.

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use std::time::Duration;

pub struct Claude {
    client: Client,
    key: String,
    model: String,
}

impl Claude {
    pub fn new(key: &str, model: &str) -> Claude {
        let client = Client::builder()
            .pool_idle_timeout(Duration::from_secs(120))
            .pool_max_idle_per_host(4)
            .tcp_keepalive(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| Client::new());
        Claude {
            client,
            key: key.to_string(),
            model: model.to_string(),
        }
    }

    /// The configured model id (e.g. "claude-haiku-4-5").
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Translate `text` into both targets at once, returning (primary, secondary).
    /// `context` is recent source-language text used only to disambiguate.
    pub fn translate_pair(
        &self,
        text: &str,
        source: &str,
        primary: &str,
        secondary: &str,
        context: Option<&str>,
    ) -> Result<(String, String)> {
        let src = lang_name(source);
        let p = lang_name(primary);
        let s = lang_name(secondary);

        let system = format!(
            "You are a real-time translator for VRChat voice chat. Translate the user's {src} message into {p} and {s}.\n\
             - Use natural, casual spoken register — how people actually talk in VRChat, not formal or written language.\n\
             - Translate faithfully: never add, omit, or embellish meaning. Render idioms by intended meaning, not word-for-word (e.g. Thai \"พี่น้อง\" addressing a crowd means \"everyone/folks\", not \"siblings\").\n\
             - Keep it concise; it goes in a small chat box.\n\
             - Output ONLY a JSON object, no markdown, no other text: {{\"primary\": \"<{p} translation>\", \"secondary\": \"<{s} translation>\"}}"
        );

        let user = match context {
            Some(c) if !c.trim().is_empty() => format!(
                "Recent conversation in {src} (context only — do NOT translate this):\n{c}\n\nNow translate this {src} message:\n{text}"
            ),
            _ => format!("Translate this {src} message:\n{text}"),
        };

        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 400,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
        });

        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .context("sending Claude request")?;

        let status = resp.status();
        let raw = resp.text().context("reading Claude response")?;
        if !status.is_success() {
            anyhow::bail!("Claude API {}: {}", status, raw);
        }

        let v: serde_json::Value = serde_json::from_str(&raw).context("parsing Claude response")?;
        if v["stop_reason"].as_str() == Some("refusal") {
            anyhow::bail!("Claude declined to translate this message");
        }

        // The text lives in the first text content block.
        let out = v["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
            .and_then(|b| b["text"].as_str())
            .unwrap_or("");

        parse_pair(out)
    }
}

/// Pull `primary`/`secondary` out of the model's JSON, tolerating stray text by
/// slicing from the first `{` to the last `}`.
fn parse_pair(s: &str) -> Result<(String, String)> {
    let json_str = match (s.find('{'), s.rfind('}')) {
        (Some(a), Some(b)) if b > a => &s[a..=b],
        _ => s,
    };
    let v: serde_json::Value =
        serde_json::from_str(json_str).with_context(|| format!("parsing Claude JSON output: {s}"))?;
    let primary = v["primary"].as_str().unwrap_or("").trim().to_string();
    let secondary = v["secondary"].as_str().unwrap_or("").trim().to_string();
    Ok((primary, secondary))
}

/// Human-readable language name for a DeepL-style code (e.g. "EN-US" -> "English").
fn lang_name(code: &str) -> &'static str {
    let c = code.to_uppercase();
    match c.split('-').next().unwrap_or("") {
        "TH" => "Thai",
        "JA" => "Japanese",
        "EN" => "English",
        "ZH" => "Chinese",
        "KO" => "Korean",
        "FR" => "French",
        "DE" => "German",
        "ES" => "Spanish",
        "VI" => "Vietnamese",
        "ID" => "Indonesian",
        _ => "the requested language",
    }
}
