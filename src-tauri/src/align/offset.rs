use std::collections::HashSet;
use std::time::Duration;

use crate::asr::whisper::Segment;
use crate::srt::parse::SrtEntry;

// ── Tokenization ──────────────────────────────────────────────────────────────

/// Tokenize text for similarity comparison.
/// - Latin: split on whitespace/punctuation, lowercase, drop empty
/// - CJK: individual characters
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut latin_buf = String::new();

    for ch in text.chars() {
        if is_cjk(ch) {
            if !latin_buf.is_empty() {
                flush_latin(&mut latin_buf, &mut tokens);
            }
            tokens.push(ch.to_string());
        } else if ch.is_alphanumeric() {
            latin_buf.push(ch.to_ascii_lowercase());
        } else {
            // punctuation / whitespace → word boundary
            if !latin_buf.is_empty() {
                flush_latin(&mut latin_buf, &mut tokens);
            }
        }
    }
    if !latin_buf.is_empty() {
        flush_latin(&mut latin_buf, &mut tokens);
    }
    tokens
}

fn is_cjk(ch: char) -> bool {
    matches!(ch,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified
        | '\u{3400}'..='\u{4DBF}' // CJK Ext A
        | '\u{20000}'..='\u{2A6DF}' // CJK Ext B
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{AC00}'..='\u{D7AF}' // Hangul
    )
}

fn flush_latin(buf: &mut String, out: &mut Vec<String>) {
    let t = buf.trim().to_string();
    if !t.is_empty() {
        out.push(t);
    }
    buf.clear();
}

// ── Jaccard similarity ────────────────────────────────────────────────────────

/// Normalised token Jaccard: |A ∩ B| / |A ∪ B|.
/// Returns 0.0 if both sets are empty.
pub fn jaccard(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let sa: HashSet<&str> = a.iter().map(String::as_str).collect();
    let sb: HashSet<&str> = b.iter().map(String::as_str).collect();
    let intersection = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    }
}

// ── Offset search ─────────────────────────────────────────────────────────────

const MIN_CONFIDENCE: f32 = 0.3;
/// Search range: ±300 s
const MAX_OFFSET_MS: i64 = 300_000;
/// Step size for sliding window (100 ms)
const STEP_MS: i64 = 100;

/// Result of an offset search.
#[derive(Debug, Clone)]
pub struct OffsetResult {
    /// The detected constant offset. Positive means srt is *early* (add this to each timestamp).
    pub offset: Duration,
    /// Whether offset should be added (+) or subtracted (-).
    pub positive: bool,
    pub confidence: f32,
}

