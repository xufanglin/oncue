use anyhow::{Result, anyhow};
use tokio_util::sync::CancellationToken;

use super::{TranslateContext, TranslateError, TranslateProvider};

// ── Tunables ──────────────────────────────────────────────────────────────────

pub const DEFAULT_CHUNK_SIZE: usize = 30;
pub const MAX_CHUNK_RETRIES: u32 = 3;
pub const HISTORY_LINES: usize = 3;

// ── Prompt construction ───────────────────────────────────────────────────────

/// Build (system, user) prompts for a numbered translation chunk.
///
/// The model is instructed to return one line per input, prefixed with
/// `[[N]]` numbering matching the input. This makes the boundaries machine-
/// parseable and lets us detect missing/merged lines.
pub fn build_prompt(chunk: &[String], ctx: &TranslateContext) -> (String, String) {
    let target = if ctx.target_lang.is_empty() {
        "Chinese (Simplified)".to_string()
    } else {
        ctx.target_lang.clone()
    };

    let mut system = String::new();
    system.push_str("You are a professional subtitle translator. ");
    if let Some(src) = &ctx.source_lang {
        system.push_str(&format!("Translate from {src} to {target}. "));
    } else {
        system.push_str(&format!("Translate the input into {target}. "));
    }
    system.push_str(
        "Each input line begins with a marker like [[N]]. \
         Return EXACTLY one output line per input, prefixed with the SAME [[N]] marker, \
         in the same order. Do NOT merge lines. Do NOT skip lines. \
         Do NOT add commentary. Preserve proper nouns where appropriate.",
    );

    let mut user = String::new();
    if !ctx.history.is_empty() {
        user.push_str("Previous lines (for context, do not translate):\n");
        for h in ctx.history.iter().take(HISTORY_LINES) {
            user.push_str(&format!("  {h}\n"));
        }
        user.push('\n');
    }
    user.push_str("Translate these lines:\n");
    for (i, line) in chunk.iter().enumerate() {
        let n = i + 1;
        user.push_str(&format!("[[{n}]] {line}\n"));
    }

    (system, user)
}

// ── Response parsing ──────────────────────────────────────────────────────────

/// Parse a `[[N]]`-numbered response. Returns the lines indexed 1..=expected,
/// in input order. Errors if any line is missing or duplicated.
pub fn parse_numbered_response(text: &str, expected: usize) -> Result<Vec<String>> {
    let mut out: Vec<Option<String>> = vec![None; expected];

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let Some((idx, rest)) = parse_marker(line) else {
            continue;
        };
        if idx < 1 || idx > expected {
            continue;
        }
        let slot = &mut out[idx - 1];
        if slot.is_none() {
            *slot = Some(rest.trim().to_string());
        }
    }

    let mut result = Vec::with_capacity(expected);
    for (i, opt) in out.into_iter().enumerate() {
        match opt {
            Some(s) if !s.is_empty() => result.push(s),
            _ => return Err(anyhow!("missing line {} in response", i + 1)),
        }
    }
    Ok(result)
}

/// Parse `[[N]] body` → `(N, body)`. Returns None if marker not at start.
fn parse_marker(line: &str) -> Option<(usize, &str)> {
    let rest = line.strip_prefix("[[")?;
    let close = rest.find("]]")?;
    let n: usize = rest[..close].trim().parse().ok()?;
    Some((n, &rest[close + 2..]))
}

// ── Chunked translate driver ──────────────────────────────────────────────────

/// Split `entries` into fixed-size chunks and translate each via `provider`.
/// On a chunk failure (parse error or provider error), retry up to
/// `MAX_CHUNK_RETRIES`. After that, fall back to single-line translation
/// (still numbered, chunk size 1).
///
/// `cancel` is checked after every chunk; returns `TranslateError::Cancelled`
/// if triggered.
pub async fn translate_in_chunks(
    provider: &dyn TranslateProvider,
    entries: &[String],
    target_lang: &str,
    source_lang: Option<&str>,
    chunk_size: usize,
    mut on_progress: impl FnMut(usize, usize),
    cancel: CancellationToken,
) -> Result<Vec<String>, TranslateError> {
    let chunk_size = chunk_size.max(1);
    let mut out: Vec<String> = Vec::with_capacity(entries.len());
    let total = entries.len();

    for window in entries.chunks(chunk_size) {
        if cancel.is_cancelled() {
            return Err(TranslateError::Cancelled);
        }

        let history: Vec<String> = out
            .iter()
            .rev()
            .take(HISTORY_LINES)
            .rev()
            .cloned()
            .collect();

        let ctx = TranslateContext {
            source_lang: source_lang.map(String::from),
            target_lang: target_lang.to_string(),
            history,
        };

        let chunk_vec: Vec<String> = window.to_vec();
        match try_chunk(provider, &chunk_vec, &ctx, MAX_CHUNK_RETRIES).await {
            Ok(translated) => out.extend(translated),
            // Rate limit errors propagate immediately — single-line fallback
            // would be throttled too, and the caller should surface the error.
            Err(e @ TranslateError::RateLimited) => return Err(e),
            Err(_) => {
                // fallback: single-line for malformed/parse failures
                for line in &chunk_vec {
                    if cancel.is_cancelled() {
                        return Err(TranslateError::Cancelled);
                    }
                    let single = vec![line.clone()];
                    let translated = try_chunk(provider, &single, &ctx, MAX_CHUNK_RETRIES).await?;
                    out.extend(translated);
                }
            }
        }
        on_progress(out.len(), total);
    }

    Ok(out)
}

