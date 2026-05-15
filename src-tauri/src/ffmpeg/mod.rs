use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;
use tauri::AppHandle;
use tauri::Manager;

/// Resolved path to the ffmpeg executable (system or sidecar).
#[derive(Debug, Clone)]
pub struct FfmpegBin {
    pub path: String,
    pub is_sidecar: bool,
}

// ── Embedded subtitle stream metadata ────────────────────────────────────────

/// One text-subtitle stream found in a container, with the info we need to
/// (a) show it to the user and (b) tell ffmpeg which one to extract.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubtitleStream {
    /// Position among **subtitle streams**, 0-based. Used as `0:s:N` in
    /// ffmpeg `-map`. NOT the absolute stream index in the container.
    pub index: u32,
    /// Codec name as ffmpeg reports it (e.g. "subrip", "ass", "mov_text").
    pub codec: String,
    /// ISO 639-2 language tag from container metadata, if present.
    pub language: Option<String>,
    /// Optional `title` metadata.
    pub title: Option<String>,
    /// True if this is a forced track.
    pub forced: bool,
    /// True if this is the default track.
    pub default: bool,
}

const TEXT_SUB_CODECS: &[&str] = &["subrip", "srt", "mov_text", "ass", "ssa", "webvtt", "text"];

/// Probe a video file and return the list of **text** subtitle streams that
/// can be extracted directly (no OCR). Bitmap formats like PGS/VobSub are
/// intentionally filtered out.
pub fn probe_subtitle_streams(ffmpeg: &str, video_path: &str) -> Vec<SubtitleStream> {
    // `ffmpeg -i <file>` exits non-zero (no output) but prints stream info
    // to stderr. We parse "Stream #0:N(lang): Subtitle: codec ..." lines.
    let out = match Command::new(ffmpeg)
        .args(["-hide_banner", "-i", video_path])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    parse_subtitle_streams(&stderr)
}

/// Parse the relevant stream-info lines out of an `ffmpeg -i` banner.
fn parse_subtitle_streams(banner: &str) -> Vec<SubtitleStream> {
    // Example matches we care about:
    //   "  Stream #0:2(eng): Subtitle: subrip (default)"
    //   "  Stream #0:3(chi): Subtitle: ass"
    //   "    Metadata:\n      title           : English SDH"
    let mut out = Vec::new();
    let mut sub_index: u32 = 0;
    let mut current_title_pending: Option<usize> = None;
    for line in banner.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("Stream #") {
            // Reset pending-title pointer every Stream line.
            current_title_pending = None;
            // Find "Subtitle:" marker; if absent it's not a subtitle.
            let Some(after_kind) = trimmed.split("Subtitle:").nth(1) else {
                continue;
            };
            let codec = after_kind
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            if !TEXT_SUB_CODECS
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&codec))
            {
                // Still bump pseudo-index? No — `0:s:N` indexes ALL subtitle
                // streams (text and bitmap together), so we MUST advance for
                // every Subtitle line, even ones we filter out.
                sub_index += 1;
                continue;
            }

            // Language: parenthesized tag right after "#0:N", e.g. "#0:2(eng)"
            let language = trimmed
                .split_once('(')
                .and_then(|(_, after)| after.split_once(')'))
                .map(|(lang, _)| lang.trim().to_string())
                .filter(|s| !s.is_empty() && s.len() <= 6);

            let dispositions = after_kind;
            let forced = dispositions.contains("(forced)");
            let default = dispositions.contains("(default)");

            out.push(SubtitleStream {
                index: sub_index,
                codec,
                language,
                title: None,
                forced,
                default,
            });
            // Remember which entry to attach a Metadata title to.
            current_title_pending = Some(out.len() - 1);
            sub_index += 1;
        } else if trimmed.starts_with("title") {
            // Lines look like: "title           : English SDH"
            if let Some(slot) = current_title_pending
                && let Some((_, value)) = trimmed.split_once(':')
            {
                let title = value.trim().to_string();
                if !title.is_empty()
                    && let Some(s) = out.get_mut(slot)
                {
                    s.title = Some(title);
                }
            }
        } else if trimmed.starts_with("Stream") {
            current_title_pending = None;
        }
    }
    out
}

