//! Silero VAD integration tests. Need the ONNX Runtime library and `silero_vad.onnx`; skip without them.

mod common;

use speech_spike::pipeline::detect;
use speech_spike::segmenter::{Event, FRAME, SegmenterConfig};
use speech_spike::{synth, wav};

fn max_probability(probabilities: &[f32]) -> f32 {
    probabilities.iter().fold(0.0f32, |m, &p| m.max(p))
}

#[test]
fn non_speech_never_reaches_the_threshold() -> Result<(), Box<dyn std::error::Error>> {
    let Some(mut vad) = common::vad() else { return Ok(()) };
    let scenes = [
        ("silence", synth::silence(5.0)),
        ("white noise", synth::white_noise(5.0, 0.1, 1)),
        ("pink noise", synth::pink_noise(5.0, 0.03, 2)),
        ("hum", synth::hum(5.0, 50.0, 0.1)),
        ("clicks", synth::clicks(5.0, 0.25, 0.3, 3)),
    ];
    for (name, audio) in scenes {
        let detection = detect(&mut vad, &audio, SegmenterConfig::default())?;
        assert!(detection.events.is_empty(), "{name}: unexpected events {:?}", detection.events);
        assert!(
            max_probability(&detection.probabilities) < 0.5,
            "{name}: max probability {}",
            max_probability(&detection.probabilities)
        );
    }
    Ok(())
}

#[test]
fn scoring_is_stateless_between_streams_and_equals_batch() -> Result<(), Box<dyn std::error::Error>> {
    let Some(mut vad) = common::vad() else { return Ok(()) };
    let audio = synth::pink_noise(2.0, 0.05, 4);
    let batch = vad.probabilities(&audio)?;
    // A second pass over the same audio from a reset state must reproduce the first bit for bit.
    let again = vad.probabilities(&audio)?;
    assert_eq!(batch, again);
    let detection = detect(&mut vad, &audio, SegmenterConfig::default())?;
    assert_eq!(batch, detection.probabilities);
    assert_eq!(batch.len(), audio.len().div_ceil(FRAME));
    Ok(())
}

/// Probabilities from the reference implementation (`tools/vad_reference.py`, Silero's own wrapper logic in Python
/// with the stock onnxruntime) on the first clip. The Rust wrapper must reproduce them: it proves the 64-sample
/// context and the LSTM state are carried exactly as the model expects.
#[test]
fn matches_the_python_reference_on_real_speech() -> Result<(), Box<dyn std::error::Error>> {
    let Some(mut vad) = common::vad() else { return Ok(()) };
    let clip = common::data_dir().join("aesop-52-farmer-and-his-sons.wav");
    let Some(clip) = common::require(&clip) else { return Ok(()) };
    let golden =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/vad_probabilities_aesop-52.txt"))?;
    let expected: Vec<f32> = golden.lines().map(str::parse).collect::<Result<_, _>>()?;
    let probabilities = vad.probabilities(&wav::read_mono_16k(clip)?)?;
    assert_eq!(probabilities.len(), expected.len());
    let worst = probabilities.iter().zip(&expected).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    eprintln!("largest deviation from the Python reference over {} frames: {worst:e}", expected.len());
    assert!(worst < 1e-4, "largest deviation from the Python reference: {worst}");
    // And the clip really does contain speech the segmenter finds.
    let detection = detect(&mut vad, &wav::read_mono_16k(clip)?, SegmenterConfig::default())?;
    assert!(detection.events.iter().any(|(_, e)| matches!(e, Event::End(_))));
    Ok(())
}
