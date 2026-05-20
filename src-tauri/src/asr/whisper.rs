use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

// ── Public output types ───────────────────────────────────────────────────────

/// A transcribed segment (sentence / phrase level).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    /// Start time in milliseconds.
    pub start_ms: i64,
    /// End time in milliseconds.
    pub end_ms: i64,
    pub text: String,
    /// ISO-639-1 code detected by Whisper, e.g. "en", "zh". Populated on first segment only.
    pub language: Option<String>,
}

/// A transcribed word with individual timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Word {
    /// Start time in milliseconds.
    pub start_ms: i64,
    /// End time in milliseconds.
    pub end_ms: i64,
    pub text: String,
}

// ── Options ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct AsrOptions {
    /// ISO-639-1 language hint. None = auto-detect.
    pub language: Option<String>,
    /// Number of threads to use. 0 = let whisper.cpp choose.
    pub n_threads: usize,
}

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum AsrError {
    #[error("invalid sample rate: expected 16000 Hz, got {0}")]
    InvalidSampleRate(u32),
    #[error("transcription cancelled")]
    Cancelled,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// ── PCM helpers ───────────────────────────────────────────────────────────────

/// Convert a raw signed 16-bit little-endian PCM byte buffer (16 kHz mono,
/// as produced by `ffmpeg::extract_audio`) to f32 samples in [-1.0, 1.0].
pub fn wav_to_pcm(pcm_bytes: &[u8]) -> Result<Vec<f32>, AsrError> {
    if !pcm_bytes.len().is_multiple_of(2) {
        return Err(AsrError::Other(anyhow::anyhow!(
            "pcm buffer length {} is not a multiple of 2 bytes",
            pcm_bytes.len()
        )));
    }
    let scale = 1.0_f32 / i16::MAX as f32;
    let samples = pcm_bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 * scale)
        .collect();
    Ok(samples)
}

// ── Context loading ───────────────────────────────────────────────────────────

/// Detect the active backend by inspecting whisper.cpp's system info string.
/// Returns "coreml", "metal", or "cpu".
fn detect_backend() -> &'static str {
    let info = whisper_rs::print_system_info();
    if info.contains("COREML = 1") {
        "coreml"
    } else if info.contains("METAL = 1") {
        "metal"
    } else {
        "cpu"
    }
}

/// Load a Whisper context from `model_path`, preferring CoreML → Metal → CPU.
pub fn load_context(model_path: &Path) -> Result<Arc<WhisperContext>> {
    let mut params = WhisperContextParameters::new();
    params.use_gpu(true); // enables Metal / CoreML when available

    let ctx = WhisperContext::new_with_params(model_path, params)
        .with_context(|| format!("failed to load whisper model at {}", model_path.display()))?;

    let backend = detect_backend();
    tracing::info!("asr backend: {backend}");

    Ok(Arc::new(ctx))
}

// ── Transcription helpers ─────────────────────────────────────────────────────

fn build_params<'a>(opts: &'a AsrOptions) -> FullParams<'a, 'a> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    params.set_print_timestamps(false);

    let n_threads = if opts.n_threads > 0 {
        opts.n_threads as i32
    } else {
        (std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(8)) as i32
    };
    params.set_n_threads(n_threads);

    match &opts.language {
        Some(lang) if lang != "auto" => {
            params.set_language(Some(lang.as_str()));
        }
        _ => {
            params.set_language(Some("auto"));
        }
    }
    params
}

/// Centiseconds (whisper internal unit) → milliseconds.
#[inline]
fn cs_to_ms(cs: i64) -> i64 {
    cs * 10
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Segment-level transcription. Returns one `Segment` per whisper segment.
pub fn transcribe(
    ctx: &WhisperContext,
    wav_bytes: &[u8],
    opts: &AsrOptions,
    cancel: CancellationToken,
) -> Result<Vec<Segment>, AsrError> {
    let pcm = wav_to_pcm(wav_bytes)?;

    let params = build_params(opts);
    // NB: we intentionally do NOT use set_abort_callback_safe — its trampoline
    // takes a Mutex that the Metal GPU thread cannot lock, causing
    // "failed to lock mutex: Invalid argument" aborts mid-transcription.
    // Cancellation is checked at stage boundaries instead.

    let mut state = ctx
        .create_state()
        .map_err(|e| AsrError::Other(anyhow::anyhow!("create_state: {e}")))?;

    state
        .full(params, &pcm)
        .map_err(|e| AsrError::Other(anyhow::anyhow!("whisper full: {e}")))?;

    if cancel.is_cancelled() {
        return Err(AsrError::Cancelled);
    }

    let n_segments = state.full_n_segments();
    let lang_id = state.full_lang_id_from_state();
    let detected_lang = whisper_rs::get_lang_str(lang_id).map(str::to_string);

    let mut segments = Vec::with_capacity(n_segments as usize);
    for i in 0..n_segments {
        let seg = state
            .get_segment(i)
            .ok_or_else(|| AsrError::Other(anyhow::anyhow!("segment {i} out of bounds")))?;

        segments.push(Segment {
            start_ms: cs_to_ms(seg.start_timestamp()),
            end_ms: cs_to_ms(seg.end_timestamp()),
            text: seg
                .to_str_lossy()
                .map(|s| s.into_owned())
                .unwrap_or_default(),
            language: if i == 0 { detected_lang.clone() } else { None },
        });
    }

    Ok(segments)
}

/// Word-level transcription using token timestamps.
pub fn transcribe_words(
    ctx: &WhisperContext,
    wav_bytes: &[u8],
    opts: &AsrOptions,
    cancel: CancellationToken,
) -> Result<Vec<Word>, AsrError> {
    let pcm = wav_to_pcm(wav_bytes)?;

    let mut params = build_params(opts);
    params.set_token_timestamps(true);
    params.set_split_on_word(true);
    // See note in `transcribe`: avoid set_abort_callback_safe on Metal.

    let mut state = ctx
        .create_state()
        .map_err(|e| AsrError::Other(anyhow::anyhow!("create_state: {e}")))?;

    state
        .full(params, &pcm)
        .map_err(|e| AsrError::Other(anyhow::anyhow!("whisper full: {e}")))?;

    if cancel.is_cancelled() {
        return Err(AsrError::Cancelled);
    }

    let n_segments = state.full_n_segments();
    let eot = ctx.token_eot();

    let mut words: Vec<Word> = Vec::new();
    for i in 0..n_segments {
        let seg = state
            .get_segment(i)
            .ok_or_else(|| AsrError::Other(anyhow::anyhow!("segment {i} out of bounds")))?;

        let n_tokens = seg.n_tokens();
        for j in 0..n_tokens {
            let tok = seg
                .get_token(j)
                .ok_or_else(|| AsrError::Other(anyhow::anyhow!("token {j} out of bounds")))?;

            // Skip special tokens
            if tok.token_id() >= eot {
                continue;
            }

            let text = match tok.to_str_lossy() {
                Ok(t) => t.into_owned(),
                Err(_) => continue,
            };
            let text = text.trim().to_string();
            if text.is_empty() {
                continue;
            }

            let data = tok.token_data();
            let start_ms = cs_to_ms(data.t0);
            let end_ms = cs_to_ms(data.t1);

            // Merge into previous word if timestamps indicate continuation of same word
            if let Some(prev) = words.last_mut()
                && start_ms <= prev.end_ms
                && !text.starts_with(' ')
            {
                prev.text.push_str(&text);
                prev.end_ms = end_ms.max(prev.end_ms);
                continue;
            }

            words.push(Word {
                start_ms,
                end_ms,
                text,
            });
        }
    }

    Ok(words)
}
