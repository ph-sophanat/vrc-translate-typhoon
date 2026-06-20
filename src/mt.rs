//! Machine-translation backend selector. The worker holds a `Translator` and
//! calls `translate_pair`; whether that's DeepL (two parallel calls) or Claude
//! (one call returning both languages) is chosen by `mt_backend` in config.

use crate::claude::Claude;
use crate::config::Config;
use crate::nllb::Nllb;
use crate::translate::DeepL;
use anyhow::Result;

pub enum Translator {
    Deepl(DeepL),
    /// Claude with a DeepL fallback used per-utterance when Claude errors
    /// (no credit, rate limit, network) — so the chatbox never goes blank.
    Claude { claude: Claude, fallback: DeepL },
    /// Local NLLB-200 via the Python service — free and fully offline.
    Nllb(Nllb),
}

impl Translator {
    pub fn from_config(cfg: &Config) -> Result<Translator> {
        match cfg.mt_backend.to_lowercase().as_str() {
            "deepl" => Ok(Translator::Deepl(DeepL::new(&cfg.deepl_key))),
            "claude" => {
                if cfg.anthropic_key.trim().is_empty() {
                    anyhow::bail!(
                        "mt_backend = \"claude\" but anthropic_key is empty in config.toml"
                    );
                }
                Ok(Translator::Claude {
                    claude: Claude::new(&cfg.anthropic_key, &cfg.claude_model),
                    fallback: DeepL::new(&cfg.deepl_key),
                })
            }
            "nllb" => Ok(Translator::Nllb(Nllb::new(&cfg.typhoon_url, &cfg.nllb_model))),
            other => anyhow::bail!(
                "unknown mt_backend \"{other}\" (use \"deepl\", \"claude\", or \"nllb\")"
            ),
        }
    }

    /// Human-readable name of the active backend, for status/logging.
    pub fn name(&self) -> &'static str {
        match self {
            Translator::Deepl(_) => "DeepL",
            Translator::Claude { .. } => "Claude",
            Translator::Nllb(_) => "NLLB",
        }
    }

    /// Specific model id, when the backend has one. DeepL returns None.
    pub fn model(&self) -> Option<&str> {
        match self {
            Translator::Deepl(_) => None,
            Translator::Claude { claude, .. } => Some(claude.model()),
            Translator::Nllb(n) => Some(n.model()),
        }
    }

    /// Translate to both targets, returning (primary, secondary). Per-language
    /// failures degrade to an empty string rather than dropping the whole result.
    pub fn translate_pair(
        &self,
        text: &str,
        source: &str,
        primary: &str,
        secondary: &str,
        context: Option<&str>,
    ) -> (String, String) {
        match self {
            Translator::Deepl(d) => deepl_pair(d, text, source, primary, secondary, context),

            // One Claude call returns both languages; on any error fall back to
            // DeepL for this utterance so the user still gets a translation.
            Translator::Claude { claude, fallback } => {
                match claude.translate_pair(text, source, primary, secondary, context) {
                    Ok(pair) => pair,
                    Err(e) => {
                        eprintln!("claude translate error: {e} — falling back to DeepL");
                        deepl_pair(fallback, text, source, primary, secondary, context)
                    }
                }
            }

            // Local NLLB returns both languages in one request. It's offline,
            // so there's no sensible fallback — degrade to empty on error.
            Translator::Nllb(n) => match n.translate_pair(text, source, primary, secondary) {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("nllb translate error: {e}");
                    (String::new(), String::new())
                }
            },
        }
    }
}

/// DeepL's two translations, run concurrently (one on a scoped thread).
fn deepl_pair(
    d: &DeepL,
    text: &str,
    source: &str,
    primary: &str,
    secondary: &str,
    context: Option<&str>,
) -> (String, String) {
    std::thread::scope(|scope| {
        let h = scope.spawn(|| d.translate(text, source, primary, context));
        let secondary_t = d
            .translate(text, source, secondary, context)
            .unwrap_or_else(|e| {
                eprintln!("translate {secondary} error: {e}");
                String::new()
            });
        let primary_t = h.join().unwrap().unwrap_or_else(|e| {
            eprintln!("translate {primary} error: {e}");
            String::new()
        });
        (primary_t, secondary_t)
    })
}
