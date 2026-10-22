//! Text to speech on the real stack (#1133): Kokoro-82M is installed through the registry after passing `tts@1`'s
//! round-trip smoke test (its speech, transcribed by the shipped Whisper, matches what it was asked to say), then
//! opened as a `tts@1` session that speaks in every voice its manifest lists. Needs the speech folders and
//! `OMUZAKI_TTS_MODELS` (see `speech_support`); skips with a message otherwise. With `OMUZAKI_TTS_EVIDENCE` set to a
//! folder, it also writes what it said there as WAV files, to listen to.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::doc_markdown)] // a panic is a test failure

mod speech_support;

use std::time::Instant;

use omuzaki_ai_api::ModelRegistry;
use omuzaki_speech::AsrCall;
use omuzaki_speech::g2p::numbers::normalise;
use omuzaki_speech::slots::asr::{edit_distance, words};
use omuzaki_speech::slots::tts::{TTS_RATE, TtsCall, Utterance, resample};
use omuzaki_speech::slots::{asr_slot, tts_slot};
use speech_support::{install, registry, tts_folders};

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("omuzaki-tts-models-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_wav(name: &str, pcm: &[f32]) {
    let Some(dir) = std::env::var_os("OMUZAKI_TTS_EVIDENCE").map(std::path::PathBuf::from) else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TTS_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(dir.join(format!("{name}.wav")), spec).unwrap();
    for &s in pcm {
        #[allow(clippy::cast_possible_truncation)] // clamped to 16 bits
        wav.write_sample((s * 32767.0).round().clamp(-32768.0, 32767.0) as i16).unwrap();
    }
    wav.finalize().unwrap();
}

/// What the person hears: a line with a number, money, an abbreviation and a question, as a stream would have it.
const LINE: &str = "Dr. Smith said the first 25 people in chat get a $5 badge, so are you ready?";

/// The words of `text` once numbers and abbreviations are read out, so "25" and "twenty-five" are the same words.
fn spoken(text: &str) -> Vec<String> {
    words(&normalise(text))
}

#[test]
fn kokoro_installs_after_its_round_trip_and_speaks_in_every_voice() {
    let Some(files) = tts_folders() else { return };
    let dir = temp("install");
    let registry = registry(files, &dir);
    install(&registry, &asr_slot(), "whisper-tiny-en");
    let kokoro = install(&registry, &tts_slot(), "kokoro-82m");

    let entry = registry.models(Some(&tts_slot())).into_iter().find(|m| m.model == kokoro).unwrap();
    let report = entry.eval.expect("an install records its smoke test");
    assert!(report.passed(), "{report:?}");
    for check in &report.checks {
        let m = &check.metric;
        eprintln!("kokoro-82m {} {:.4} on {} (floor {:?})", m.name, m.value, m.set.id, check.floor);
    }

    let manifest = registry.manifest(&kokoro).expect("the registry keeps the manifest");
    let voices: Vec<&str> = manifest.voices.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(voices, ["af_heart", "af_bella", "am_michael", "bf_emma"]);
    assert!(manifest.voices.iter().all(|v| !v.needs_consent()), "Kokoro's voices are its own");
    assert!(manifest.tts.as_ref().is_some_and(|t| t.honours("speed") && !t.honours("expressiveness")));

    let mut tts = registry.open(&tts_slot(), Some(&kokoro)).unwrap();
    let mut asr = registry.open(&asr_slot(), None).unwrap();
    let (mut call, mut heard) = (TtsCall::new(), AsrCall::new());
    let expected = spoken(LINE);
    for voice in &voices {
        let started = Instant::now();
        let pcm = call.speak(tts.as_mut(), &Utterance { voice, ..Utterance::plain(LINE) }).unwrap().to_vec();
        let took = started.elapsed().as_secs_f64();
        let seconds = pcm.len() as f64 / f64::from(TTS_RATE);
        let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        write_wav(&format!("kokoro-{voice}"), &pcm);
        let text = heard.transcribe(asr.as_mut(), &resample(&pcm, TTS_RATE, 16_000), Some("en")).unwrap().text;
        let errors = edit_distance(&spoken(&text), &expected);
        eprintln!(
            "{voice}: {seconds:.2} s of speech in {took:.2} s (real-time factor {:.2}), peak {peak:.2}, heard {text:?}, \
             {errors} word errors",
            took / seconds
        );
        assert!((3.0..12.0).contains(&seconds), "{voice}: {seconds} s");
        assert!(peak > 0.05 && peak <= 1.5, "{voice}: peak {peak}");
        assert!(errors <= 1, "{voice} said {text:?}");
    }

    // Speed is honoured: twice as fast is about half as long.
    let normal = call.speak(tts.as_mut(), &Utterance::plain(LINE)).unwrap().len();
    let fast = call.speak(tts.as_mut(), &Utterance { speed: 2.0, ..Utterance::plain(LINE) }).unwrap().len();
    let ratio = fast as f64 / normal as f64;
    eprintln!("speed 2: {ratio:.2} of the length");
    assert!((0.4..0.7).contains(&ratio), "speed 2 gave {ratio} of the length");

    // An unknown voice is an error, not a silent fallback.
    assert!(call.speak(tts.as_mut(), &Utterance { voice: "nobody", ..Utterance::plain(LINE) }).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}
