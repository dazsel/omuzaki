//! Whisper integration test. Needs a ggml model; skips (loudly) without one.

use std::path::PathBuf;

use speech_spike::asr::{AsrOptions, Whisper};
use speech_spike::synth;

/// The model directory: `OMUZAKI_SPEECH_MODELS`, or `models/speech` under `OMUZAKI_ASSETS` (default: `assets-local` in the
/// repository root).
fn models_dir() -> PathBuf {
    std::env::var_os("OMUZAKI_SPEECH_MODELS").map_or_else(
        || {
            let assets = std::env::var_os("OMUZAKI_ASSETS")
                .filter(|v| !v.is_empty())
                .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets-local"), PathBuf::from);
            assets.join("models/speech")
        },
        PathBuf::from,
    )
}

#[test]
fn silence_transcribes_without_error_and_without_inventing_much() -> Result<(), Box<dyn std::error::Error>> {
    let model = models_dir().join("ggml-tiny.en.bin");
    if !model.exists() {
        eprintln!("SKIPPED: {} not found (set OMUZAKI_SPEECH_MODELS)", model.display());
        return Ok(());
    }
    let mut whisper = Whisper::load(&model)?;
    let transcript = whisper.transcribe(&synth::silence(2.0), &AsrOptions::default())?;
    // Whisper may emit a short filler such as "[BLANK_AUDIO]" or nothing; it must not hallucinate a paragraph.
    assert!(transcript.text.split_whitespace().count() <= 4, "hallucinated: {:?}", transcript.text);
    Ok(())
}
