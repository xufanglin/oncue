/// Integration test: run precise resync on a fixture SRT with artificial +3s offset.
///
/// Requirements:
///   1. ffmpeg on PATH (or FFMPEG_PATH env)
///   2. Whisper model at ~/Library/Application Support/com.oncue.app/models/ggml-small.bin
///   3. Fixture video at tests/fixtures/sample_30s.mp4
///
/// Run with: cargo test --test precise_integration -- --include-ignored
use std::time::Duration;

use oncue_lib::align::nw::{align, derive_timecodes, srt_to_tokens};
use oncue_lib::asr::whisper::{AsrOptions, load_context, transcribe_words};
use oncue_lib::ffmpeg::extract_audio;
use oncue_lib::srt::parse::SrtEntry;
use tokio_util::sync::CancellationToken;

fn model_path() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap()
        .join("com.oncue.app/models/ggml-small.bin")
}

fn fixture_video() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample_30s.mp4")
}

/// Build a synthetic SRT with 3-second positive offset applied to known good timings.
fn shifted_srt(entries: &[SrtEntry], offset_ms: i64) -> Vec<SrtEntry> {
    entries
        .iter()
        .map(|e| {
            let s = (e.start.as_millis() as i64 + offset_ms).max(0) as u64;
            let en = (e.end.as_millis() as i64 + offset_ms).max(0) as u64;
            SrtEntry {
                idx: e.idx,
                start: Duration::from_millis(s),
                end: Duration::from_millis(en),
                text: e.text.clone(),
            }
        })
        .collect()
}

#[test]
#[ignore = "requires fixture video and whisper model"]
fn precise_eliminates_3s_offset() {
    let ffmpeg = std::env::var("FFMPEG_PATH").unwrap_or("ffmpeg".to_string());

    // 1. Extract audio
    let wav = extract_audio(&ffmpeg, fixture_video(), None, None).expect("extract_audio failed");

    // 2. Transcribe words
    let model = model_path();
    let ctx = load_context(&model).expect("load_context failed");
    let cancel = CancellationToken::new();
    let words = transcribe_words(&ctx, &wav, &AsrOptions::default(), cancel)
        .expect("transcribe_words failed");
    assert!(!words.is_empty());

    // 3. Build "ground truth" SRT from ASR segments (pretend these are the correct timings)
    let ground_truth: Vec<SrtEntry> = words
        .chunks(3)
        .enumerate()
        .map(|(i, chunk)| SrtEntry {
            idx: i as u32 + 1,
            start: Duration::from_millis(chunk[0].start_ms.max(0) as u64),
            end: Duration::from_millis(chunk.last().unwrap().end_ms.max(0) as u64),
            text: chunk
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        })
        .collect();

    // 4. Apply +3s artificial offset
    let shifted = shifted_srt(&ground_truth, 3000);

    // 5. Run NW alignment on the shifted SRT vs real ASR words
    let (srt_tokens, spans) = srt_to_tokens(&shifted);
    let srt_refs: Vec<&str> = srt_tokens.iter().map(String::as_str).collect();
    let asr_refs: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let ops = align(&srt_refs, &asr_refs, 500);
    let timings = derive_timecodes(&ops, &words, &shifted, &spans);

    // 6. Assert: corrected timings are close to ground truth (within 500 ms)
    let from_alignment = timings.iter().filter(|t| t.from_alignment).count();
    assert!(
        from_alignment as f32 / timings.len() as f32 > 0.5,
        "fewer than 50% entries aligned: {from_alignment}/{}",
        timings.len()
    );

    for (i, (t, gt)) in timings.iter().zip(ground_truth.iter()).enumerate() {
        if !t.from_alignment {
            continue;
        }
        let delta = (t.start.as_millis() as i64 - gt.start.as_millis() as i64).abs();
        assert!(
            delta <= 500,
            "entry {i}: corrected start {}ms, ground truth {}ms, delta {delta}ms",
            t.start.as_millis(),
            gt.start.as_millis()
        );
    }
}
