/// Banded Needleman-Wunsch alignment of SRT token sequence vs Whisper word sequence.
///
/// Design:
/// - O(n·K) time and space, K = bandwidth (default 500).
/// - Token similarity uses normalised character overlap (Dice coefficient).
/// - Each SRT entry is represented by its token span in the concatenated SRT token list.
/// - After alignment, every SRT entry gets new start/end from the first/last matched ASR word.
/// - Entries with all-gap alignment fall back to linear interpolation from neighbours.
/// - Strictly monotone output: if a computed time regresses, the original is kept.
use std::time::Duration;

use crate::asr::whisper::Word;
use crate::srt::parse::SrtEntry;

// ── Token similarity (T5.2) ───────────────────────────────────────────────────

/// Dice coefficient on character bigrams: 2|A∩B| / (|A|+|B|).
/// Falls back to exact-match for very short strings.
pub fn token_similarity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    // For strings shorter than 2 chars, use prefix match
    if a.chars().count() < 2 || b.chars().count() < 2 {
        return if a == b { 1.0 } else { 0.0 };
    }
    let bigrams = |s: &str| -> std::collections::HashSet<[char; 2]> {
        let chars: Vec<char> = s.chars().collect();
        chars.windows(2).map(|w| [w[0], w[1]]).collect()
    };
    let ba = bigrams(a);
    let bb = bigrams(b);
    let intersection = ba.intersection(&bb).count();
    2.0 * intersection as f32 / (ba.len() + bb.len()) as f32
}

// ── NW scores ─────────────────────────────────────────────────────────────────

const MATCH_BONUS: f32 = 2.0; // applied on top of similarity
const MISMATCH_BASE: f32 = -1.0;
const GAP_PENALTY: f32 = -1.0;
/// Minimum similarity to count as a "match" rather than mismatch
const MATCH_THRESHOLD: f32 = 0.5;

#[inline]
fn score(a: &str, b: &str) -> f32 {
    let sim = token_similarity(a, b);
    if sim >= MATCH_THRESHOLD {
        sim * MATCH_BONUS
    } else {
        MISMATCH_BASE * (1.0 - sim)
    }
}

// ── Alignment cell ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Dir {
    Diag,
    Up,   // gap in B (ASR)
    Left, // gap in A (SRT)
}

// ── Banded NW (T5.1) ──────────────────────────────────────────────────────────

/// Alignment operation for one position.
#[derive(Debug, Clone)]
pub enum AlignOp {
    /// Matched/mismatched: srt token index, asr word index
    Match { srt: usize, asr: usize },
    /// Gap in SRT (ASR word has no SRT partner)
    GapSrt { asr: usize },
    /// Gap in ASR (SRT token has no ASR partner)
    GapAsr { srt: usize },
}