/// Find the constant offset that best aligns `srt_entries` to `asr_segments`.
///
/// Both inputs are assumed to cover roughly the same audio window (e.g. 30-120 s).
/// Returns `None` when best confidence < MIN_CONFIDENCE.
pub fn find_offset(asr_segments: &[Segment], srt_entries: &[SrtEntry]) -> Option<OffsetResult> {
    if asr_segments.is_empty() || srt_entries.is_empty() {
        return None;
    }

    // Build ASR token bag per 1-second bucket for fast lookup
    // We slide the srt window across offsets and compute average Jaccard.

    let asr_text: String = asr_segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let srt_text: String = srt_entries
        .iter()
        .map(|e| e.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    // For each candidate offset, shift the srt timestamps and compute overlap
    // between ASR segments and (shifted) srt entries.
    let mut best_score = -1.0f32;
    let mut best_offset_ms: i64 = 0;

    for offset_ms in (-MAX_OFFSET_MS..=MAX_OFFSET_MS).step_by(STEP_MS as usize) {
        let score = score_at_offset(asr_segments, srt_entries, offset_ms);
        if score > best_score {
            best_score = score;
            best_offset_ms = offset_ms;
        }
    }

    // Refine within ±1 s of best with 10 ms steps
    let lo = (best_offset_ms - 1000).max(-MAX_OFFSET_MS);
    let hi = (best_offset_ms + 1000).min(MAX_OFFSET_MS);
    for offset_ms in (lo..=hi).step_by(10) {
        let score = score_at_offset(asr_segments, srt_entries, offset_ms);
        if score > best_score {
            best_score = score;
            best_offset_ms = offset_ms;
        }
    }

    tracing::debug!(
        "find_offset: best_offset={best_offset_ms}ms confidence={best_score:.3} asr_tokens={} srt_tokens={}",
        tokenize(&asr_text).len(),
        tokenize(&srt_text).len(),
    );

    if best_score < MIN_CONFIDENCE {
        return None;
    }

    let abs_ms = best_offset_ms.unsigned_abs();
    Some(OffsetResult {
        offset: Duration::from_millis(abs_ms),
        positive: best_offset_ms >= 0,
        confidence: best_score,
    })
}

/// Score the alignment when srt timestamps are shifted by `offset_ms`.
///
/// For each ASR segment, find srt entries that overlap after shifting and
/// compute Jaccard. Return the mean across all matched ASR segments.
fn score_at_offset(asr_segments: &[Segment], srt_entries: &[SrtEntry], offset_ms: i64) -> f32 {
    let mut total = 0.0f32;
    let mut count = 0usize;

    for seg in asr_segments {
        // Collect srt tokens that overlap with this ASR segment after applying offset
        let mut srt_tokens: Vec<String> = Vec::new();
        for entry in srt_entries {
            let shifted_start = entry.start.as_millis() as i64 + offset_ms;
            let shifted_end = entry.end.as_millis() as i64 + offset_ms;
            // Overlap check
            if shifted_end > seg.start_ms && shifted_start < seg.end_ms {
                srt_tokens.extend(tokenize(&entry.text));
            }
        }
        if srt_tokens.is_empty() {
            continue;
        }
        let asr_tokens = tokenize(&seg.text);
        let j = jaccard(&asr_tokens, &srt_tokens);
        total += j;
        count += 1;
    }

    if count == 0 {
        0.0
    } else {
        total / count as f32
    }
}

/// Apply `offset` to all SRT entries. Returns modified copies.
pub fn apply_offset(entries: &[SrtEntry], result: &OffsetResult) -> Vec<SrtEntry> {
    entries
        .iter()
        .map(|e| {
            let start = shift(e.start, result);
            let end = shift(e.end, result);
            SrtEntry {
                idx: e.idx,
                start,
                end,
                text: e.text.clone(),
            }
        })
        .collect()
}

fn shift(d: Duration, r: &OffsetResult) -> Duration {
    if r.positive {
        d.saturating_add(r.offset)
    } else {
        d.saturating_sub(r.offset)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_latin() {
        assert_eq!(tokenize("Hello, world!"), vec!["hello", "world"]);
    }

    #[test]
    fn tokenize_cjk() {
        assert_eq!(tokenize("你好，世界"), vec!["你", "好", "世", "界"]);
    }

    #[test]
    fn tokenize_mixed() {
        let t = tokenize("Hello 世界 world");
        assert_eq!(t, vec!["hello", "世", "界", "world"]);
    }

    #[test]
    fn jaccard_identical() {
        let a = tokenize("hello world");
        assert!((jaccard(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn jaccard_disjoint() {
        let a = tokenize("hello");
        let b = tokenize("world");
        assert!((jaccard(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn jaccard_partial() {
        let a = tokenize("hello world");
        let b = tokenize("hello there");
        // intersection=1 ("hello"), union=3
        let j = jaccard(&a, &b);
        assert!((j - 1.0 / 3.0).abs() < 1e-5, "got {j}");
    }

    #[test]
    fn find_offset_detects_plus3s() {
        // ASR segments: 3-8 s and 8-13 s
        // SRT is 3 s early: 0-5 s and 5-10 s
        // Correct offset = +3000 ms (add 3s to srt to align with ASR)
        let asr = vec![
            seg(3000, 8000, "the quick brown fox"),
            seg(8000, 13000, "jumps over the lazy dog"),
        ];
        let srt = vec![
            srt_entry(1, 0, 5000, "the quick brown fox"),
            srt_entry(2, 5000, 10000, "jumps over the lazy dog"),
        ];
        let result = find_offset(&asr, &srt).expect("should find offset");
        let off_ms = if result.positive {
            result.offset.as_millis() as i64
        } else {
            -(result.offset.as_millis() as i64)
        };
        assert!(
            (off_ms - 3000).abs() <= 500,
            "expected ~3000ms offset, got {off_ms}ms (confidence={:.3})",
            result.confidence
        );
    }

    #[test]
    fn find_offset_returns_none_when_low_confidence() {
        let asr = vec![seg(0, 5000, "completely unrelated text")];
        let srt = vec![srt_entry(
            1,
            0,
            5000,
            "无关内容 nothing in common here at all",
        )];
        // May or may not find offset but shouldn't panic
        let _ = find_offset(&asr, &srt);
    }

    fn seg(start_ms: i64, end_ms: i64, text: &str) -> Segment {
        Segment {
            start_ms,
            end_ms,
            text: text.to_string(),
            language: None,
        }
    }

    fn srt_entry(idx: u32, start_ms: u64, end_ms: u64, text: &str) -> SrtEntry {
        SrtEntry {
            idx,
            start: Duration::from_millis(start_ms),
            end: Duration::from_millis(end_ms),
            text: text.to_string(),
        }
    }
}
