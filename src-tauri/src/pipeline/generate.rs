use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

use crate::asr::whisper::{AsrOptions, Segment, load_context, transcribe};
use crate::ffmpeg::{extract_audio_with_progress, extract_subtitle, resolve};
use crate::models;
use crate::settings::{self, ProviderConfig, Providers};
use crate::srt::parse::parse_str as parse_srt_str;
use crate::srt::{
    parse::SrtEntry,
    render::{BilingualEntry, render_bilingual, write_srt},
};
use crate::translate::{
    TranslateError, build_provider,
    chunker::{DEFAULT_CHUNK_SIZE, translate_in_chunks},
};

// ── Progress event ────────────────────────────────────────────────────────────

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
    Translate {
        current: usize,
        total: usize,
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
pub enum GenerateError {
    #[error("video file not found: {0}")]
    VideoNotFound(String),
    #[error("audio extraction failed: {0}")]
    VideoAudioExtractFailed(String),
    #[error("ASR failed: {0}")]
    AsrFailed(String),
    #[error("no provider configured")]
    NoProvider,
    #[error("translate failed: {0}")]
    TranslateFailed(String),
    #[error("output already exists: {0}")]
    OutputExists(String),
    #[error("write failed: {0}")]
    WriteFailed(String),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Serialize for GenerateError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn output_path(video: &Path) -> PathBuf {
    let mut p = video.to_path_buf();
    p.set_extension("srt");
    p
}

fn pick_active_provider(p: &Providers) -> Option<&ProviderConfig> {
    let key = p.active.as_deref()?;
    match key {
        "openai_official" => p.openai_official.as_ref(),
        "anthropic_official" => p.anthropic_official.as_ref(),
        "openai_compatible" => p.openai_compatible.as_ref(),
        "anthropic_compatible" => p.anthropic_compatible.as_ref(),
        _ => None,
    }
}

/// Whisper often emits sound-effect / non-speech annotations like
/// `[Music]`, `(applause)`, `♪♪`, `【掌声】`. These don't translate well and
/// add noise to the bilingual output, so we drop them entirely.
fn is_non_speech(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    // Strip an outer pair of bracket-like delimiters, then check if anything
    // letter-like remains. If the entire content sits inside one pair of
    // brackets (or a music-note pair), treat it as a sound-effect cue.
    const BRACKETS: &[(char, char)] = &[
        ('[', ']'),
        ('(', ')'),
        ('（', '）'),
        ('【', '】'),
        ('♪', '♪'),
        ('♫', '♫'),
        ('*', '*'),
    ];
    let chars: Vec<char> = t.chars().collect();
    if chars.len() < 2 {
        return false;
    }
    let first = chars[0];
    let last = *chars.last().unwrap();
    BRACKETS
        .iter()
        .any(|(open, close)| first == *open && last == *close)
}

/// Maximum on-screen duration for a single subtitle entry. Whisper sometimes
/// extends a segment's `end_ms` into trailing silence (e.g. a short line that
/// "lasts" 9 s). Industry convention is ≤ 6–7 s per cue.
const MAX_DURATION_MS: u64 = 7_000;
/// Leave a small gap so consecutive cues don't visually merge.
const MIN_GAP_MS: u64 = 50;

fn segments_to_entries(segments: &[Segment]) -> Vec<SrtEntry> {
    use std::time::Duration;

    // First pass: filter + collect raw start/end ms.
    let kept: Vec<(u64, u64, String)> = segments
        .iter()
        .filter(|s| !is_non_speech(&s.text))
        .map(|s| {
            (
                s.start_ms.max(0) as u64,
                s.end_ms.max(0) as u64,
                s.text.trim().to_string(),
            )
        })
        .collect();

    // Second pass: cap each end at min(start + 7s, next.start - 50ms).
    let n = kept.len();
    kept.iter()
        .enumerate()
        .map(|(i, (start, end, text))| {
            let hard_cap = start + MAX_DURATION_MS;
            let next_cap = if i + 1 < n {
                kept[i + 1].0.saturating_sub(MIN_GAP_MS)
            } else {
                u64::MAX
            };
            let capped_end = (*end).min(hard_cap).min(next_cap).max(*start + 200);
            SrtEntry {
                idx: (i as u32) + 1,
                start: Duration::from_millis(*start),
                end: Duration::from_millis(capped_end),
                text: text.clone(),
            }
        })
        .collect()
}

/// Convert pre-parsed SRT entries (from an embedded text subtitle stream)
/// into the same shape we use for ASR output: drop sound-effect cues,
/// renumber indices. Original timestamps are trusted as-is — they were
/// authored by the production crew and don't need 7-sec capping.
fn entries_from_srt(parsed: Vec<SrtEntry>) -> Vec<SrtEntry> {
    parsed
        .into_iter()
        .filter(|e| !is_non_speech(&e.text))
        .enumerate()
        .map(|(i, mut e)| {
            e.idx = (i as u32) + 1;
            e
        })
        .collect()
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

pub async fn run(
    app: AppHandle,
    video_path: String,
    target_lang: String,
    force: bool,
    // If `Some(N)`, extract the N-th text subtitle stream from the video
    // instead of running Whisper ASR. Saves minutes per run when the
    // container ships a clean text subtitle track (mov_text / subrip /
    // ass / webvtt).
    embedded_subtitle_index: Option<u32>,
    cancel: CancellationToken,
) -> Result<(), GenerateError> {
    let ffmpeg = resolve(&app).map_err(GenerateError::Other)?;

    let video_p = Path::new(&video_path);
    if !video_p.exists() {
        return Err(GenerateError::VideoNotFound(video_path.clone()));
    }

    let out_path = output_path(video_p);
    if out_path.exists() {
        if !force {
            return Err(GenerateError::OutputExists(out_path.display().to_string()));
        }
        // User confirmed overwrite: move existing srt aside as <name>.srt.bak.
        // We do this BEFORE the long-running ASR/translate work so a cancelled
        // run doesn't lose the user's data.
        let bak = out_path.with_extension("srt.bak");
        std::fs::rename(&out_path, &bak).map_err(|e| {
            GenerateError::WriteFailed(format!(
                "cannot back up existing {}: {}",
                out_path.display(),
                e
            ))
        })?;
        tracing::info!(
            "backed up existing {} -> {}",
            out_path.display(),
            bak.display()
        );
    }

    let settings_loaded = settings::load(&app).unwrap_or_default();
    let provider_cfg = pick_active_provider(&settings_loaded.providers)
        .ok_or(GenerateError::NoProvider)?
        .clone();

    let (entries, source_lang) = if let Some(stream_index) = embedded_subtitle_index {
        // Fast path: pull the embedded subtitle stream out of the container
        // and parse it directly. No ASR needed.
        emit(&app, ProgressEvent::ExtractAudio { percent: 100 });
        emit(
            &app,
            ProgressEvent::Asr {
                percent: 100,
                partial: None,
            },
        );

        let srt_text = tokio::task::spawn_blocking({
            let video = video_path.clone();
            let ff = ffmpeg.path.clone();
            move || extract_subtitle(&ff, &video, stream_index)
        })
        .await
        .context("spawn_blocking extract_subtitle")?
        .map_err(|e| GenerateError::AsrFailed(format!("subtitle extract failed: {e}")))?;

        let parsed = parse_srt_str(&srt_text)
            .map_err(|e| GenerateError::AsrFailed(format!("subtitle parse failed: {e}")))?;
        let entries = entries_from_srt(parsed);
        if entries.is_empty() {
            return Err(GenerateError::AsrFailed(
                "embedded subtitle stream produced no entries".into(),
            ));
        }
        tracing::info!(
            "using embedded subtitle stream 0:s:{} ({} entries)",
            stream_index,
            entries.len()
        );
        (entries, None)
    } else {
        // Default path: ffmpeg → audio → Whisper.
        emit(&app, ProgressEvent::ExtractAudio { percent: 0 });
        if cancel.is_cancelled() {
            return Err(GenerateError::Cancelled);
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
        .map_err(|e| GenerateError::VideoAudioExtractFailed(e.to_string()))?;

        emit(&app, ProgressEvent::ExtractAudio { percent: 100 });
        if cancel.is_cancelled() {
            return Err(GenerateError::Cancelled);
        }

        emit(
            &app,
            ProgressEvent::Asr {
                percent: 0,
                partial: None,
            },
        );

        let model_path = {
            let name = settings_loaded
                .last_model
                .clone()
                .or_else(|| models::first_present_model(&app))
                .ok_or_else(|| GenerateError::AsrFailed("no whisper model installed".into()))?;
            models::model_path(&app, &name).map_err(GenerateError::Other)?
        };

        let ctx = tokio::task::spawn_blocking({
            let mp = model_path.clone();
            move || load_context(&mp)
        })
        .await
        .context("spawn_blocking load_context")?
        .map_err(|e| GenerateError::AsrFailed(e.to_string()))?;

        let segments = tokio::task::spawn_blocking({
            let cancel2 = cancel.clone();
            move || transcribe(&ctx, &wav, &AsrOptions::default(), cancel2)
        })
        .await
        .context("spawn_blocking transcribe")?
        .map_err(|e| GenerateError::AsrFailed(e.to_string()))?;

        emit(
            &app,
            ProgressEvent::Asr {
                percent: 100,
                partial: None,
            },
        );
        if cancel.is_cancelled() {
            return Err(GenerateError::Cancelled);
        }

        let entries = segments_to_entries(&segments);
        if entries.is_empty() {
            return Err(GenerateError::AsrFailed("ASR produced no segments".into()));
        }

        let source_lang = segments.iter().find_map(|s| s.language.clone());
        (entries, source_lang)
    };

    let total = entries.len();
    emit(&app, ProgressEvent::Translate { current: 0, total });

    let provider = build_provider(&provider_cfg);
    let inputs: Vec<String> = entries.iter().map(|e| e.text.clone()).collect();

    let app_for_progress = app.clone();
    let translations = translate_in_chunks(
        provider.as_ref(),
        &inputs,
        &target_lang,
        source_lang.as_deref(),
        DEFAULT_CHUNK_SIZE,
        move |current, total| {
            emit(
                &app_for_progress,
                ProgressEvent::Translate { current, total },
            );
        },
    )
    .await
    .map_err(|e| match e {
        TranslateError::Cancelled => GenerateError::Cancelled,
        other => GenerateError::TranslateFailed(other.to_string()),
    })?;

    if translations.len() != entries.len() {
        return Err(GenerateError::TranslateFailed(format!(
            "expected {} translations, got {}",
            entries.len(),
            translations.len()
        )));
    }

    if cancel.is_cancelled() {
        return Err(GenerateError::Cancelled);
    }

    emit(&app, ProgressEvent::WriteOutput { done: false });

    let bilingual: Vec<BilingualEntry> = entries
        .iter()
        .zip(translations.iter())
        .map(|(e, t)| BilingualEntry {
            entry: e,
            translation: t.as_str(),
        })
        .collect();

    let rendered = render_bilingual(&bilingual);
    write_srt(
        &out_path,
        &rendered,
        crate::srt::parse::SourceEncoding::Utf8,
    )
    .map_err(|e| GenerateError::WriteFailed(e.to_string()))?;

    emit(&app, ProgressEvent::WriteOutput { done: true });

    tracing::info!(
        "generate done: {} entries, target={}, written to {}",
        entries.len(),
        target_lang,
        out_path.display()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_non_speech;

    #[test]
    fn detects_bracket_cues() {
        assert!(is_non_speech("[Music]"));
        assert!(is_non_speech("[ Screams ]"));
        assert!(is_non_speech("(applause)"));
        assert!(is_non_speech("【掌声】"));
        assert!(is_non_speech("♪ la la la ♪"));
        assert!(is_non_speech("（笑声）"));
    }

    #[test]
    fn keeps_real_speech() {
        assert!(!is_non_speech("Hello, world."));
        assert!(!is_non_speech("[1] this is the first item"));
        assert!(!is_non_speech("She said \"hi\"."));
    }

    #[test]
    fn empty_is_dropped() {
        assert!(is_non_speech(""));
        assert!(is_non_speech("   "));
    }
}