/// Run banded Needleman-Wunsch.
///
/// Returns the alignment as a sequence of `AlignOp`s from start to end.
pub fn align(srt_tokens: &[&str], asr_tokens: &[&str], bandwidth: usize) -> Vec<AlignOp> {
    let n = srt_tokens.len();
    let m = asr_tokens.len();
    if n == 0 || m == 0 {
        return Vec::new();
    }

    let k = bandwidth;
    // dp[i][j] only valid when |i-j| <= k
    // We store as flat 2-D with offset: column index in band = j - i + k
    let band_w = 2 * k + 1;

    // Allocate score + direction tables
    let mut dp = vec![vec![f32::NEG_INFINITY; band_w]; n + 1];
    let mut dir = vec![vec![Dir::Diag; band_w]; n + 1];

    // Initialise
    dp[0][k] = 0.0; // (0,0) → band offset = 0 - 0 + k = k
    for j in 1..=k.min(m) {
        let col = k + j; // j - 0 + k
        if col < band_w {
            dp[0][col] = GAP_PENALTY * j as f32;
            dir[0][col] = Dir::Left;
        }
    }
    for i in 1..=k.min(n) {
        let col = k - i; // 0 - i + k, band offset for j=0
        dp[i][col] = GAP_PENALTY * i as f32;
        dir[i][col] = Dir::Up;
    }

    for i in 1..=n {
        let j_lo = i.saturating_sub(k);
        let j_hi = (i + k).min(m);
        for j in j_lo..=j_hi {
            let col = (j as isize - i as isize + k as isize) as usize;

            // Diagonal: (i-1, j-1) → col unchanged
            let diag = if j > 0 {
                dp[i - 1][col] + score(srt_tokens[i - 1], asr_tokens[j - 1])
            } else {
                f32::NEG_INFINITY
            };

            // Up: (i-1, j) → col + 1
            let up = if col + 1 < band_w {
                dp[i - 1][col + 1] + GAP_PENALTY
            } else {
                f32::NEG_INFINITY
            };

            // Left: (i, j-1) → col - 1
            let left = if col > 0 {
                dp[i][col - 1] + GAP_PENALTY
            } else {
                f32::NEG_INFINITY
            };

            let (best, d) = if diag >= up && diag >= left {
                (diag, Dir::Diag)
            } else if up >= left {
                (up, Dir::Up)
            } else {
                (left, Dir::Left)
            };

            dp[i][col] = best;
            dir[i][col] = d;
        }
    }

    // Traceback from (n, m)
    let mut ops_rev = Vec::new();
    let mut i = n;
    let mut j = m;

    while i > 0 || j > 0 {
        let col = (j as isize - i as isize + k as isize) as usize;
        if i == 0 {
            ops_rev.push(AlignOp::GapSrt { asr: j - 1 });
            j -= 1;
        } else if j == 0 {
            ops_rev.push(AlignOp::GapAsr { srt: i - 1 });
            i -= 1;
        } else {
            match dir[i][col] {
                Dir::Diag => {
                    ops_rev.push(AlignOp::Match {
                        srt: i - 1,
                        asr: j - 1,
                    });
                    i -= 1;
                    j -= 1;
                }
                Dir::Up => {
                    ops_rev.push(AlignOp::GapAsr { srt: i - 1 });
                    i -= 1;
                }
                Dir::Left => {
                    ops_rev.push(AlignOp::GapSrt { asr: j - 1 });
                    j -= 1;
                }
            }
        }
    }

    ops_rev.reverse();
    ops_rev
}

// ── Time-code derivation (T5.3) ───────────────────────────────────────────────

/// New timing for one SRT entry derived from alignment.
#[derive(Debug, Clone)]
pub struct NewTiming {
    pub start: Duration,
    pub end: Duration,
    /// True if we found at least one matched ASR word for this entry.
    pub from_alignment: bool,
}

/// Given the alignment ops and the original SRT + ASR word lists, compute
/// new timecodes for every SRT entry.
///
/// `srt_token_spans[i]` = (start_token_idx, end_token_idx_exclusive) in the
/// concatenated SRT token list for entry i.
pub fn derive_timecodes(
    ops: &[AlignOp],
    asr_words: &[Word],
    srt_entries: &[SrtEntry],
    srt_token_spans: &[(usize, usize)],
) -> Vec<NewTiming> {
    let n = srt_entries.len();
    if n == 0 {
        return Vec::new();
    }

    // For each SRT token index, find the first/last matched ASR word index.
    // We'll accumulate per-entry.
    let mut entry_first_asr: Vec<Option<usize>> = vec![None; n];
    let mut entry_last_asr: Vec<Option<usize>> = vec![None; n];

    for op in ops {
        if let AlignOp::Match {
            srt: srt_tok,
            asr: asr_idx,
        } = op
        {
            // Find which SRT entry owns srt_tok
            let entry_idx = srt_token_spans.partition_point(|&(_, end)| end <= *srt_tok);
            if entry_idx < n {
                let first = entry_first_asr[entry_idx].get_or_insert(*asr_idx);
                *first = (*first).min(*asr_idx);
                let last = entry_last_asr[entry_idx].get_or_insert(*asr_idx);
                *last = (*last).max(*asr_idx);
            }
        }
    }

    // Build initial timings
    let mut timings: Vec<Option<NewTiming>> = vec![None; n];
    for (i, _entry) in srt_entries.iter().enumerate() {
        if let (Some(first), Some(last)) = (entry_first_asr[i], entry_last_asr[i]) {
            let start = Duration::from_millis(asr_words[first].start_ms.max(0) as u64);
            let end = Duration::from_millis(asr_words[last].end_ms.max(0) as u64);
            timings[i] = Some(NewTiming {
                start,
                end,
                from_alignment: true,
            });
        }
    }

    // Linear interpolation for all-gap entries
    interpolate_gaps(&mut timings, srt_entries);

    // Enforce monotone (start ≤ next start); fall back to original if violated
    enforce_monotone(&mut timings, srt_entries);

    timings
        .into_iter()
        .zip(srt_entries.iter())
        .map(|(t, e)| {
            t.unwrap_or(NewTiming {
                start: e.start,
                end: e.end,
                from_alignment: false,
            })
        })
        .collect()
}

