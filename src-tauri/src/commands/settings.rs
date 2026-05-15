use tauri::AppHandle;

use crate::settings::{self, ProviderConfig, Providers};
use crate::translate::{TranslateError, build_provider};

// ── Read providers ────────────────────────────────────────────────────────────

/// Return the persisted providers map. Sensitive fields (api_key) are kept
/// in the response so the UI can display masked previews; the frontend MUST
/// not log them. For multi-user/cloud builds we'd redact here.
#[tauri::command]
pub fn get_providers(app: AppHandle) -> Result<Providers, String> {
    let s = settings::load(&app).map_err(|e| e.to_string())?;
    Ok(s.providers)
}

// ── Save provider ────────────────────────────────────────────────────────────

#[tauri::command]
pub fn save_provider(app: AppHandle, config: ProviderConfig) -> Result<(), String> {
    let mut s = settings::load(&app).unwrap_or_default();
    let key = config.provider_type().to_string();
    match &config {
        ProviderConfig::OpenAiOfficial { .. } => s.providers.openai_official = Some(config),
        ProviderConfig::AnthropicOfficial { .. } => s.providers.anthropic_official = Some(config),
        ProviderConfig::OpenAiCompatible { .. } => s.providers.openai_compatible = Some(config),
        ProviderConfig::AnthropicCompatible { .. } => {
            s.providers.anthropic_compatible = Some(config)
        }
    }
    s.providers.active = Some(key);
    settings::save(&app, &s).map_err(|e| e.to_string())
}

// ── Set active ────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn set_active_provider(app: AppHandle, provider_type: String) -> Result<(), String> {
    let mut s = settings::load(&app).unwrap_or_default();
    s.providers.active = Some(provider_type);
    settings::save(&app, &s).map_err(|e| e.to_string())
}

// ── Active whisper model ──────────────────────────────────────────────────────

#[tauri::command]
pub fn get_active_model(app: AppHandle) -> Option<String> {
    settings::load(&app).ok().and_then(|s| s.last_model)
}

#[tauri::command]
pub fn set_active_model(app: AppHandle, name: String) -> Result<(), String> {
    let mut s = settings::load(&app).unwrap_or_default();
    s.last_model = Some(name);
    settings::save(&app, &s).map_err(|e| e.to_string())
}

// ── Test connection ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn test_provider(config: ProviderConfig) -> Result<(), String> {
    let provider = build_provider(&config);
    provider.ping().await.map_err(|e| match e {
        TranslateError::Auth(m) => format!("auth failed: {m}"),
        other => other.to_string(),
    })
}
