use anyhow::{Context, Result, bail};
use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

// ── Data types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SrtEntry {
    pub idx: u32,
    pub start: Duration,
    pub end: Duration,
    pub text: String,
}

/// Source encoding detected when reading the file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SourceEncoding {
    Utf8,
    Other(&'static Encoding),
}

// ── Encoding detection ────────────────────────────────────────────────────────

/// Detect encoding and decode bytes to UTF-8.
/// Returns (utf8_string, detected_encoding).
pub fn decode_bytes(raw: &[u8]) -> Result<(String, SourceEncoding)> {
    // Strip UTF-8 BOM if present
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);

    // Try strict UTF-8 first
    if let Ok(s) = std::str::from_utf8(raw) {
        return Ok((s.to_string(), SourceEncoding::Utf8));
    }

    // Use chardetng for charset detection
    let mut det = EncodingDetector::new(Iso2022JpDetection::Allow);
    det.feed(raw, true);
    let encoding = det.guess(None, Utf8Detection::Deny);

    let (cow, enc_used, had_errors) = encoding.decode(raw);
    if had_errors {
        bail!(
            "decoding with {} produced errors; file may be corrupted",
            enc_used.name()
        );
    }
    Ok((cow.into_owned(), SourceEncoding::Other(enc_used)))
}

/// Load raw bytes from a file, detect encoding, return (utf8_text, encoding).
pub fn load_file(path: &Path) -> Result<(String, SourceEncoding)> {
    let raw = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    decode_bytes(&raw).with_context(|| format!("encoding detection failed for {}", path.display()))
}

// ── Timestamp parsing ─────────────────────────────────────────────────────────

/// Parse `HH:MM:SS,mmm` or `HH:MM:SS.mmm` → Duration.
pub fn parse_timestamp(s: &str) -> Result<Duration> {
    let s = s.trim().replace('.', ",");
    let parts: Vec<&str> = s.splitn(2, ',').collect();
    let ms: u64 = if parts.len() == 2 {
        parts[1].parse().unwrap_or(0)
    } else {
        0
    };
    let hms: Vec<&str> = parts[0].splitn(3, ':').collect();
    if hms.len() != 3 {
        bail!("invalid timestamp: '{s}'");
    }
    let h: u64 = hms[0].trim().parse().context("bad hours")?;
    let m: u64 = hms[1].trim().parse().context("bad minutes")?;
    let sec: u64 = hms[2].trim().parse().context("bad seconds")?;
    Ok(Duration::from_millis(
        h * 3_600_000 + m * 60_000 + sec * 1_000 + ms,
    ))
}

// ── SRT parser ────────────────────────────────────────────────────────────────

/// Parse UTF-8 SRT text into a list of entries.
pub fn parse_str(text: &str) -> Result<Vec<SrtEntry>> {
    let mut entries = Vec::new();
    // Split on blank-line separators; handle both CRLF and LF
    let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
    let blocks: Vec<&str> = normalised
        .split("\n\n")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .collect();

    for (block_num, block) in blocks.iter().enumerate() {
        let lines: Vec<&str> = block.lines().collect();
        if lines.len() < 3 {
            // Allow blocks with only index + timecode and no text (rare but valid)
            if lines.len() < 2 {
                continue;
            }
        }

        // Line 0: index
        let idx: u32 = lines[0].trim().parse().with_context(|| {
            format!(
                "block {block_num}: expected numeric index, got '{}'",
                lines[0]
            )
        })?;

        // Line 1: timecodes "HH:MM:SS,mmm --> HH:MM:SS,mmm"
        let tc_line = lines[1];
        let arrow = tc_line.find("-->").with_context(|| {
            format!("block {block_num}: missing '-->' in timecode line '{tc_line}'")
        })?;
        let start = parse_timestamp(&tc_line[..arrow])?;
        let end = parse_timestamp(&tc_line[arrow + 3..])?;

        // Lines 2+: text (may contain HTML tags, keep as-is)
        let text = lines[2..].join("\n");

        entries.push(SrtEntry {
            idx,
            start,
            end,
            text,
        });
    }

    Ok(entries)
}

/// Convenience: load file, detect encoding, dispatch parser by extension.
/// `.ass` / `.ssa` go through the ASS parser; everything else is treated as
/// SRT. The result type is `SrtEntry` regardless — ASS styling is dropped.
pub fn parse_file(path: &Path) -> Result<(Vec<SrtEntry>, SourceEncoding)> {
    let (text, enc) = load_file(path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    let entries = match ext.as_deref() {
        Some("ass") | Some("ssa") => super::ass::parse_str(&text)
            .with_context(|| format!("ASS parse failed for {}", path.display()))?,
        _ => {
            parse_str(&text).with_context(|| format!("SRT parse failed for {}", path.display()))?
        }
    };
    Ok((entries, enc))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const BASIC: &str = "\
1
00:00:01,000 --> 00:00:03,500
Hello, world!

2
00:00:04,000 --> 00:00:06,000
Second line.
";

    const CRLF: &str = "1\r\n00:00:01,000 --> 00:00:03,500\r\nHello\r\n\r\n2\r\n00:00:04,000 --> 00:00:06,000\r\nWorld\r\n";

    #[test]
    fn parse_basic() {
        let entries = parse_str(BASIC).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].idx, 1);
        assert_eq!(entries[0].start, Duration::from_millis(1000));
        assert_eq!(entries[0].end, Duration::from_millis(3500));
        assert_eq!(entries[0].text, "Hello, world!");
        assert_eq!(entries[1].text, "Second line.");
    }

    #[test]
    fn parse_crlf() {
        let entries = parse_str(CRLF).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "Hello");
        assert_eq!(entries[1].text, "World");
    }

    #[test]
    fn parse_timestamp_variants() {
        assert_eq!(
            parse_timestamp("01:23:45,678").unwrap(),
            Duration::from_millis(5025678)
        );
        assert_eq!(
            parse_timestamp("01:23:45.678").unwrap(),
            Duration::from_millis(5025678)
        );
        assert_eq!(parse_timestamp("00:00:00,000").unwrap(), Duration::ZERO);
    }

    #[test]
    fn parse_multiline_text() {
        let srt = "1\n00:00:01,000 --> 00:00:03,000\nLine one\nLine two\n\n";
        let entries = parse_str(srt).unwrap();
        assert_eq!(entries[0].text, "Line one\nLine two");
    }

    #[test]
    fn parse_special_chars() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\n<i>Italic</i> & \"quoted\"\n\n";
        let entries = parse_str(srt).unwrap();
        assert_eq!(entries[0].text, "<i>Italic</i> & \"quoted\"");
    }

    #[test]
    fn decode_utf8() {
        let raw = "hello UTF-8 世界".as_bytes();
        let (s, enc) = decode_bytes(raw).unwrap();
        assert_eq!(s, "hello UTF-8 世界");
        assert_eq!(enc, SourceEncoding::Utf8);
    }

    #[test]
    fn decode_utf8_bom() {
        let mut raw = vec![0xEF, 0xBB, 0xBF];
        raw.extend_from_slice("hello".as_bytes());
        let (s, enc) = decode_bytes(&raw).unwrap();
        assert_eq!(s, "hello");
        assert_eq!(enc, SourceEncoding::Utf8);
    }
}
