use std::path::Path;

use anyhow::Context;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

use crate::align::offset::{apply_offset, find_offset};
use crate::asr::whisper::{AsrOptions, load_context, transcribe};
use crate::ffmpeg::{extract_audio_with_progress, resolve};
use crate::models;
use crate::settings;
use crate::srt::{
    parse::parse_file,
    render::{backup_srt, render, write_srt},
};

use super::ProgressEvent;

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
    #[error("no confident offset found (best similarity < 0.3); try precise mode")]
    NoConfidentOffset,
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

    // ── 3. Load model (once, reused across retries) ──
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
    let ctx = std::sync::Arc::new(ctx);

    // ── 4. Try a few sample windows. Films often open with logos / score
    // before any dialogue — if 30 s in still has no speech, jump deeper.
    const SAMPLE_DURATION_S: f64 = 90.0;
    const SAMPLE_START_OFFSETS_S: &[f64] = &[30.0, 5.0 * 60.0, 15.0 * 60.0];

    let mut offset_result = None;
    for (attempt, start_s) in SAMPLE_START_OFFSETS_S.iter().copied().enumerate() {
        let tried = attempt + 1;
        if cancel.is_cancelled() {
            return Err(ResyncError::Cancelled);
        }

        emit(&app, ProgressEvent::ExtractAudio { percent: 0 });
        let wav = tokio::task::spawn_blocking({
            let video = video_path.clone();
            let ff = ffmpeg.path.clone();
            let app_p = app.clone();
            move || {
                extract_audio_with_progress(
                    &ff,
                    &video,
                    Some(start_s),
                    Some(SAMPLE_DURATION_S),
                    |f| {
                        let _ = app_p.emit(
                            "pipeline:progress",
                            ProgressEvent::ExtractAudio {
                                percent: (f * 100.0) as u8,
                            },
                        );
                    },
                )
            }
        })
        .await
        .context("spawn_blocking extract_audio")?
        .map_err(|e| ResyncError::VideoAudioExtractFailed(e.to_string()))?;
        emit(&app, ProgressEvent::ExtractAudio { percent: 100 });

        if cancel.is_cancelled() {
            return Err(ResyncError::Cancelled);
        }

        emit(
            &app,
            ProgressEvent::Asr {
                percent: 0,
                partial: None,
            },
        );
        let asr_segments = tokio::task::spawn_blocking({
            let ctx = ctx.clone();
            let cancel2 = cancel.clone();
            move || transcribe(&ctx, &wav, &AsrOptions::default(), cancel2)
        })
        .await
        .context("spawn_blocking transcribe")?
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

        emit(&app, ProgressEvent::Align { percent: 0 });

        let window_start_ms = (start_s * 1000.0) as i64;
        let window_end_ms = ((start_s + SAMPLE_DURATION_S) * 1000.0) as i64;
        let srt_window: Vec<_> = srt_entries
            .iter()
            .filter(|e| {
                let start = e.start.as_millis() as i64;
                start >= window_start_ms && start < window_end_ms
            })
            .cloned()
            .collect();

        let shift_ms = (start_s * 1000.0) as i64;
        let asr_abs: Vec<_> = asr_segments
            .iter()
            .map(|s| crate::asr::whisper::Segment {
                start_ms: s.start_ms + shift_ms,
                end_ms: s.end_ms + shift_ms,
                text: s.text.clone(),
                language: s.language.clone(),
            })
            .collect();

        if let Some(r) = find_offset(&asr_abs, &srt_window) {
            tracing::info!(
                "fast resync: window {}s ok, offset={}{:.1}s confidence={:.3}",
                start_s,
                if r.positive { "+" } else { "-" },
                r.offset.as_secs_f64(),
                r.confidence
            );
            offset_result = Some(r);
            break;
        }

        tracing::warn!(
            "fast resync: window {}s failed (attempt {}/{}), retrying deeper",
            start_s,
            tried,
            SAMPLE_START_OFFSETS_S.len()
        );
    }

    let offset_result = offset_result.ok_or(ResyncError::NoConfidentOffset)?;

    emit(&app, ProgressEvent::Align { percent: 100 });

    // ── 6. Apply offset + write ──
    emit(&app, ProgressEvent::WriteOutput { done: false });

    let shifted = apply_offset(&srt_entries, &offset_result);
    let rendered = render(&shifted);

    // For .ass / .ssa input we leave the original alone and write a sibling
    // .srt file. For .srt input we back up in place and overwrite.
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
        // Keep encoding only when source was UTF-8 SRT; for ASS we always
        // emit UTF-8 to avoid encoding round-trip surprises.
        write_srt(&out, &rendered, crate::srt::parse::SourceEncoding::Utf8)
            .map_err(|e| ResyncError::WriteFailed(e.to_string()))?;
    }
    let _ = srt_enc; // keep var live for both branches without warning

    emit(&app, ProgressEvent::WriteOutput { done: true });
    Ok(())
}
