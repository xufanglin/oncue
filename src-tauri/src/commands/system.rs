use std::io::{Read, Write};
use std::path::Path;

use anyhow::Context;
use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::ffmpeg;
use crate::models;

// ── Download URL for the LGPL macOS arm64 ffmpeg build ────────────────────────
//
// Source: https://www.osxexperts.net/ — static LGPL builds maintained by
// the OSX Experts maintainers. The zip contains a single `ffmpeg` executable.
// OSX Experts publishes per-arch builds at separate cadences. Bump these
// manually when a newer file appears on https://www.osxexperts.net/. The
// site doesn't expose a `/latest` redirect — file names are versioned.
#[allow(dead_code)]
const FFMPEG_DOWNLOAD_URL_AARCH64: &str = "https://www.osxexperts.net/ffmpeg81arm.zip";
#[allow(dead_code)]
const FFMPEG_DOWNLOAD_URL_X86_64: &str = "https://www.osxexperts.net/ffmpeg80intel.zip";

fn ffmpeg_download_url() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    return FFMPEG_DOWNLOAD_URL_AARCH64;
    #[cfg(not(target_arch = "aarch64"))]
    return FFMPEG_DOWNLOAD_URL_X86_64;
}

#[derive(Serialize)]
pub struct SystemStatus {
    pub ffmpeg_ok: bool,
    pub ffmpeg_version: Option<String>,
    pub model_ready: bool,
}

#[tauri::command]
pub fn system_check(app: AppHandle) -> SystemStatus {
    let (ffmpeg_ok, ffmpeg_version) = match ffmpeg::resolve(&app) {
        Ok(bin) => match ffmpeg::probe_ffmpeg(&bin.path) {
            Ok(banner) => (true, Some(banner)),
            Err(_) => (false, None),
        },
        Err(_) => (false, None),
    };
    SystemStatus {
        ffmpeg_ok,
        ffmpeg_version,
        model_ready: models::any_model_ready(&app),
    }
}

// ── ffmpeg download progress event ────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct FfmpegDownloadProgress {
    pub downloaded: u64,
    pub total: u64,
    pub fraction: f32,
    pub done: bool,
    pub error: Option<String>,
}

// Single-flight guard for the ffmpeg download.
static FFMPEG_DOWNLOAD_BUSY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

struct FfmpegDownloadGuard;
impl FfmpegDownloadGuard {
    fn try_acquire() -> Option<Self> {
        if FFMPEG_DOWNLOAD_BUSY
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            Some(Self)
        } else {
            None
        }
    }
}
impl Drop for FfmpegDownloadGuard {
    fn drop(&mut self) {
        FFMPEG_DOWNLOAD_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Download a static LGPL ffmpeg build into the user data dir and make it
/// executable. Emits `ffmpeg:download_progress` events.
#[tauri::command]
pub async fn download_ffmpeg(app: AppHandle) -> Result<(), String> {
    let _guard = FfmpegDownloadGuard::try_acquire()
        .ok_or_else(|| "ffmpeg download already in progress".to_string())?;

    let dest = ffmpeg::user_bin_path(&app).map_err(|e| e.to_string())?;
    let parent = dest
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("cannot create {}", parent.display()))
        .map_err(|e| e.to_string())?;

    let url = ffmpeg_download_url();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} for {url}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);

    // Buffer the zip in memory (~21 MB) so we can seek for unzip.
    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    let mut downloaded: u64 = 0;
    let mut last_pct: u8 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        let pct = if total > 0 {
            ((downloaded as f64 / total as f64) * 100.0) as u8
        } else {
            0
        };
        if pct > last_pct {
            last_pct = pct;
            let _ = app.emit(
                "ffmpeg:download_progress",
                FfmpegDownloadProgress {
                    downloaded,
                    total,
                    fraction: if total > 0 {
                        downloaded as f32 / total as f32
                    } else {
                        0.0
                    },
                    done: false,
                    error: None,
                },
            );
        }
    }

    // Extract single ffmpeg binary from zip.
    let cursor = std::io::Cursor::new(&buf);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| e.to_string())?;
    let mut found = false;
    let tmp = dest.with_extension("download");
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        let basename = Path::new(&name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if basename == "ffmpeg" {
            let mut out = std::fs::File::create(&tmp)
                .with_context(|| format!("cannot create {}", tmp.display()))
                .map_err(|e| e.to_string())?;
            let mut data = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
            out.write_all(&data).map_err(|e| e.to_string())?;
            found = true;
            break;
        }
    }
    if !found {
        return Err("ffmpeg binary not found in archive".into());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms).map_err(|e| e.to_string())?;
    }

    std::fs::rename(&tmp, &dest)
        .with_context(|| format!("cannot rename to {}", dest.display()))
        .map_err(|e| e.to_string())?;

    // Strip macOS quarantine attr so the OS doesn't block execution.
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(&dest)
            .output();
    }

    // Verify it actually runs.
    let dest_str = dest.to_string_lossy().into_owned();
    ffmpeg::probe_ffmpeg(&dest_str)
        .with_context(|| format!("downloaded ffmpeg at {dest_str} failed to run"))
        .map_err(|e| e.to_string())?;

    let _ = app.emit(
        "ffmpeg:download_progress",
        FfmpegDownloadProgress {
            downloaded,
            total,
            fraction: 1.0,
            done: true,
            error: None,
        },
    );

    tracing::info!("ffmpeg downloaded to {}", dest.display());
    Ok(())
}

/// List the **text** subtitle streams embedded in a video file. Bitmap
/// formats (PGS, VobSub) are filtered out — they'd require OCR which we
/// don't ship.
#[tauri::command]
pub fn list_embedded_subtitles(
    app: AppHandle,
    video_path: String,
) -> Result<Vec<ffmpeg::SubtitleStream>, String> {
    let ff = ffmpeg::resolve(&app).map_err(|e| e.to_string())?;
    Ok(ffmpeg::probe_subtitle_streams(&ff.path, &video_path))
}

/// Given a video path, return the path of a sibling subtitle file sharing
/// the same stem (.srt preferred, then .ass / .ssa). Used to route drag-and-
/// drop into align vs generate mode.
#[tauri::command]
pub fn detect_sibling_srt(video_path: String) -> Option<String> {
    let p = Path::new(&video_path);
    for ext in ["srt", "ass", "ssa"] {
        let candidate = p.with_extension(ext);
        if candidate.exists() {
            return Some(candidate.display().to_string());
        }
    }
    None
}
