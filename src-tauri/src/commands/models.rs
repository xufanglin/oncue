use crate::models::{self, MODELS, ModelStatus};
use crate::settings;

// Single-flight guard. Only one model download may run at a time; further
// requests return early so duplicate clicks don't kick off parallel writes
// to the same .tmp file.
static MODEL_DOWNLOAD_BUSY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

struct DownloadGuard;
impl DownloadGuard {
    fn try_acquire() -> Option<Self> {
        if MODEL_DOWNLOAD_BUSY
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            Some(Self)
        } else {
            None
        }
    }
}
impl Drop for DownloadGuard {
    fn drop(&mut self) {
        MODEL_DOWNLOAD_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}
use anyhow::Context;
use reqwest::Client;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use tauri::{AppHandle, Emitter};

#[tauri::command]
pub async fn list_models(app: AppHandle) -> Result<Vec<ModelStatus>, String> {
    models::list_status(&app).map_err(|e| e.to_string())
}

// ── Download progress event ───────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct DownloadProgress {
    pub name: String,
    pub downloaded: u64,
    pub total: u64,
    /// 0.0–1.0
    pub fraction: f32,
    pub done: bool,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn download_model(app: AppHandle, name: String) -> Result<(), String> {
    let _guard = DownloadGuard::try_acquire()
        .ok_or_else(|| "another model download is already in progress".to_string())?;

    let meta = MODELS
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| format!("unknown model '{name}'"))?;

    let dir = models::models_dir(&app).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create {}", dir.display()))
        .map_err(|e| e.to_string())?;

    let dest = dir.join(meta.name);
    let tmp = dir.join(format!("{}.tmp", meta.name));

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .get(meta.url)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("HTTP {} for {}", response.status(), meta.url));
    }

    let total = response.content_length().unwrap_or(meta.size_bytes);
    let mut file = std::fs::File::create(&tmp)
        .with_context(|| format!("cannot create {}", tmp.display()))
        .map_err(|e| e.to_string())?;

    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let mut last_pct: u8 = 0;
    let mut stream = response.bytes_stream();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;

        // Throttle: only emit when integer percent advances. Otherwise the
        // chunk-rate event flood (hundreds/sec) makes the UI bar jitter as
        // each setState interrupts the bar's CSS transition.
        let pct = if total > 0 {
            ((downloaded as f64 / total as f64) * 100.0) as u8
        } else {
            0
        };
        if pct > last_pct {
            last_pct = pct;
            let _ = app.emit(
                "model:download_progress",
                DownloadProgress {
                    name: name.clone(),
                    downloaded,
                    total,
                    fraction: downloaded as f32 / total as f32,
                    done: false,
                    error: None,
                },
            );
        }
    }
    drop(file);

    // SHA-256 verify
    let digest = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if digest != meta.sha256 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "SHA-256 mismatch for {name}: expected {} got {digest}",
            meta.sha256
        ));
    }

    std::fs::rename(&tmp, &dest)
        .with_context(|| format!("cannot rename to {}", dest.display()))
        .map_err(|e| e.to_string())?;

    // Persist as last-used model so pipelines pick it up by default.
    let mut s = settings::load(&app).unwrap_or_default();
    s.last_model = Some(name.clone());
    let _ = settings::save(&app, &s);

    let _ = app.emit(
        "model:download_progress",
        DownloadProgress {
            name: name.clone(),
            downloaded,
            total,
            fraction: 1.0,
            done: true,
            error: None,
        },
    );

    tracing::info!("model {name} downloaded and verified");
    Ok(())
}
