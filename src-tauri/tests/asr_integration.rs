/// Integration tests for extract_audio + transcribe.
///
/// These tests require:
///   1. ffmpeg accessible on PATH (or set FFMPEG_PATH env var)
///   2. A whisper model at ~/Library/Application Support/com.oncue.app/models/ggml-small.bin
///   3. A 30-second English fixture video at tests/fixtures/sample_30s.mp4
///
/// Run with: cargo test --test asr_integration -- --include-ignored
use oncue_lib::asr::whisper::{AsrOptions, load_context, transcribe};
use oncue_lib::ffmpeg::extract_audio;
use tokio_util::sync::CancellationToken;

fn model_path() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap()
        .join("com.oncue.app/models/ggml-small.bin")
}

fn fixture_video() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample_30s.mp4")
}

#[test]
#[ignore = "requires fixture video and whisper model"]
fn test_extract_and_transcribe() {
    let ffmpeg = std::env::var("FFMPEG_PATH").unwrap_or("ffmpeg".to_string());

    let wav = extract_audio(&ffmpeg, fixture_video(), Some(0.0), Some(30.0))
        .expect("ffmpeg extract_audio failed");
    assert!(!wav.is_empty(), "wav bytes should not be empty");

    let model = model_path();
    assert!(
        model.exists(),
        "whisper model not found at {}",
        model.display()
    );

    let ctx = load_context(&model).expect("load_context failed");
    let cancel = CancellationToken::new();
    let segments =
        transcribe(&ctx, &wav, &AsrOptions::default(), cancel).expect("transcribe failed");

    assert!(!segments.is_empty(), "expected at least one segment");

    // Timestamps must be monotonically non-decreasing
    for w in segments.windows(2) {
        assert!(w[0].start_ms <= w[1].start_ms, "segments not ordered");
    }

    // First segment should have language set
    assert!(
        segments[0].language.is_some(),
        "first segment should report detected language"
    );

    println!("detected language: {:?}", segments[0].language);
    println!("segment count: {}", segments.len());
}
