use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::AppHandle;
use tauri::Manager;

// ── Provider configuration ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ProviderConfig {
    #[serde(rename = "openai_official")]
    OpenAiOfficial {
        api_key: String,
        /// Defaults to "gpt-4o-mini"
        model: String,
    },
    #[serde(rename = "anthropic_official")]
    AnthropicOfficial {
        api_key: String,
        /// Defaults to "claude-haiku-4-5"
        model: String,
    },
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible {
        base_url: String,
        api_key: String,
        model: String,
    },
    #[serde(rename = "anthropic_compatible")]
    AnthropicCompatible {
        base_url: String,
        api_key: String,
        model: String,
    },
}

impl ProviderConfig {
    pub fn provider_type(&self) -> &'static str {
        match self {
            Self::OpenAiOfficial { .. } => "openai_official",
            Self::AnthropicOfficial { .. } => "anthropic_official",
            Self::OpenAiCompatible { .. } => "openai_compatible",
            Self::AnthropicCompatible { .. } => "anthropic_compatible",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Providers {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_official: Option<ProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_official: Option<ProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_compatible: Option<ProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_compatible: Option<ProviderConfig>,
    /// Active provider type key ("openai_official" | "anthropic_official" | …)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
}

// ── Settings ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    pub providers: Providers,
    /// Last used whisper model name (e.g. "ggml-large-v3.bin")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
}

// ── Path ──────────────────────────────────────────────────────────────────────

pub fn settings_path(app: &AppHandle) -> Result<PathBuf> {
    let mut p = app
        .path()
        .app_data_dir()
        .context("cannot resolve app data dir")?;
    p.push("settings.json");
    Ok(p)
}

// ── I/O ───────────────────────────────────────────────────────────────────────

pub fn load(app: &AppHandle) -> Result<Settings> {
    let path = settings_path(app)?;
    if !path.exists() {
        return Ok(Settings::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("cannot parse {}", path.display()))
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<()> {
    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(settings).context("cannot serialize settings")?;
    std::fs::write(&path, json).with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_config_round_trip_openai_official() {
        let cfg = ProviderConfig::OpenAiOfficial {
            api_key: "sk-test".into(),
            model: "gpt-4o-mini".into(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        // Tag-based serialization
        assert!(json.contains(r#""type":"openai_official""#));
        let back: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn provider_config_round_trip_all_variants() {
        let cases = vec![
            ProviderConfig::OpenAiOfficial {
                api_key: "k1".into(),
                model: "m1".into(),
            },
            ProviderConfig::AnthropicOfficial {
                api_key: "k2".into(),
                model: "m2".into(),
            },
            ProviderConfig::OpenAiCompatible {
                base_url: "https://x.example/v1".into(),
                api_key: "k3".into(),
                model: "m3".into(),
            },
            ProviderConfig::AnthropicCompatible {
                base_url: "https://y.example".into(),
                api_key: "k4".into(),
                model: "m4".into(),
            },
        ];
        for cfg in cases {
            let json = serde_json::to_string(&cfg).unwrap();
            let back: ProviderConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(cfg, back);
            assert_eq!(cfg.provider_type(), back.provider_type());
        }
    }

    #[test]
    fn settings_round_trip_preserves_active() {
        let mut s = Settings::default();
        s.providers.openai_official = Some(ProviderConfig::OpenAiOfficial {
            api_key: "secret".into(),
            model: "gpt-4o-mini".into(),
        });
        s.providers.active = Some("openai_official".into());
        s.last_model = Some("ggml-large-v3.bin".into());

        let json = serde_json::to_string_pretty(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();

        assert_eq!(back.providers.active.as_deref(), Some("openai_official"));
        assert_eq!(back.last_model.as_deref(), Some("ggml-large-v3.bin"));
        assert!(back.providers.openai_official.is_some());
    }

    #[test]
    fn settings_default_serializes_minimally() {
        // Empty settings should not bloat the file with `null` fields.
        let s = Settings::default();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("null"));
        // Should round-trip cleanly.
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert!(back.providers.active.is_none());
        assert!(back.last_model.is_none());
    }

    #[test]
    fn settings_load_missing_file_returns_default() {
        // We can't test `load()` directly without an AppHandle, but we can
        // verify the JSON shape it parses. An empty providers map / missing
        // file behavior is exercised at the Settings::default() level.
        let parsed: Settings = serde_json::from_str(r#"{"providers":{}}"#).unwrap();
        assert!(parsed.providers.active.is_none());
        assert!(parsed.last_model.is_none());
    }

    #[test]
    fn provider_config_legacy_format_rejection() {
        // If a future renames "openai_official" → "openai", old settings should
        // fail to parse rather than silently fall back to a default. This locks
        // the wire format.
        let bad = r#"{"type":"openai","api_key":"x","model":"y"}"#;
        assert!(serde_json::from_str::<ProviderConfig>(bad).is_err());
    }
}