/// Extract one text-subtitle stream into a UTF-8 string. The stream is
/// transcoded to SRT format on the fly via `-c:s srt`.
pub fn extract_subtitle(ffmpeg: &str, video_path: &str, stream_index: u32) -> Result<String> {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error"])
        .args(["-i", video_path])
        .args(["-map", &format!("0:s:{stream_index}")])
        .args(["-c:s", "srt", "-f", "srt", "pipe:1"])
        .output()
        .with_context(|| format!("failed to spawn ffmpeg for subtitle extract '{video_path}'"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "ffmpeg subtitle extract failed: status={} err={}",
            out.status,
            err.trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run `ffmpeg -version` at the given path and return the first output line.
pub fn probe_ffmpeg(path: &str) -> Result<String> {
    let out = Command::new(path)
        .arg("-version")
        .output()
        .with_context(|| format!("failed to run '{path} -version'"))?;
    if !out.status.success() {
        bail!("'{path} -version' exited with {}", out.status);
    }
    let first = std::str::from_utf8(&out.stdout)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    Ok(first)
}

fn parse_major_version(banner: &str) -> Option<u32> {
    // "ffmpeg version 7.1.1 ..." or "ffmpeg version n5.1.2-0 ..."
    let version_str = banner.split_whitespace().nth(2)?;
    let numeric = version_str.trim_start_matches(|c: char| !c.is_ascii_digit());
    numeric.split('.').next()?.parse().ok()
}

/// Platform-specific sidecar binary name embedded in `Resources/bin/`.
fn sidecar_name() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    return "ffmpeg-aarch64-apple-darwin";
    #[cfg(not(target_arch = "aarch64"))]
    return "ffmpeg-x86_64-apple-darwin";
}

fn sidecar_path(app: &AppHandle) -> Result<PathBuf> {
    let mut p = app
        .path()
        .resource_dir()
        .context("cannot resolve app resource dir")?;
    p.push("bin");
    p.push(sidecar_name());
    Ok(p)
}

/// User-writable location where the in-app downloader places ffmpeg.
pub fn user_bin_path(app: &AppHandle) -> Result<PathBuf> {
    let mut p = app
        .path()
        .app_data_dir()
        .context("cannot resolve app data dir")?;
    p.push("bin");
    p.push("ffmpeg");
    Ok(p)
}

/// Resolve ffmpeg: prefer system PATH (≥ 5), then user-downloaded binary,
/// then bundled sidecar.
pub fn resolve(app: &AppHandle) -> Result<FfmpegBin> {
    if let Ok(banner) = probe_ffmpeg("ffmpeg") {
        if parse_major_version(&banner).unwrap_or(0) >= 5 {
            tracing::info!("using system ffmpeg: {banner}");
            return Ok(FfmpegBin {
                path: "ffmpeg".into(),
                is_sidecar: false,
            });
        }
        tracing::warn!("system ffmpeg too old ({banner}), falling back");
    }

    if let Ok(user) = user_bin_path(app)
        && user.exists()
    {
        let s = user.to_string_lossy().into_owned();
        if let Ok(banner) = probe_ffmpeg(&s) {
            tracing::info!("using user-downloaded ffmpeg: {banner}");
            return Ok(FfmpegBin {
                path: s,
                is_sidecar: false,
            });
        }
    }

    let path = sidecar_path(app)?;
    let path_str = path.to_string_lossy().into_owned();
    let banner = probe_ffmpeg(&path_str)
        .with_context(|| format!("bundled ffmpeg at '{path_str}' is not functional"))?;
    tracing::info!("using bundled ffmpeg: {banner}");
    Ok(FfmpegBin {
        path: path_str,
        is_sidecar: true,
    })
}

// ── Audio extraction ──────────────────────────────────────────────────────────

const SAMPLE_RATE: u64 = 16_000;
const BYTES_PER_SAMPLE: u64 = 2; // s16

/// Probe the total duration of `video_path` in seconds via ffmpeg's banner.
/// Returns None if duration cannot be parsed.
pub fn probe_duration(ffmpeg: &str, video_path: &str) -> Option<f64> {
    // `ffmpeg -i <file>` exits with non-zero status (no output specified) but
    // prints stream info to stderr, including `Duration: HH:MM:SS.cc`.
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-i", video_path])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("Duration:"))?;
    let after = line.split("Duration:").nth(1)?.trim();
    let token = after.split([',', ' ']).next()?;
    let mut parts = token.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

/// Extract a segment of audio from `video_path` as raw 16 kHz mono s16le PCM
/// bytes, invoking `on_progress(0.0..=1.0)` periodically as bytes accumulate.
///
/// * `start_secs` – start offset (None = beginning)
/// * `duration_secs` – duration to extract (None = until end of file)
pub fn extract_audio_with_progress<F: FnMut(f32)>(
    ffmpeg: &str,
    video_path: &str,
    start_secs: Option<f64>,
    duration_secs: Option<f64>,
    mut on_progress: F,
) -> Result<Vec<u8>> {
    use std::io::Read;

    // Total expected output bytes: duration × sample_rate × bytes_per_sample.
    let target_dur = match duration_secs {
        Some(d) => Some(d),
        None => probe_duration(ffmpeg, video_path)
            .map(|total| (total - start_secs.unwrap_or(0.0)).max(0.0)),
    };
    let total_bytes: Option<u64> =
        target_dur.map(|d| (d * SAMPLE_RATE as f64 * BYTES_PER_SAMPLE as f64) as u64);

    let mut cmd = Command::new(ffmpeg);
    if let Some(ss) = start_secs {
        cmd.args(["-ss", &format!("{ss:.3}")]);
    }
    cmd.args(["-i", video_path]);
    if let Some(t) = duration_secs {
        cmd.args(["-t", &format!("{t:.3}")]);
    }
    cmd.args([
        "-vn",
        "-ac",
        "1",
        "-ar",
        "16000",
        "-acodec",
        "pcm_s16le",
        "-f",
        "s16le",
        "pipe:1",
    ]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn ffmpeg for '{video_path}'"))?;
    let mut stdout = child.stdout.take().expect("stdout piped");

    let mut buffer = Vec::with_capacity(total_bytes.unwrap_or(1 << 20) as usize);
    let mut chunk = [0u8; 64 * 1024];
    let mut last_pct: u8 = 0;
    on_progress(0.0);
    loop {
        let n = stdout
            .read(&mut chunk)
            .with_context(|| format!("read ffmpeg stdout for '{video_path}'"))?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(total) = total_bytes
            && total > 0
        {
            let f = (buffer.len() as f64 / total as f64).min(1.0) as f32;
            let pct = (f * 100.0) as u8;
            // Report only when integer % advances, to avoid event spam.
            if pct > last_pct {
                last_pct = pct;
                on_progress(f);
            }
        }
    }

    let status = child
        .wait()
        .with_context(|| format!("wait ffmpeg for '{video_path}'"))?;
    if !status.success() {
        bail!("ffmpeg exited with {status} for '{video_path}'");
    }
    on_progress(1.0);
    Ok(buffer)
}

/// Convenience wrapper without progress reporting.
pub fn extract_audio(
    ffmpeg: &str,
    video_path: &str,
    start_secs: Option<f64>,
    duration_secs: Option<f64>,
) -> Result<Vec<u8>> {
    extract_audio_with_progress(ffmpeg, video_path, start_secs, duration_secs, |_| {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_major_version() {
        assert_eq!(
            parse_major_version("ffmpeg version 7.1.1 Copyright"),
            Some(7)
        );
        assert_eq!(
            parse_major_version("ffmpeg version n5.1.2-0+something"),
            Some(5)
        );
        assert_eq!(parse_major_version("ffmpeg version 4.4.0"), Some(4));
    }
}
