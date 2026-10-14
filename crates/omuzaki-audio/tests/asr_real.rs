//! Whisper and the whole host on real models. Needs `OMUZAKI_SPEECH_MODELS` (and `OMUZAKI_SPEECH_DATA` for the speech
//! clip); skips with a message otherwise. Nothing is downloaded.

#![allow(clippy::cast_precision_loss)] // word counts into a ratio

mod common;

use std::time::Duration;

use common::{data_dir, existing, feed_all, models_dir, ort_library, read_wav};
use omuzaki_audio::asr::{Recognizer, Whisper};
use omuzaki_audio::catalog;
use omuzaki_audio::host::{ModelPaths, SpeechEvent};
use omuzaki_audio::synth::{silence, white_noise};
use omuzaki_audio::{HostConfig, SpeechHost};

#[test]
fn silence_transcribes_without_error_and_without_inventing_much() {
    let Some(models) = models_dir() else { return };
    let Some(model) = existing(&models, "ggml-tiny.en.bin") else { return };
    let mut whisper = Whisper::load(&model, 2).unwrap_or_else(|e| panic!("loading {}: {e}", model.display()));
    for audio in [silence(2.0), white_noise(2.0, 0.01, 1)] {
        let transcript = whisper.transcribe(&audio, Some("en")).unwrap();
        // Whisper may emit a short filler such as "[BLANK_AUDIO]" or nothing; it must not hallucinate a paragraph.
        assert!(transcript.text.split_whitespace().count() <= 4, "hallucinated: {:?}", transcript.text);
    }
}

/// The catalogue's sizes and hashes must equal what `SHA256SUMS.txt` records for the files the spike downloaded.
#[test]
fn the_catalogue_matches_the_downloaded_files() {
    let Some(models) = models_dir() else { return };
    let Some(sums) = existing(&models, "SHA256SUMS.txt") else { return };
    let sums = std::fs::read_to_string(sums).unwrap();
    for spec in catalog::CATALOG {
        let Some(line) = sums.lines().find(|l| l.ends_with(spec.file_name)) else {
            panic!("{} is not in SHA256SUMS.txt", spec.file_name)
        };
        assert_eq!(line.split_whitespace().next(), Some(spec.sha256), "{}", spec.id);
        if let Some(file) = existing(&models, spec.file_name) {
            assert_eq!(std::fs::metadata(file).unwrap().len(), spec.size, "{}", spec.id);
        }
    }
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// A public-domain `LibriVox` reading goes in at the speed the pipeline can take; every word the reader spoke should
/// come back (the readers depart from the printed text a little, so the bar is 90 % of the reference words).
#[test]
fn the_host_transcribes_a_real_reading_end_to_end() {
    let Some(models) = models_dir() else { return };
    let Some(data) = data_dir() else { return };
    let (Some(vad_model), Some(whisper_model)) =
        (existing(&models, "silero_vad.onnx"), existing(&models, "ggml-base.en.bin"))
    else {
        return;
    };
    let (Some(clip), Some(reference)) =
        (existing(&data, "aesop-52-farmer-and-his-sons.wav"), existing(&data, "aesop-52-farmer-and-his-sons.txt"))
    else {
        return;
    };
    let ort_library = ort_library(&models);
    if !ort_library.is_file() {
        eprintln!("SKIPPED: {} not found", ort_library.display());
        return;
    }
    let paths = ModelPaths { ort_library, vad_model, whisper_model };
    // Offline replay is much faster than real time, so give Whisper room to queue every utterance.
    let config = HostConfig { threads: 4, utterance_queue: 64, ..HostConfig::default() };
    let (mut host, mut input) = SpeechHost::open(&paths, &config).unwrap_or_else(|e| panic!("opening the host: {e}"));
    host.request_transcripts().keep();
    feed_all(&mut input, &read_wav(&clip));
    let stats = host.stop();
    let mut heard = Vec::new();
    while let Some(event) = host.try_next_event() {
        match event {
            SpeechEvent::Final { text, .. } => heard.extend(words(&text)),
            SpeechEvent::Error { message, .. } => panic!("host error: {message}"),
            _ => {}
        }
    }
    let expected = words(&std::fs::read_to_string(reference).unwrap());
    let mut pool = heard;
    let found =
        expected.iter().filter(|w| pool.iter().position(|h| h == *w).map(|i| pool.swap_remove(i)).is_some()).count();
    let recall = found as f64 / expected.len() as f64;
    eprintln!("recall {recall:.3} over {} reference words; {stats:?}", expected.len());
    assert!(recall > 0.9, "recall {recall:.3}");
    assert_eq!((stats.utterances_dropped, stats.samples_dropped), (0, 0));
    assert!(stats.asr_busy > Duration::ZERO);
}
