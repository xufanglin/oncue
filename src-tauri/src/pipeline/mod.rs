pub mod generate;
pub mod resync_fast;
pub mod resync_precise;

use serde::Serialize;

// ── Shared progress event ─────────────────────────────────────────────────────
//
// All three pipelines emit progress via the "pipeline:progress" Tauri event.
// The shape is identical across pipelines (superset of all variants used).

#[derive(Clone, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum ProgressEvent {
    ExtractAudio {
        percent: u8,
    },
    Asr {
        percent: u8,
        partial: Option<String>,
    },
    /// Used by resync pipelines.
    Align {
        percent: u8,
    },
    /// Used by the generate pipeline.
    Translate {
        current: usize,
        total: usize,
    },
    WriteOutput {
        done: bool,
    },
}
