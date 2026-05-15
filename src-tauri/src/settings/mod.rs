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
