//! The speech models on the real stack (#1163, #1269): Silero VAD and Whisper are installed through the registry (each
//! passing its slot's smoke test on the LibriVox fables), opened as `vad@1` and `asr@1` sessions, and run by the speech
//! host. Silero through the `onnx` runtime matches the Python reference within 1e-3, and a real reading comes back as
//! text. Needs `OMUZAKI_SPEECH_MODELS`, `OMUZAKI_ORT_CACHE` and `OMUZAKI_SPEECH_DATA` (see `speech_support`); skips
//! with a message otherwise.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)] // a panic is a test failure

mod speech_support;

use std::time::Duration;

use omuzaki_ai_api::ModelRegistry;
use omuzaki_speech::host::SpeechEvent;
use omuzaki_speech::segmenter::{Event, Segmenter, SegmenterConfig};
use omuzaki_speech::slots::asr::{edit_distance, words};
use omuzaki_speech::slots::clips::{CLIPS, decode_16k};
use omuzaki_speech::slots::{asr_slot, vad_slot};
use omuzaki_speech::{HostConfig, Recognizer as _, SileroVad, SpeechHost, Whisper, synth};
use speech_support::{folders, install, read_wav, registry};

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("omuzaki-speech-models-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Installs both models (smoke tests included) and prints what the smoke tests measured: the numbers the slots'
/// floors were set from.
#[test]
fn both_speech_models_install_after_passing_their_smoke_tests() {
    let Some(files) = folders() else { return };
    let dir = temp("install");
    let registry = registry(files, &dir);
    for (slot, id) in [(vad_slot(), "silero-vad"), (asr_slot(), "whisper-tiny-en")] {
        let model = install(&registry, &slot, id);
        let entry = registry.models(Some(&slot)).into_iter().find(|m| m.model == model).unwrap();
        let report = entry.eval.expect("an install records its smoke test");
        assert!(report.passed(), "{id}: {report:?}");
        for check in &report.checks {
            let m = &check.metric;
            eprintln!("{id} {} {:.4} on {} (floor {:?})", m.name, m.value, m.set.id, check.floor);
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Probabilities from the reference implementation (`omuzaki-speech/tools/vad_reference.py`: Silero's own wrapper
/// logic in Python on the stock onnxruntime) on the first LibriVox clip. The session the registry opens must reproduce
/// them within 1e-3: the 64-sample context and the recurrent state are carried exactly as the model expects.
#[test]
fn silero_through_the_registry_matches_the_python_reference() {
    let Some(files) = folders() else { return };
    let clip = files.0[2].join("aesop-52-farmer-and-his-sons.wav");
    let dir = temp("golden");
    let registry = registry(files, &dir);
    install(&registry, &vad_slot(), "silero-vad");
    let mut vad = SileroVad::new(registry.open(&vad_slot(), None).unwrap()).unwrap();

    for (name, audio) in [
        ("silence", synth::silence(5.0)),
        ("white noise -20 dBFS", synth::white_noise(5.0, 0.1, 1)),
        ("50 Hz hum", synth::tone(5.0, 50.0, 0.1)),
        ("1 kHz tone", synth::tone(5.0, 1000.0, 0.2)),
        ("clicks", synth::clicks(5.0, 0.25, 0.3, 3)),
    ] {
        let probabilities = vad.probabilities(&audio).unwrap();
        let most = probabilities.iter().fold(0.0f32, |m, &p| m.max(p));
        assert!(most < 0.5, "{name}: max probability {most}");
    }

    if clip.is_file() {
        let golden = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../omuzaki-speech/tests/golden/vad_probabilities_aesop-52.txt"
        ))
        .unwrap();
        let expected: Vec<f32> = golden.lines().map(|l| l.parse().unwrap()).collect();
        let probabilities = vad.probabilities(&read_wav(&clip)).unwrap();
        assert_eq!(probabilities.len(), expected.len());
        let worst = probabilities.iter().zip(&expected).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        eprintln!("largest deviation from the Python reference over {} frames: {worst:e}", expected.len());
        assert!(worst < 1e-3, "largest deviation from the Python reference: {worst}");
        let mut segmenter = Segmenter::new(SegmenterConfig::default());
        let ends = probabilities.iter().filter_map(|&p| segmenter.push(p)).filter(|e| matches!(e, Event::End(_)));
        assert!(ends.count() > 0, "no speech found in the clip");
    } else {
        eprintln!("SKIPPED the golden comparison: {} not found", clip.display());
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn whisper_hears_little_in_silence_and_noise() {
    let Some(files) = folders() else { return };
    let dir = temp("silence");
    let registry = registry(files, &dir);
    install(&registry, &asr_slot(), "whisper-tiny-en");
    let mut whisper = Whisper::new(registry.open(&asr_slot(), None).unwrap()).unwrap();
    for audio in [synth::silence(2.0), synth::white_noise(2.0, 0.01, 1)] {
        let transcript = whisper.transcribe(&audio, Some("en")).unwrap();
        // Whisper may emit a short filler such as "[BLANK_AUDIO]" or nothing; it must not hallucinate a paragraph.
        assert!(transcript.text.split_whitespace().count() <= 4, "hallucinated: {:?}", transcript.text);
        assert_eq!(whisper.language(), "en");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A public-domain LibriVox reading goes in at the speed the pipeline can take, through both registry sessions and the
/// host; the words that come back match what was read (word error rate, as the `asr@1` smoke test counts it).
#[test]
fn the_host_transcribes_a_real_reading_on_registry_sessions() {
    let Some(files) = folders() else { return };
    let clip = CLIPS[0];
    let audio = decode_16k(&files.0[2].join(clip.url.rsplit('/').next().unwrap())).unwrap();
    let dir = temp("host");
    let registry = registry(files, &dir);
    install(&registry, &vad_slot(), "silero-vad");
    install(&registry, &asr_slot(), "whisper-tiny-en");
    let vad = registry.open(&vad_slot(), None).unwrap();
    let asr = registry.open(&asr_slot(), None).unwrap();
    // Offline replay is much faster than real time, so give Whisper room to queue every utterance.
    let config = HostConfig { utterance_queue: 64, ..HostConfig::default() };
    let (mut host, mut input) = SpeechHost::open(vad, asr, &config).unwrap();
    host.request_transcripts().keep();
    for block in audio.chunks(160) {
        while input.free() < block.len() {
            std::thread::park_timeout(Duration::from_millis(1));
        }
        assert_eq!(input.push(block), block.len());
    }
    let stats = host.stop();
    let mut heard = String::new();
    while let Some(event) = host.try_next_event() {
        match event {
            SpeechEvent::Final { text, .. } => {
                heard.push_str(&text);
                heard.push(' ');
            }
            SpeechEvent::Error { message, .. } => panic!("host error: {message}"),
            _ => {}
        }
    }
    let reference = words(clip.text);
    let errors = edit_distance(&words(&heard), &reference);
    let wer = errors as f64 / reference.len() as f64;
    eprintln!("{}: word error rate {wer:.3} over {} words; heard {heard:?}; {stats:?}", clip.name, reference.len());
    assert!(wer < 0.15, "word error rate {wer:.3}");
    assert_eq!((stats.utterances_dropped, stats.samples_dropped, stats.vad_errors), (0, 0, 0));
    assert!(stats.asr_busy > Duration::ZERO);
    std::fs::remove_dir_all(&dir).unwrap();
}