fn interpolate_gaps(timings: &mut [Option<NewTiming>], entries: &[SrtEntry]) {
    let n = timings.len();
    let mut i = 0;
    while i < n {
        if timings[i].is_none() {
            // Find the gap run [i, j)
            let mut j = i + 1;
            while j < n && timings[j].is_none() {
                j += 1;
            }
            // prev anchor (ms)
            let prev_end = if i > 0 {
                timings[i - 1]
                    .as_ref()
                    .map(|t| t.end.as_millis() as i64)
                    .unwrap_or(entries[i - 1].end.as_millis() as i64)
            } else {
                0
            };
            // next anchor
            let next_start = if j < n {
                timings[j]
                    .as_ref()
                    .map(|t| t.start.as_millis() as i64)
                    .unwrap_or(entries[j].start.as_millis() as i64)
            } else {
                // use original duration of last entry as upper bound
                entries[n - 1].end.as_millis() as i64
            };
            let steps = (j - i + 1) as i64;
            let dur_per_step = (next_start - prev_end).max(0) / steps;
            for k in i..j {
                let offset = (k - i + 1) as i64;
                let s = prev_end + dur_per_step * offset;
                let orig_dur = (entries[k].end.as_millis() as i64
                    - entries[k].start.as_millis() as i64)
                    .max(100);
                let e = s + orig_dur;
                timings[k] = Some(NewTiming {
                    start: Duration::from_millis(s.max(0) as u64),
                    end: Duration::from_millis(e.max(s) as u64),
                    from_alignment: false,
                });
            }
            i = j;
        } else {
            i += 1;
        }
    }
}

