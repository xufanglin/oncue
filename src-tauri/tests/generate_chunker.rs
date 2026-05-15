/// Integration test for the chunked translate driver with a mock provider.
///
/// Verifies:
///   1. translate_in_chunks splits input into chunks and reassembles in order.
///   2. Output count matches input count even when a chunk needs retries.
///   3. Single-line fallback kicks in if a chunk repeatedly fails.
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use oncue_lib::translate::chunker::translate_in_chunks;
use oncue_lib::translate::{TranslateContext, TranslateError, TranslateProvider};

// ── Mock providers ────────────────────────────────────────────────────────────

/// Always succeeds. Returns each input prefixed with "T:".
struct OkProvider;

#[async_trait]
impl TranslateProvider for OkProvider {
    async fn translate(
        &self,
        chunk: &[String],
        _ctx: &TranslateContext,
    ) -> Result<Vec<String>, TranslateError> {
        Ok(chunk.iter().map(|s| format!("T:{s}")).collect())
    }
    async fn ping(&self) -> Result<(), TranslateError> {
        Ok(())
    }
}

/// Fails the first N attempts (regardless of chunk), then succeeds.
struct FlakyProvider {
    fail_until: usize,
    calls: AtomicUsize,
}

#[async_trait]
impl TranslateProvider for FlakyProvider {
    async fn translate(
        &self,
        chunk: &[String],
        _ctx: &TranslateContext,
    ) -> Result<Vec<String>, TranslateError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fail_until {
            return Err(TranslateError::Malformed("simulated".into()));
        }
        Ok(chunk.iter().map(|s| format!("T:{s}")).collect())
    }
    async fn ping(&self) -> Result<(), TranslateError> {
        Ok(())
    }
}

/// Fails on multi-line chunks, succeeds on single-line. Forces fallback.
struct OnlySingleLineProvider;

#[async_trait]
impl TranslateProvider for OnlySingleLineProvider {
    async fn translate(
        &self,
        chunk: &[String],
        _ctx: &TranslateContext,
    ) -> Result<Vec<String>, TranslateError> {
        if chunk.len() > 1 {
            return Err(TranslateError::Malformed("only single line".into()));
        }
        Ok(chunk.iter().map(|s| format!("T:{s}")).collect())
    }
    async fn ping(&self) -> Result<(), TranslateError> {
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

fn inputs(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("line {i}")).collect()
}

#[tokio::test]
async fn chunked_basic_count_preserved() {
    let p = OkProvider;
    let lines = inputs(75);
    let out = translate_in_chunks(&p, &lines, "中文", Some("English"), 30, |_, _| {})
        .await
        .unwrap();
    assert_eq!(out.len(), 75);
    for (i, s) in out.iter().enumerate() {
        assert_eq!(s, &format!("T:line {i}"));
    }
}

#[tokio::test]
async fn chunked_retries_recovers() {
    // First two attempts fail, then succeed. Chunk size 5 over 5 inputs = 1 chunk
    // with retries.
    let p = FlakyProvider {
        fail_until: 2,
        calls: AtomicUsize::new(0),
    };
    let lines = inputs(5);
    let out = translate_in_chunks(&p, &lines, "中文", None, 5, |_, _| {})
        .await
        .unwrap();
    assert_eq!(out.len(), 5);
    assert_eq!(out[0], "T:line 0");
}

#[tokio::test]
async fn chunked_falls_back_to_single_line() {
    // Multi-line always fails; single-line succeeds. Driver must fall back.
    let p = OnlySingleLineProvider;
    let lines = inputs(7);
    let out = translate_in_chunks(&p, &lines, "中文", None, 5, |_, _| {})
        .await
        .unwrap();
    assert_eq!(out.len(), 7);
    for (i, s) in out.iter().enumerate() {
        assert_eq!(s, &format!("T:line {i}"));
    }
}

#[tokio::test]
async fn chunked_progress_reports_total_and_advances() {
    use std::sync::Mutex;
    let p = OkProvider;
    let lines = inputs(60);
    let progress: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
    let _ = translate_in_chunks(&p, &lines, "中文", None, 20, |c, t| {
        progress.lock().unwrap().push((c, t));
    })
    .await
    .unwrap();
    let p = progress.lock().unwrap();
    // Three chunks of 20 → three progress callbacks, all with total = 60.
    assert_eq!(p.len(), 3);
    assert_eq!(p[0], (20, 60));
    assert_eq!(p[1], (40, 60));
    assert_eq!(p[2], (60, 60));
}
