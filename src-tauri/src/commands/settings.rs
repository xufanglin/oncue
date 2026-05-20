use tauri::AppHandle;

use crate::settings::{self, ProviderConfig, Providers};
use crate::translate::{TranslateError, build_provider};

// ── API Key masking ───────────────────────────────────────────────────────────

const MASK_PREFIX: &str = "sk-***...";

fn mask_api_key(key: &str) -> String {
    if key.len() <= 4 {
        return MASK_PREFIX.to_string();
    }
    format!("{}{}", MASK_PREFIX, &key[key.len() - 4..])
}

fn is_masked(key: &str) -> bool {
    key.starts_with(MASK_PREFIX)
}

fn mask_config(cfg: ProviderConfig) -> ProviderConfig {
    match cfg {
        ProviderConfig::OpenAiOfficial { api_key, model } => ProviderConfig::OpenAiOfficial {
            api_key: mask_api_key(&api_key),
            model,
        },
        ProviderConfig::AnthropicOfficial { api_key, model } => {
            ProviderConfig::AnthropicOfficial {
                api_key: mask_api_key(&api_key),
                model,
            }
        }
        ProviderConfig::OpenAiCompatible {
            base_url,
            api_key,
            model,
        } => ProviderConfig::OpenAiCompatible {
            base_url,
            api_key: mask_api_key(&api_key),
            model,
        },
        ProviderConfig::AnthropicCompatible {
            base_url,
            api_key,
            model,
        } => ProviderConfig::AnthropicCompatible {
            base_url,
            api_key: mask_api_key(&api_key),
            model,
        },
    }
}

fn get_api_key(cfg: &ProviderConfig) -> &str {
    match cfg {
        ProviderConfig::OpenAiOfficial { api_key, .. } => api_key,
        ProviderConfig::AnthropicOfficial { api_key, .. } => api_key,
        ProviderConfig::OpenAiCompatible { api_key, .. } => api_key,
        ProviderConfig::AnthropicCompatible { api_key, .. } => api_key,
    }
}

fn with_api_key(cfg: ProviderConfig, key: String) -> ProviderConfig {
    match cfg {
        ProviderConfig::OpenAiOfficial { model, .. } => {
            ProviderConfig::OpenAiOfficial { api_key: key, model }
        }
        ProviderConfig::AnthropicOfficial { model, .. } => {
            ProviderConfig::AnthropicOfficial { api_key: key, model }
        }
        ProviderConfig::OpenAiCompatible { base_url, model, .. } => {
            ProviderConfig::OpenAiCompatible { base_url, api_key: key, model }
        }
        ProviderConfig::AnthropicCompatible { base_url, model, .. } => {
            ProviderConfig::AnthropicCompatible { base_url, api_key: key, model }
        }
    }
}

/// If the incoming config has a masked api_key, replace it with the stored key.
fn resolve_masked_key(config: ProviderConfig, existing: &Providers) -> ProviderConfig {
    if !is_masked(get_api_key(&config)) {
        return config;
    }
    let stored = match &config {
        ProviderConfig::OpenAiOfficial { .. } => existing.openai_official.as_ref(),
        ProviderConfig::AnthropicOfficial { .. } => existing.anthropic_official.as_ref(),
        ProviderConfig::OpenAiCompatible { .. } => existing.openai_compatible.as_ref(),
        ProviderConfig::AnthropicCompatible { .. } => existing.anthropic_compatible.as_ref(),
    };
    match stored {
        Some(s) => with_api_key(config, get_api_key(s).to_string()),
        None => config,
    }
}

// ── Read providers ────────────────────────────────────────────────────────────

/// Return the persisted providers map with api_key fields masked.
/// Use `save_provider` to update; it handles masked keys transparently.
#[tauri::command]
pub fn get_providers(app: AppHandle) -> Result<Providers, String> {
    let s = settings::load(&app).map_err(|e| e.to_string())?;
    let p = s.providers;
    Ok(Providers {
        openai_official: p.openai_official.map(mask_config),
        anthropic_official: p.anthropic_official.map(mask_config),
        openai_compatible: p.openai_compatible.map(mask_config),
        anthropic_compatible: p.anthropic_compatible.map(mask_config),
        active: p.active,
    })
}

