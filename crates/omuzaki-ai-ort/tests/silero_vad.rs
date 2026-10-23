//! The Silero VAD model through the `onnx` runtime. Needs `OMUZAKI_ORT_CACHE` and `OMUZAKI_SPEECH_MODELS` (the folder holding
//! `silero_vad.onnx`, the file `omuzaki-speech/models/silero-vad.model.toml` pins); skips with a message when either is unset.
//!
//! The wrapper below does what `omuzaki_speech::SileroVad` does around the model: it prefixes each 512-sample frame with
//! the last 64 samples of the previous one and carries the LSTM state in the slot's `state` / `stateN` ports. With
//! `OMUZAKI_SPEECH_DATA` set to the `LibriVox` clips folder, the probabilities must match the Python reference golden
//! (`omuzaki-speech/tests/golden`) within 1e-3, as the speech host's real-model test does; without it the same frames are scored by a plain `ort`
//! session and the two must agree.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a panic is a test failure

mod common;

use std::path::PathBuf;

use common::{cache, local, with_source};
use omuzaki_ai_api::{DType, Dim, Runtime as _, Session, SessionOptions, Signature, TensorBuf, TensorRef, TensorSpec};
use omuzaki_ai_ort::OrtRuntime;
use ort::session::Session as OrtSession;
use ort::value::TensorRef as OrtTensorRef;

const FRAME: usize = 512;
const CONTEXT: usize = 64;
const STATE: usize = 2 * 128;

fn silero_slot() -> Signature {
    let f32s = |name: &str, shape: &[usize]| {
        TensorSpec::new(name, DType::F32, shape.iter().map(|&n| Dim::Fixed(n)).collect::<Vec<_>>())
    };
    Signature {
        inputs: vec![
            f32s("input", &[1, CONTEXT + FRAME]),
            f32s("state", &[2, 1, 128]),
            TensorSpec::new("sr", DType::I64, []),
        ],
        outputs: vec![f32s("output", &[1, 1]), f32s("stateN", &[2, 1, 128])],
    }
}

/// Scores `audio` in order from a fresh state through the runtime's session.
fn through_the_runtime(session: &mut dyn Session, audio: &[f32]) -> Vec<f32> {
    let mut outputs = TensorBuf::for_outputs(session.signature());
    let mut state = vec![0.0f32; STATE];
    let mut input = vec![0.0f32; CONTEXT + FRAME];
    let sr = [16_000i64];
    audio
        .chunks(FRAME)
        .map(|chunk| {
            input.copy_within(FRAME.., 0);
            input[CONTEXT..].fill(0.0);
            input[CONTEXT..CONTEXT + chunk.len()].copy_from_slice(chunk);
            session
                .run(
                    &[
                        TensorRef::f32(&input, &[1, CONTEXT + FRAME]),
                        TensorRef::f32(&state, &[2, 1, 128]),
                        TensorRef::i64(&sr, &[]),
                    ],
                    &mut outputs,
                )
                .unwrap();
            state.copy_from_slice(outputs[1].as_f32().unwrap());
            outputs[0].as_f32().unwrap()[0]
        })
        .collect()
}