async fn try_chunk(
    provider: &dyn TranslateProvider,
    chunk: &[String],
    ctx: &TranslateContext,
    retries: u32,
) -> Result<Vec<String>, TranslateError> {
    let mut last_err: Option<TranslateError> = None;
    for attempt in 0..=retries {
        match provider.translate(chunk, ctx).await {
            Ok(v) if v.len() == chunk.len() => return Ok(v),
            Ok(v) => {
                last_err = Some(TranslateError::Malformed(format!(
                    "expected {} lines, got {}",
                    chunk.len(),
                    v.len()
                )));
            }
            Err(e) => last_err = Some(e),
        }
        if attempt < retries {
            let backoff_ms = if matches!(last_err, Some(TranslateError::RateLimited)) {
                // 429: wait 5s, 15s, 45s — well beyond API rate limit reset windows
                5_000u64 * (1 << attempt.min(2))
            } else {
                200u64 * (1 << attempt)
            };
            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
        }
    }
    Err(last_err.unwrap_or_else(|| TranslateError::Malformed("unknown chunk failure".into())))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_marker_basic() {
        assert_eq!(parse_marker("[[1]] hello"), Some((1, " hello")));
        assert_eq!(parse_marker("[[42]]world"), Some((42, "world")));
        assert_eq!(parse_marker("plain text"), None);
    }

    #[test]
    fn parse_response_ok() {
        let text = "[[1]] hello\n[[2]] world";
        let out = parse_numbered_response(text, 2).unwrap();
        assert_eq!(out, vec!["hello", "world"]);
    }

    #[test]
    fn parse_response_missing_errors() {
        let text = "[[1]] hello";
        assert!(parse_numbered_response(text, 2).is_err());
    }

    #[test]
    fn parse_response_ignores_garbage_lines() {
        let text = "Sure! Here are translations:\n[[1]] hi\n[[2]] there\nDone.";
        let out = parse_numbered_response(text, 2).unwrap();
        assert_eq!(out, vec!["hi", "there"]);
    }

    #[test]
    fn parse_response_duplicate_keeps_first() {
        let text = "[[1]] first\n[[1]] second\n[[2]] x";
        let out = parse_numbered_response(text, 2).unwrap();
        assert_eq!(out, vec!["first", "x"]);
    }

    #[test]
    fn build_prompt_includes_markers_and_target() {
        let chunk = vec!["hello".to_string(), "world".to_string()];
        let ctx = TranslateContext {
            source_lang: Some("English".into()),
            target_lang: "中文".into(),
            history: vec![],
        };
        let (sys, user) = build_prompt(&chunk, &ctx);
        assert!(sys.contains("中文"));
        assert!(user.contains("[[1]] hello"));
        assert!(user.contains("[[2]] world"));
    }

    // ── Cancellation ──────────────────────────────────────────────────────────

    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl TranslateProvider for CountingProvider {
        async fn translate(
            &self,
            chunk: &[String],
            _ctx: &TranslateContext,
        ) -> Result<Vec<String>, TranslateError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(chunk.iter().map(|s| format!("T:{s}")).collect())
        }
        async fn ping(&self) -> Result<(), TranslateError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn cancel_before_first_chunk_returns_immediately() {
        let p = CountingProvider {
            calls: AtomicUsize::new(0),
        };
        let lines: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let cancel = CancellationToken::new();
        cancel.cancel(); // already cancelled

        let res = translate_in_chunks(&p, &lines, "中文", None, 10, |_, _| {}, cancel).await;

        assert!(matches!(res, Err(TranslateError::Cancelled)));
        // Provider must not be called at all.
        assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancel_between_chunks_stops_progress() {
        // Cancel after the first chunk completes — second chunk must not run.
        let p = CountingProvider {
            calls: AtomicUsize::new(0),
        };
        let lines: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        let res = translate_in_chunks(
            &p,
            &lines,
            "中文",
            None,
            10,
            move |current, _total| {
                // Trip the token after first chunk's progress callback fires.
                if current >= 10 {
                    cancel_clone.cancel();
                }
            },
            cancel,
        )
        .await;

        assert!(matches!(res, Err(TranslateError::Cancelled)));
        // First chunk ran (1 call); second chunk should be skipped.
        assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_propagates_without_fallback() {
        // Verify that RateLimited error is NOT swallowed by single-line fallback.
        // `start_paused` skips the 5s/15s/45s backoff sleeps via tokio's virtual clock.
        struct RateLimitedProvider;
        #[async_trait]
        impl TranslateProvider for RateLimitedProvider {
            async fn translate(
                &self,
                _chunk: &[String],
                _ctx: &TranslateContext,
            ) -> Result<Vec<String>, TranslateError> {
                Err(TranslateError::RateLimited)
            }
            async fn ping(&self) -> Result<(), TranslateError> {
                Ok(())
            }
        }

        let p = RateLimitedProvider;
        let lines: Vec<String> = (0..3).map(|i| format!("line {i}")).collect();
        let res = translate_in_chunks(
            &p,
            &lines,
            "中文",
            None,
            5,
            |_, _| {},
            CancellationToken::new(),
        )
        .await;

        assert!(matches!(res, Err(TranslateError::RateLimited)));
    }
}
