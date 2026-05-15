use std::sync::Mutex;

use tauri::{AppHandle, State};
use tokio_util::sync::CancellationToken;

use crate::pipeline::{generate, resync_fast, resync_precise};

// ── Shared cancel token ───────────────────────────────────────────────────────

pub struct PipelineState {
    cancel: Mutex<Option<CancellationToken>>,
}

impl PipelineState {
    pub fn new() -> Self {
        Self {
            cancel: Mutex::new(None),
        }
    }

    fn start(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self.cancel.lock().unwrap() = Some(token.clone());
        token
    }

    fn cancel(&self) {
        if let Some(t) = self.cancel.lock().unwrap().take() {
            t.cancel();
        }
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_resync_fast(
    app: AppHandle,
    state: State<'_, PipelineState>,
    video_path: String,
    srt_path: String,
) -> Result<(), String> {
    let cancel = state.start();
    resync_fast::run(app, video_path, srt_path, cancel)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_resync_precise(
    app: AppHandle,
    state: State<'_, PipelineState>,
    video_path: String,
    srt_path: String,
) -> Result<(), String> {
    let cancel = state.start();
    resync_precise::run(app, video_path, srt_path, cancel)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_generate(
    app: AppHandle,
    state: State<'_, PipelineState>,
    video_path: String,
    target_lang: String,
    force: Option<bool>,
    embedded_subtitle_index: Option<u32>,
) -> Result<(), String> {
    let cancel = state.start();
    generate::run(
        app,
        video_path,
        target_lang,
        force.unwrap_or(false),
        embedded_subtitle_index,
        cancel,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cancel_pipeline(state: State<'_, PipelineState>) {
    state.cancel();
}
