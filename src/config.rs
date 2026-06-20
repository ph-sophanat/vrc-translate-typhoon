use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub deepl_key: String,
    #[serde(default = "default_source")]
    pub source_lang: String,
    #[serde(default = "default_primary")]
    pub target_primary: String,
    #[serde(default = "default_secondary")]
    pub target_secondary: String,
    #[serde(default = "default_osc")]
    pub osc_addr: String,
    /// Base URL of the local Typhoon ASR service (service/server.py).
    #[serde(default = "default_typhoon_url")]
    pub typhoon_url: String,

    /// Translation backend: "deepl" or "claude".
    #[serde(default = "default_mt_backend")]
    pub mt_backend: String,
    /// Anthropic API key (required when mt_backend = "claude").
    #[serde(default)]
    pub anthropic_key: String,
    /// Claude model id. Default is Haiku for low latency; override for quality.
    #[serde(default = "default_claude_model")]
    pub claude_model: String,

    /// NLLB model id (HuggingFace) for mt_backend = "nllb". The default 600M is
    /// fast on CPU; "facebook/nllb-200-3.3B" is higher quality but much slower.
    #[serde(default = "default_nllb_model")]
    pub nllb_model: String,
}

fn default_typhoon_url() -> String {
    "http://127.0.0.1:8765".into()
}
fn default_source() -> String {
    "th".into()
}
fn default_primary() -> String {
    "JA".into()
}
fn default_secondary() -> String {
    "EN-US".into()
}
fn default_osc() -> String {
    "127.0.0.1:9000".into()
}
fn default_mt_backend() -> String {
    "deepl".into()
}
fn default_claude_model() -> String {
    "claude-haiku-4-5".into()
}
fn default_nllb_model() -> String {
    "facebook/nllb-200-distilled-600M".into()
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Config> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).context("parsing config.toml")?;
        Ok(cfg)
    }

    /// Load `config.toml` from next to the executable first (how a packaged build
    /// is run — double-clicking sets an arbitrary working dir), then fall back to
    /// the current directory (how `cargo run` is used during development).
    pub fn load_default() -> Result<Config> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("config.toml"));
            }
        }
        candidates.push(PathBuf::from("config.toml"));

        let chosen = candidates.iter().find(|p| p.exists()).cloned();
        match chosen {
            Some(path) => Self::load(path),
            None => anyhow::bail!(
                "config.toml not found next to the program or in the current directory. \
                 Copy config.example.toml to config.toml and add your keys."
            ),
        }
    }

    /// DeepL source-language code (uppercased Whisper code, e.g. "th" -> "TH").
    pub fn deepl_source(&self) -> String {
        self.source_lang.to_uppercase()
    }
}
