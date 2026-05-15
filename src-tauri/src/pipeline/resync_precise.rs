use std::path::Path;

use anyhow::Context;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

use crate::align::nw::{align, derive_timecodes, srt_to_tokens};
use crate::asr::whisper::{AsrOptions, load_context, transcribe_words};
use crate::ffmpeg::{extract_audio_with_progress, resolve};
use crate::models;
use crate::settings;
use crate::srt::{
    parse::{SrtEntry, parse_file},
    render::{backup_srt, render, write_srt},
};

// ── NW bandwidth ──────────────────────────────────────────────────────────────
const NW_BANDWIDTH: usize = 500;

// ── Progress event (reuse same shape as fast mode) ────────────────────────────

#[derive(Clone, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum ProgressEvent {
    ExtractAudio {
        percent: u8,
    },
    Asr {
        percent: u8,
        partial: Option<String>,
    },
    Align {
        percent: u8,
    },
    WriteOutput {
        done: bool,
    },
    Error {
        message: String,
    },
}

fn emit(app: &AppHandle, ev: ProgressEvent) {
    let _ = app.emit("pipeline:progress", ev);
}

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ResyncError {
    #[error("SRT file not found: {0}")]
    SrtNotFound(String),
    #[error("SRT parse failed: {0}")]
    SrtParseFailed(String),
    #[error("audio extraction failed: {0}")]
    VideoAudioExtractFailed(String),
    #[error("ASR failed: {0}")]
    AsrFailed(String),
    #[error("write failed: {0}")]
    WriteFailed(String),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Serialize for ResyncError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

pub async fn run(
    app: AppHandle,
    video_path: String,
    srt_path: String,
    cancel: CancellationToken,
) -> Result<(), ResyncError> {
    // ── 1. Resolve ffmpeg ──
    let ffmpeg = resolve(&app).map_err(ResyncError::Other)?;

    // ── 2. Load SRT ──
    let srt_p = Path::new(&srt_path);
    if !srt_p.exists() {
        return Err(ResyncError::SrtNotFound(srt_path.clone()));
    }
    let (srt_entries, srt_enc) =
        parse_file(srt_p).map_err(|e| ResyncError::SrtParseFailed(e.to_string()))?;

    // ── 3. Extract full audio ──
    emit(&app, ProgressEvent::ExtractAudio { percent: 0 });
    if cancel.is_cancelled() {
        return Err(ResyncError::Cancelled);
    }

    let wav = tokio::task::spawn_blocking({
        let video = video_path.clone();
        let ff = ffmpeg.path.clone();
        let app_p = app.clone();
        move || {
            extract_audio_with_progress(&ff, &video, None, None, |f| {
                let _ = app_p.emit(
                    "pipeline:progress",
                    ProgressEvent::ExtractAudio {
                        percent: (f * 100.0) as u8,
                    },
                );
            })
        }
    })
    .await
    .context("spawn_blocking extract_audio")?
    .map_err(|e| ResyncError::VideoAudioExtractFailed(e.to_string()))?;

    emit(&app, ProgressEvent::ExtractAudio { percent: 100 });
    if cancel.is_cancelled() {
        return Err(ResyncError::Cancelled);
    }

    // ── 4. Load model + word-level ASR ──
    emit(
        &app,
        ProgressEvent::Asr {
            percent: 0,
            partial: None,
        },
    );

    let model_path = {
        let settings = settings::load(&app).unwrap_or_default();
        let name = settings
            .last_model
            .or_else(|| models::first_present_model(&app))
            .ok_or_else(|| ResyncError::AsrFailed("no whisper model installed".into()))?;
        models::model_path(&app, &name).map_err(ResyncError::Other)?
    };

    let ctx = tokio::task::spawn_blocking({
        let mp = model_path.clone();
        move || load_context(&mp)
    })
    .await
    .context("spawn_blocking load_context")?
    .map_err(|e| ResyncError::AsrFailed(e.to_string()))?;

    let asr_words = tokio::task::spawn_blocking({
        let cancel2 = cancel.clone();
        move || transcribe_words(&ctx, &wav, &AsrOptions::default(), cancel2)
    })
    .await
    .context("spawn_blocking transcribe_words")?
    .map_err(|e| ResyncError::AsrFailed(e.to_string()))?;

    emit(
        &app,
        ProgressEvent::Asr {
            percent: 100,
            partial: None,
        },
    );
    if cancel.is_cancelled() {
        return Err(ResyncError::Cancelled);
    }

    // ── 5. Banded NW alignment ──
    emit(&app, ProgressEvent::Align { percent: 0 });

    let (srt_tokens, srt_spans) = srt_to_tokens(&srt_entries);
    let asr_token_strings: Vec<String> = asr_words.iter().map(|w| w.text.clone()).collect();

    let ops = tokio::task::spawn_blocking({
        let srt_t = srt_tokens.clone();
        move || {
            let srt_refs: Vec<&str> = srt_t.iter().map(String::as_str).collect();
            let asr_refs: Vec<&str> = asr_token_strings.iter().map(String::as_str).collect();
            align(&srt_refs, &asr_refs, NW_BANDWIDTH)
        }
    })
    .await
    .context("spawn_blocking align")?;

    let timings = derive_timecodes(&ops, &asr_words, &srt_entries, &srt_spans);

    emit(&app, ProgressEvent::Align { percent: 100 });

    // ── 6. Rewrite SRT entries with new timecodes ──
    let updated: Vec<SrtEntry> = srt_entries
        .iter()
        .zip(timings.iter())
        .map(|(e, t)| SrtEntry {
            idx: e.idx,
            start: t.start,
            end: t.end,
            text: e.text.clone(),
        })
        .collect();

    let rendered = render(&updated);

    emit(&app, ProgressEvent::WriteOutput { done: false });
    let is_srt = srt_p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.eq_ignore_ascii_case("srt"))
        .unwrap_or(true);
    if is_srt {
        backup_srt(srt_p).map_err(|e| ResyncError::WriteFailed(e.to_string()))?;
        write_srt(srt_p, &rendered, srt_enc)
            .map_err(|e| ResyncError::WriteFailed(e.to_string()))?;
    } else {
        let out = srt_p.with_extension("srt");
        write_srt(&out, &rendered, crate::srt::parse::SourceEncoding::Utf8)
            .map_err(|e| ResyncError::WriteFailed(e.to_string()))?;
    }
    let _ = srt_enc;

    emit(&app, ProgressEvent::WriteOutput { done: true });

    tracing::info!(
        "precise resync done: {} entries, {} asr words, {} from alignment",
        srt_entries.len(),
        asr_words.len(),
        timings.iter().filter(|t| t.from_alignment).count(),
    );

    Ok(())
}
