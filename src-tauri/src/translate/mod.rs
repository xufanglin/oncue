pub mod anthropic;
pub mod chunker;
pub mod openai;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::settings::ProviderConfig;

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum TranslateError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("auth failed: {0}")]
    Auth(String),
    #[error("rate limited")]
    RateLimited,
    #[error("provider returned malformed response: {0}")]
    Malformed(String),
    #[error("provider error: {0}")]
    ProviderError(String),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// ── Translate context ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranslateContext {
    /// Source language code or human label (e.g. "en", "English"). Optional.
    pub source_lang: Option<String>,
    /// Target language. Required for non-trivial translation.
    pub target_lang: String,
    /// Up to a few previous lines of conversation/subtitle history.
    pub history: Vec<String>,
}

// ── Provider trait ────────────────────────────────────────────────────────────

#[async_trait]
pub trait TranslateProvider: Send + Sync {
    /// Translate `chunk` (each entry is one line/sentence). Returned vector
    /// MUST have the same length as `chunk` and be in matching order.
    async fn translate(
        &self,
        chunk: &[String],
        context: &TranslateContext,
    ) -> Result<Vec<String>, TranslateError>;

    /// Cheap connectivity test: returns Ok if credentials and endpoint work.
    async fn ping(&self) -> Result<(), TranslateError>;
}

// ── Factory ───────────────────────────────────────────────────────────────────

pub fn build_provider(cfg: &ProviderConfig) -> Box<dyn TranslateProvider> {
    match cfg {
        ProviderConfig::OpenAiOfficial { api_key, model } => Box::new(openai::OpenAiProvider::new(
            "https://api.openai.com/v1".to_string(),
            api_key.clone(),
            model.clone(),
        )),
        ProviderConfig::OpenAiCompatible {
            base_url,
            api_key,
            model,
        } => Box::new(openai::OpenAiProvider::new(
            base_url.trim_end_matches('/').to_string(),
            api_key.clone(),
            model.clone(),
        )),
        ProviderConfig::AnthropicOfficial { api_key, model } => {
            Box::new(anthropic::AnthropicProvider::new(
                "https://api.anthropic.com".to_string(),
                api_key.clone(),
                model.clone(),
            ))
        }
        ProviderConfig::AnthropicCompatible {
            base_url,
            api_key,
            model,
        } => Box::new(anthropic::AnthropicProvider::new(
            base_url.trim_end_matches('/').to_string(),
            api_key.clone(),
            model.clone(),
        )),
    }
}