/// The same frames through a plain `ort` session: what `omuzaki_speech::SileroVad` does.
fn through_plain_ort(model: &std::path::Path, audio: &[f32]) -> Vec<f32> {
    let mut session = OrtSession::builder().unwrap().with_intra_threads(1).unwrap().commit_from_file(model).unwrap();
    let mut state = vec![0.0f32; STATE];
    let mut input = vec![0.0f32; CONTEXT + FRAME];
    let sr = [16_000i64];
    audio
        .chunks(FRAME)
        .map(|chunk| {
            input.copy_within(FRAME.., 0);
            input[CONTEXT..].fill(0.0);
            input[CONTEXT..CONTEXT + chunk.len()].copy_from_slice(chunk);
            let a = OrtTensorRef::from_array_view(([1usize, CONTEXT + FRAME], input.as_slice())).unwrap();
            let b = OrtTensorRef::from_array_view(([2usize, 1, 128], state.as_slice())).unwrap();
            let c = OrtTensorRef::from_array_view(([0usize; 0], sr.as_slice())).unwrap();
            let out = session.run(ort::inputs![a, b, c]).unwrap();
            state.copy_from_slice(out["stateN"].try_extract_tensor::<f32>().unwrap().1);
            out["output"].try_extract_tensor::<f32>().unwrap().1[0]
        })
        .collect()
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// Deterministic test audio: low-level noise with a voiced-like burst (harmonics with a slow envelope).
fn synthetic(seconds: f32) -> Vec<f32> {
    let n = (16_000.0 * seconds) as usize;
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    (0..n)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let noise = ((seed >> 40) as f32 / 16_777_216.0 - 0.5) * 0.02;
            let t = i as f32 / 16_000.0;
            let envelope = (std::f32::consts::PI * (t % 1.5) / 1.5).sin().max(0.0);
            let voiced: f32 =
                (1..=6).map(|h| (2.0 * std::f32::consts::PI * 140.0 * h as f32 * t).sin() / h as f32).sum();
            noise + 0.15 * envelope * voiced
        })
        .collect()
}

#[test]
fn silero_vad_through_the_onnx_runtime_matches_the_reference() {
    let Some(natives) = cache("silero_vad_through_the_onnx_runtime_matches_the_reference") else { return };
    let Some(models) = std::env::var_os("OMUZAKI_SPEECH_MODELS").map(PathBuf::from) else {
        eprintln!("skipping silero_vad_through_the_onnx_runtime_matches_the_reference: set OMUZAKI_SPEECH_MODELS");
        return;
    };
    let model = models.join("silero_vad.onnx");
    let files = [local("silero_vad.onnx", &model)];
    let mut session = with_source(&files, &[], &natives, |s| {
        OrtRuntime.load(s, &silero_slot(), &std::collections::BTreeMap::new(), &SessionOptions::default())
    })
    .unwrap();

    let audio = synthetic(6.0);
    let ours = through_the_runtime(session.as_mut(), &audio);
    let plain = through_plain_ort(&model, &audio);
    let deviation = worst(&ours, &plain);
    eprintln!("synthetic audio, {} frames: largest deviation from a plain ort session {deviation:e}", ours.len());
    assert!(deviation < 1e-3, "{deviation}");
    assert!(ours.iter().any(|&p| p > 0.05), "the burst should score above noise");

    if let Some(data) = std::env::var_os("OMUZAKI_SPEECH_DATA").map(PathBuf::from) {
        let clip = data.join("aesop-52-farmer-and-his-sons.wav");
        if clip.is_file() {
            let golden = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../omuzaki-speech/tests/golden/vad_probabilities_aesop-52.txt"
            ))
            .unwrap();
            let expected: Vec<f32> = golden.lines().map(|l| l.parse().unwrap()).collect();
            let mut wav = hound::WavReader::open(&clip).unwrap();
            let pcm: Vec<f32> = match wav.spec().sample_format {
                hound::SampleFormat::Int => wav.samples::<i16>().map(|s| f32::from(s.unwrap()) / 32768.0).collect(),
                hound::SampleFormat::Float => wav.samples::<f32>().map(Result::unwrap).collect(),
            };
            let probabilities = through_the_runtime(session.as_mut(), &pcm);
            let deviation = worst(&probabilities, &expected);
            eprintln!(
                "LibriVox clip, {} frames: largest deviation from the Python reference {deviation:e}",
                expected.len()
            );
            assert!(deviation < 1e-3, "largest deviation from the Python reference: {deviation}");
        } else {
            eprintln!("the LibriVox golden skipped: {} not found", clip.display());
        }
    } else {
        eprintln!("the LibriVox golden skipped: set OMUZAKI_SPEECH_DATA to the clips folder");
    }
}
