use std::fmt::Write;
use std::time::Duration;

use super::parse::SrtEntry;

// ── Timestamp formatting ──────────────────────────────────────────────────────

/// Format a Duration as `HH:MM:SS,mmm`.
pub fn format_timestamp(d: Duration) -> String {
    let total_ms = d.as_millis() as u64;
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let secs = total_secs % 60;
    let total_mins = total_secs / 60;
    let mins = total_mins % 60;
    let hours = total_mins / 60;
    format!("{hours:02}:{mins:02}:{secs:02},{ms:03}")
}

// ── Mono render ───────────────────────────────────────────────────────────────

/// Render entries to standard SRT text.
pub fn render(entries: &[SrtEntry]) -> String {
    let mut out = String::with_capacity(entries.len() * 80);
    for (i, e) in entries.iter().enumerate() {
        let idx = i as u32 + 1; // re-number sequentially
        let _ = writeln!(
            out,
            "{idx}\n{} --> {}\n{}\n",
            format_timestamp(e.start),
            format_timestamp(e.end),
            e.text.trim()
        );
    }
    out
}

// ── Bilingual render ──────────────────────────────────────────────────────────

/// Bilingual entry: `primary` is shown on top, `secondary` below.
pub struct BilingualEntry<'a> {
    pub entry: &'a SrtEntry,
    /// Translation text (empty string = omit secondary line).
    pub translation: &'a str,
}

/// Render bilingual SRT: primary (e.g. zh) on top, secondary (e.g. en) below.
pub fn render_bilingual(entries: &[BilingualEntry<'_>]) -> String {
    let mut out = String::with_capacity(entries.len() * 120);
    for (i, b) in entries.iter().enumerate() {
        let idx = i as u32 + 1;
        let primary = b.entry.text.trim();
        let secondary = b.translation.trim();

        let text = if secondary.is_empty() {
            primary.to_string()
        } else {
            format!("{primary}\n{secondary}")
        };

        let _ = writeln!(
            out,
            "{idx}\n{} --> {}\n{}\n",
            format_timestamp(b.entry.start),
            format_timestamp(b.entry.end),
            text
        );
    }
    out
}

// ── File I/O helpers ──────────────────────────────────────────────────────────

use super::parse::SourceEncoding;
use anyhow::{Context, Result};
use std::path::Path;

/// Encode UTF-8 text back to the source encoding and write atomically
/// (write to .tmp, then rename).
pub fn write_srt(path: &Path, text: &str, enc: SourceEncoding) -> Result<()> {
    let bytes: Vec<u8> = match enc {
        SourceEncoding::Utf8 => text.as_bytes().to_vec(),
        SourceEncoding::Other(encoding) => {
            let (cow, _, had_errors) = encoding.encode(text);
            if had_errors {
                // Fall back to UTF-8 with BOM on encode failure
                let mut b = vec![0xEF, 0xBB, 0xBF];
                b.extend_from_slice(text.as_bytes());
                b
            } else {
                cow.into_owned()
            }
        }
    };

    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("srt")
    ));

    std::fs::write(&tmp, &bytes)
        .with_context(|| format!("cannot write tmp file {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("cannot rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Backup `path` → `path.bak` (overwrite if exists).
pub fn backup_srt(path: &Path) -> Result<()> {
    let bak = path.with_extension(format!(
        "{}.bak",
        path.extension().and_then(|e| e.to_str()).unwrap_or("srt")
    ));
    std::fs::copy(path, &bak)
        .with_context(|| format!("cannot backup {} → {}", path.display(), bak.display()))?;
    Ok(())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(idx: u32, start_ms: u64, end_ms: u64, text: &str) -> SrtEntry {
        SrtEntry {
            idx,
            start: Duration::from_millis(start_ms),
            end: Duration::from_millis(end_ms),
            text: text.to_string(),
        }
    }

    #[test]
    fn format_ts() {
        assert_eq!(format_timestamp(Duration::from_millis(0)), "00:00:00,000");
        assert_eq!(
            format_timestamp(Duration::from_millis(3661001)),
            "01:01:01,001"
        );
        assert_eq!(
            format_timestamp(Duration::from_millis(5025678)),
            "01:23:45,678"
        );
    }

    #[test]
    fn render_basic() {
        let entries = vec![
            entry(1, 1000, 3500, "Hello, world!"),
            entry(2, 4000, 6000, "Second line."),
        ];
        let out = render(&entries);
        assert!(out.contains("00:00:01,000 --> 00:00:03,500"));
        assert!(out.contains("Hello, world!"));
        assert!(out.contains("00:00:04,000 --> 00:00:06,000"));
        assert!(out.contains("Second line."));
    }

    #[test]
    fn render_renumbers_sequentially() {
        let entries = vec![entry(5, 0, 1000, "A"), entry(10, 2000, 3000, "B")];
        let out = render(&entries);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "1");
        assert_eq!(lines[4], "2");
    }

    #[test]
    fn render_bilingual_basic() {
        let e = entry(1, 1000, 3000, "你好，世界");
        let entries = vec![BilingualEntry {
            entry: &e,
            translation: "Hello, world!",
        }];
        let out = render_bilingual(&entries);
        assert!(out.contains("你好，世界\nHello, world!"));
    }

    #[test]
    fn render_bilingual_empty_translation() {
        let e = entry(1, 1000, 3000, "Only primary");
        let entries = vec![BilingualEntry {
            entry: &e,
            translation: "",
        }];
        let out = render_bilingual(&entries);
        assert!(out.contains("Only primary"));
        assert!(!out.contains("\n\n\n")); // no extra blank line in text
    }

    #[test]
    fn roundtrip() {
        use super::super::parse::parse_str;
        let entries = vec![
            entry(1, 1000, 3500, "Hello, world!"),
            entry(2, 4000, 6000, "Second line."),
        ];
        let rendered = render(&entries);
        let parsed = parse_str(&rendered).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].start, Duration::from_millis(1000));
        assert_eq!(parsed[0].text, "Hello, world!");
        assert_eq!(parsed[1].text, "Second line.");
    }
}