fn enforce_monotone(timings: &mut [Option<NewTiming>], entries: &[SrtEntry]) {
    let n = timings.len();
    for i in 1..n {
        if let (Some(prev), Some(curr)) = (&timings[i - 1].clone(), &timings[i])
            && curr.start < prev.start
        {
            tracing::warn!("NW timecode regression at entry {i}: revert both to original");
            timings[i - 1] = Some(NewTiming {
                start: entries[i - 1].start,
                end: entries[i - 1].end,
                from_alignment: false,
            });
            timings[i] = Some(NewTiming {
                start: entries[i].start,
                end: entries[i].end,
                from_alignment: false,
            });
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Tokenize SRT entries into a flat token list + per-entry spans.
/// Re-uses `align::offset::tokenize` for consistency.
pub fn srt_to_tokens(entries: &[SrtEntry]) -> (Vec<String>, Vec<(usize, usize)>) {
    let mut tokens = Vec::new();
    let mut spans = Vec::new();
    for e in entries {
        let start = tokens.len();
        tokens.extend(crate::align::offset::tokenize(&e.text));
        spans.push((start, tokens.len()));
    }
    (tokens, spans)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_identical() {
        assert!((token_similarity("hello", "hello") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn similarity_empty() {
        assert_eq!(token_similarity("", "hello"), 0.0);
        assert_eq!(token_similarity("hello", ""), 0.0);
    }

    #[test]
    fn similarity_partial() {
        // "hello" vs "helo" share some bigrams
        let s = token_similarity("hello", "helo");
        assert!(s > 0.0 && s < 1.0, "got {s}");
    }

    #[test]
    fn align_identical_sequences() {
        let a = ["the", "quick", "brown", "fox"];
        let b = ["the", "quick", "brown", "fox"];
        let ops = align(&a, &b, 10);
        let matches = ops
            .iter()
            .filter(|o| matches!(o, AlignOp::Match { .. }))
            .count();
        assert_eq!(matches, 4);
    }

    #[test]
    fn align_insertion_deletion() {
        // SRT: "the brown fox"  ASR: "the quick brown fox"
        let srt = ["the", "brown", "fox"];
        let asr = ["the", "quick", "brown", "fox"];
        let ops = align(&srt, &asr, 10);
        // Should have 3 matches and 1 gap_srt (for "quick")
        let matches = ops
            .iter()
            .filter(|o| matches!(o, AlignOp::Match { .. }))
            .count();
        assert_eq!(matches, 3, "ops={ops:?}");
    }

    #[test]
    fn align_empty_inputs() {
        assert!(align(&[], &["a", "b"], 10).is_empty());
        assert!(align(&["a"], &[], 10).is_empty());
    }

    #[test]
    fn derive_timecodes_basic() {
        use std::time::Duration;

        let entries = vec![
            make_entry(1, 0, 2000, "hello world"),
            make_entry(2, 2000, 4000, "foo bar"),
        ];
        let words = vec![
            make_word(500, 800, "hello"),
            make_word(900, 1200, "world"),
            make_word(2100, 2400, "foo"),
            make_word(2500, 2800, "bar"),
        ];

        let (srt_tokens, spans) = srt_to_tokens(&entries);
        let srt_refs: Vec<&str> = srt_tokens.iter().map(String::as_str).collect();
        let asr_refs: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();

        let ops = align(&srt_refs, &asr_refs, 50);
        let timings = derive_timecodes(&ops, &words, &entries, &spans);

        assert_eq!(timings.len(), 2);
        assert!(
            timings[0].from_alignment,
            "entry 0 should be from alignment"
        );
        assert_eq!(timings[0].start, Duration::from_millis(500));
        assert_eq!(timings[0].end, Duration::from_millis(1200));
        assert!(timings[1].from_alignment);
        assert_eq!(timings[1].start, Duration::from_millis(2100));
    }

    #[test]
    fn enforce_monotone_reverts() {
        // Two entries where NW produces a regression
        let entries = vec![
            make_entry(1, 1000, 2000, "hello"),
            make_entry(2, 3000, 4000, "world"),
        ];
        let words = vec![
            make_word(5000, 5500, "hello"), // NW places entry 1 late
            make_word(4000, 4500, "world"), // NW places entry 2 earlier than entry 1 → regression
        ];

        let (srt_tokens, spans) = srt_to_tokens(&entries);
        let srt_refs: Vec<&str> = srt_tokens.iter().map(String::as_str).collect();
        let asr_refs: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();

        let ops = align(&srt_refs, &asr_refs, 50);
        let timings = derive_timecodes(&ops, &words, &entries, &spans);

        // The second timing must not start before the first
        assert!(
            timings[1].start >= timings[0].start,
            "monotone violated: {:?} >= {:?}",
            timings[1].start,
            timings[0].start
        );
    }

    fn make_entry(idx: u32, start_ms: u64, end_ms: u64, text: &str) -> SrtEntry {
        SrtEntry {
            idx,
            start: Duration::from_millis(start_ms),
            end: Duration::from_millis(end_ms),
            text: text.to_string(),
        }
    }

    fn make_word(start_ms: i64, end_ms: i64, text: &str) -> Word {
        Word {
            start_ms,
            end_ms,
            text: text.to_string(),
        }
    }
}
