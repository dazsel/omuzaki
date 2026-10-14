//! Silero VAD on real models: synthetic non-speech, state handling, and the golden match with the Python reference.
//! Needs `OMUZAKI_SPEECH_MODELS` (and `OMUZAKI_SPEECH_DATA` for the golden test); skips with a message otherwise.

mod common;

use omuzaki_audio::segmenter::{Event, FRAME, Segmenter, SegmenterConfig};
use omuzaki_audio::synth;

fn max_probability(probabilities: &[f32]) -> f32 {
    probabilities.iter().fold(0.0f32, |m, &p| m.max(p))
}

fn events(probabilities: &[f32]) -> Vec<Event> {
    let mut segmenter = Segmenter::new(SegmenterConfig::default());
    let mut out: Vec<Event> = probabilities.iter().filter_map(|&p| segmenter.push(p)).collect();
    out.extend(segmenter.finish());
    out
}

#[test]
fn silence_noise_tones_and_clicks_are_not_speech() {
    let Some(mut vad) = common::silero() else { return };
    let scenes = [
        ("silence", synth::silence(5.0)),
        ("white noise -20 dBFS", synth::white_noise(5.0, 0.1, 1)),
        ("white noise -40 dBFS", synth::white_noise(5.0, 0.01, 2)),
        ("50 Hz hum", synth::tone(5.0, 50.0, 0.1)),
        ("1 kHz tone", synth::tone(5.0, 1000.0, 0.2)),
        ("clicks", synth::clicks(5.0, 0.25, 0.3, 3)),
    ];
    for (name, audio) in scenes {
        let probabilities = vad.probabilities(&audio).unwrap();
        assert!(events(&probabilities).is_empty(), "{name}: segments found");
        assert!(max_probability(&probabilities) < 0.5, "{name}: max probability {}", max_probability(&probabilities));
    }
}

#[test]
fn scoring_restarts_from_a_clean_state_and_yields_one_probability_per_frame() {
    let Some(mut vad) = common::silero() else { return };
    let audio = synth::white_noise(2.0, 0.05, 4);
    let first = vad.probabilities(&audio).unwrap();
    assert_eq!(first, vad.probabilities(&audio).unwrap(), "a second pass from a reset state must be bit-identical");
    assert_eq!(first.len(), audio.len().div_ceil(FRAME));
}

/// Probabilities from the reference implementation (`tools/vad_reference.py`: Silero's own wrapper logic in Python on the
/// stock onnxruntime) on the first `LibriVox` clip. The Rust wrapper must reproduce them within 1e-3: that proves the
/// 64-sample context and the LSTM state are carried exactly as the model expects.
#[test]
fn matches_the_python_reference_on_real_speech() {
    let Some(mut vad) = common::silero() else { return };
    let Some(data) = common::data_dir() else { return };
    let Some(clip) = common::existing(&data, "aesop-52-farmer-and-his-sons.wav") else { return };
    let golden =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/vad_probabilities_aesop-52.txt"))
            .unwrap();
    let expected: Vec<f32> = golden.lines().map(|l| l.parse().unwrap()).collect();
    let probabilities = vad.probabilities(&common::read_wav(&clip)).unwrap();
    assert_eq!(probabilities.len(), expected.len());
    let worst = probabilities.iter().zip(&expected).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    eprintln!("largest deviation from the Python reference over {} frames: {worst:e}", expected.len());
    assert!(worst < 1e-3, "largest deviation from the Python reference: {worst}");
    assert!(events(&probabilities).iter().any(|e| matches!(e, Event::End(_))), "no speech found in the clip");
}
