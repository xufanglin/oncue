use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::AppHandle;
use tauri::Manager;

// ── Model metadata ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMeta {
    pub name: &'static str,
    /// Human-readable label
    pub label: &'static str,
    /// Approximate size in bytes
    pub size_bytes: u64,
    pub sha256: &'static str,
    pub url: &'static str,
    /// Short user-facing tip about quality / speed trade-offs.
    pub description: &'static str,
}

pub const MODELS: &[ModelMeta] = &[
    // SHA-256 values are HuggingFace x-linked-etag for the canonical files.
    // Fetch via:
    //   curl -sI -H 'Range: bytes=0-0' \
    //     https://huggingface.co/ggerganov/whisper.cpp/resolve/main/<file>
    ModelMeta {
        name: "ggml-large-v3.bin",
        label: "large-v3 (~3.1 GB)",
        size_bytes: 3_095_033_483,
        sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
        description: "最高精度。M2 Pro + CoreML 处理 2h 视频约 25–35 分钟。",
    },
    ModelMeta {
        name: "ggml-medium.bin",
        label: "medium (~1.5 GB)",
        size_bytes: 1_533_763_059,
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        description: "精度/速度均衡。2h 视频约 12–18 分钟。",
    },
    ModelMeta {
        name: "ggml-small.bin",
        label: "small (~466 MB)",
        size_bytes: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        description: "速度最快，适合快档对齐。2h 视频约 4–6 分钟。",
    },
];

// ── Paths ─────────────────────────────────────────────────────────────────────

pub fn models_dir(app: &AppHandle) -> Result<PathBuf> {
    let mut p = app
        .path()
        .app_data_dir()
        .context("cannot resolve app data dir")?;
    p.push("models");
    Ok(p)
}

pub fn model_path(app: &AppHandle, name: &str) -> Result<PathBuf> {
    Ok(models_dir(app)?.join(name))
}

// ── Status ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatus {
    pub name: String,
    pub label: String,
    pub size_bytes: u64,
    pub present: bool,
    /// File size on disk; None if not present.
    pub disk_bytes: Option<u64>,
    pub description: String,
}

pub fn list_status(app: &AppHandle) -> Result<Vec<ModelStatus>> {
    let dir = models_dir(app)?;
    MODELS
        .iter()
        .map(|m| {
            let path = dir.join(m.name);
            let disk_bytes = if path.exists() {
                std::fs::metadata(&path).ok().map(|md| md.len())
            } else {
                None
            };
            Ok(ModelStatus {
                name: m.name.to_string(),
                label: m.label.to_string(),
                size_bytes: m.size_bytes,
                // Existence (non-empty) is enough; SHA-256 was verified at
                // download time, and `size_bytes` constants are only approximate.
                present: disk_bytes.is_some_and(|b| b > 0),
                disk_bytes,
                description: m.description.to_string(),
            })
        })
        .collect()
}

/// Return true when at least one model file is fully present on disk.
pub fn any_model_ready(app: &AppHandle) -> bool {
    list_status(app)
        .map(|statuses| statuses.iter().any(|s| s.present))
        .unwrap_or(false)
}

/// Return the name of the first model that's actually present on disk,
/// preferring the order declared in `MODELS` (large → medium → small).
pub fn first_present_model(app: &AppHandle) -> Option<String> {
    list_status(app)
        .ok()?
        .into_iter()
        .find(|s| s.present)
        .map(|s| s.name)
}