// ── Save provider ────────────────────────────────────────────────────────────

#[tauri::command]
pub fn save_provider(app: AppHandle, config: ProviderConfig) -> Result<(), String> {
    let mut s = settings::load(&app).unwrap_or_default();
    // If the frontend didn't change the api_key (still masked), restore the real key.
    let config = resolve_masked_key(config, &s.providers);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_shows_last_four() {
        assert_eq!(mask_api_key("sk-abcdef1234567890"), "sk-***...7890");
        assert_eq!(mask_api_key("0123456789"), "sk-***...6789");
    }

    #[test]
    fn mask_short_key_no_tail() {
        // Keys ≤ 4 chars: just the prefix, no tail (avoid leaking the full key).
        assert_eq!(mask_api_key("abcd"), "sk-***...");
        assert_eq!(mask_api_key(""), "sk-***...");
    }

    #[test]
    fn is_masked_detects_prefix() {
        assert!(is_masked("sk-***...1234"));
        assert!(is_masked("sk-***..."));
        assert!(!is_masked("sk-abcdef1234"));
        assert!(!is_masked("real-key-without-mask"));
    }

    #[test]
    fn mask_config_round_trips_each_variant() {
        let cases = vec![
            ProviderConfig::OpenAiOfficial {
                api_key: "sk-realkey1234".into(),
                model: "gpt-4o-mini".into(),
            },
            ProviderConfig::AnthropicOfficial {
                api_key: "sk-ant-abcd9999".into(),
                model: "claude-haiku-4-5".into(),
            },
            ProviderConfig::OpenAiCompatible {
                base_url: "https://api.example.com/v1".into(),
                api_key: "sk-localxxxx".into(),
                model: "qwen".into(),
            },
            ProviderConfig::AnthropicCompatible {
                base_url: "https://api.example.com".into(),
                api_key: "key-aaaa".into(),
                model: "anth-local".into(),
            },
        ];
        for cfg in cases {
            let original_key = get_api_key(&cfg).to_string();
            let masked = mask_config(cfg.clone());
            // Real key never appears in masked output.
            assert!(
                !get_api_key(&masked).contains(&original_key),
                "masked key still contains plaintext: {}",
                get_api_key(&masked)
            );
            // Masked key starts with prefix.
            assert!(is_masked(get_api_key(&masked)));
            // Non-key fields preserved.
            assert_eq!(cfg.provider_type(), masked.provider_type());
        }
    }

    #[test]
    fn resolve_masked_restores_stored_key() {
        let stored = ProviderConfig::OpenAiOfficial {
            api_key: "sk-real-secret-1234".into(),
            model: "gpt-4o-mini".into(),
        };
        let mut providers = Providers::default();
        providers.openai_official = Some(stored);

        // Frontend sends back the masked key, intending no change.
        let incoming = ProviderConfig::OpenAiOfficial {
            api_key: "sk-***...1234".into(),
            model: "gpt-4o-mini".into(),
        };
        let resolved = resolve_masked_key(incoming, &providers);
        assert_eq!(get_api_key(&resolved), "sk-real-secret-1234");
    }

    #[test]
    fn resolve_real_key_passes_through() {
        // User typed a new key — we should NOT touch it.
        let providers = Providers::default();
        let incoming = ProviderConfig::OpenAiOfficial {
            api_key: "sk-brand-new-key".into(),
            model: "gpt-4o-mini".into(),
        };
        let resolved = resolve_masked_key(incoming, &providers);
        assert_eq!(get_api_key(&resolved), "sk-brand-new-key");
    }

    #[test]
    fn resolve_masked_with_no_stored_falls_through() {
        // Edge case: incoming masked, but nothing stored. Keep the masked
        // string rather than crashing — save will write the placeholder, which
        // is non-functional but recoverable. Better than silent data loss.
        let providers = Providers::default();
        let incoming = ProviderConfig::OpenAiOfficial {
            api_key: "sk-***...1234".into(),
            model: "gpt-4o-mini".into(),
        };
        let resolved = resolve_masked_key(incoming, &providers);
        assert!(is_masked(get_api_key(&resolved)));
    }
}
