//! The speech slots and shipped models (#1163, #1133): `vad@1`, `asr@1` and `tts@1` are registered with their
//! signatures, every shipped manifest parses and validates (with the pins the old catalogue had, and Kokoro with its
//! voices and gold lexicons only), the `whisper-ggml` and `kokoro` runtimes are registered, and the evaluations pass a
//! perfect fake and fail a useless one.

// Integration-test helpers may panic (a clear failure) and convert frame counts.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss)]

use omuzaki_ai_api::{
    DType, ModelManifest, Origin, RuntimeId, Session, SlotMode, TensorBuf, TensorRef, find_runtime, find_slot,
    shipped_models,
};
use omuzaki_ai_mock::{FakeSession, FakeTts};
use omuzaki_speech::segmenter::FRAME;
use omuzaki_speech::slots::asr::{self, Reading, word_error_rate};
use omuzaki_speech::slots::tts::{self, SENTENCES, TtsCall, Utterance, round_trip_error};
use omuzaki_speech::slots::vad::{self, Labelled, balanced_accuracy, false_alarms, non_speech};
use omuzaki_speech::slots::{asr_signature, asr_slot, tts_signature, tts_slot, vad_signature, vad_slot};
use omuzaki_speech::synth;

/// The catalogue `omuzaki_speech::catalog` pinned before the models moved into the registry: file, URL, SHA-256, size.
const OLD_CATALOGUE: [(&str, &str, &str, u64); 7] = [
    (
        "silero_vad.onnx",
        "https://raw.githubusercontent.com/snakers4/silero-vad/5cd7945676eb32225748052e2e6a0580e4686a08/src/silero_vad/data/silero_vad.onnx",
        "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3",
        2_327_524,
    ),
    (
        "ggml-tiny.en.bin",
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-tiny.en.bin",
        "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
        77_704_715,
    ),
    (
        "ggml-base.en.bin",
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.en.bin",
        "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
        147_964_211,
    ),
    (
        "ggml-base.en-q5_1.bin",
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.en-q5_1.bin",
        "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f",
        59_721_011,
    ),
    (
        "ggml-small.en.bin",
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small.en.bin",
        "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
        487_614_201,
    ),
    (
        "ggml-small.en-q5_1.bin",
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small.en-q5_1.bin",
        "bfdff4894dcb76bbf647d56263ea2a96645423f1669176f4844a1bf8e478ad30",
        190_098_681,
    ),
    (
        "ggml-large-v3-turbo-q5_0.bin",
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-large-v3-turbo-q5_0.bin",
        "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        574_041_195,
    ),
];

fn speech_manifests() -> Vec<ModelManifest> {
    shipped_models()
        .filter(|m| !m.id.starts_with("echo"))
        .map(|m| {
            let manifest = ModelManifest::from_toml(m.manifest).unwrap_or_else(|e| panic!("{}: {e}", m.id));
            assert_eq!(manifest.id.as_str(), m.id, "the registration and the manifest agree on the id");
            manifest
        })
        .collect()
}

#[test]
fn both_slots_are_registered_with_their_signatures_and_deadlines() {
    let vad = find_slot(&vad_slot()).expect("vad@1 is registered");
    assert_eq!(vad.signature(), vad_signature());
    assert_eq!(vad.info().mode, SlotMode::Call { stall_ms: 50 });
    assert_eq!((vad.info().likeness, vad.info().threads), (None, 1));
    let asr = find_slot(&asr_slot()).expect("asr@1 is registered");
    assert_eq!(asr.signature(), asr_signature());
    assert_eq!(asr.info().mode, SlotMode::Call { stall_ms: 30_000 });
    assert_eq!(asr.info().likeness, None);
    assert_eq!(vad_slot().to_string(), "vad@1");
    assert_eq!(asr_slot().to_string(), "asr@1");
    let tts = find_slot(&tts_slot()).expect("tts@1 is registered");
    assert_eq!(tts.signature(), tts_signature());
    assert_eq!(tts.signature(), omuzaki_ai_mock::tts_signature(), "the fake presents the real slot");
    assert_eq!(tts.info().mode, SlotMode::Call { stall_ms: 30_000 });
}

#[test]
fn the_whisper_and_kokoro_runtimes_are_registered() {
    for id in ["whisper-ggml", "kokoro"] {
        let runtime = find_runtime(&RuntimeId::new(id).unwrap()).unwrap_or_else(|| panic!("{id} is registered"));
        assert_eq!(runtime.id().as_str(), id);
    }
}

#[test]
fn every_shipped_manifest_validates_and_keeps_the_old_catalogue_pins() {
    let manifests = speech_manifests();
    assert_eq!(manifests.len(), OLD_CATALOGUE.len() + 1, "the old catalogue and Kokoro");
    for (file, url, sha256, size) in OLD_CATALOGUE {
        let manifest = manifests.iter().find(|m| m.files[0].name == file).unwrap_or_else(|| panic!("{file} ships"));
        manifest.validate(Origin::Shipped).unwrap_or_else(|e| panic!("{}: {e:?}", manifest.id));
        let pinned = &manifest.files[0];
        assert_eq!((pinned.urls[0].as_str(), pinned.sha256.to_string(), pinned.size), (url, sha256.to_owned(), size));
        let (slot, runtime) = if std::path::Path::new(file).extension().is_some_and(|e| e.eq_ignore_ascii_case("onnx"))
        {
            (vad_slot(), "onnx")
        } else {
            (asr_slot(), "whisper-ggml")
        };
        assert_eq!((&manifest.slot, manifest.runtime.as_str()), (&slot, runtime), "{}", manifest.id);
        assert_eq!(manifest.card.licence.spdx.as_deref(), Some("MIT"), "{}", manifest.id);
        // The digest that names the model's exact bytes, for the pull request.
        eprintln!("{} {} {}", manifest.id, manifest.version, manifest.digest_text().trim_end());
    }
}

fn f32s(values: &[f32], shape: &[usize]) -> TensorBuf {
    let mut buf = TensorBuf::new(DType::F32);
    buf.set(&TensorRef::f32(values, shape)).unwrap();
    buf
}

fn text(value: &str) -> TensorBuf {
    let mut buf = TensorBuf::new(DType::U8);
    buf.set(&TensorRef::u8(value.as_bytes(), &[value.len()])).unwrap();
    buf
}

/// A `vad@1` fake answering `probabilities` in order.
fn vad_fake(probabilities: &[f32]) -> FakeSession {
    let state = [0.0f32; 256];
    let script = probabilities.iter().map(|&p| vec![f32s(&[p], &[1, 1]), f32s(&state, &[2, 1, 128])]).collect();
    FakeSession::scripted(vad_signature(), script)
}

/// Speech (a loud tone) from 1 s to 2 s of 3 s of silence.
fn speech_from_one_to_two(t: f64) -> Option<bool> {
    if (0.9..1.1).contains(&t) || (1.9..2.1).contains(&t) { None } else { Some((1.0..2.0).contains(&t)) }
}

#[test]
fn the_vad_evaluation_passes_a_perfect_model_and_fails_one_that_always_says_maybe() {
    let audio = [synth::silence(1.0), synth::tone(1.0, 220.0, 0.3), synth::silence(1.0)].concat();
    let item = [Labelled { audio: &audio, label: &speech_from_one_to_two }];
    let frames = audio.len().div_ceil(FRAME);
    let truth: Vec<f32> = (0..frames)
        .map(|i| {
            let t = (i * FRAME + FRAME / 2) as f64 / 16_000.0;
            if (1.0..2.0).contains(&t) { 0.98 } else { 0.01 }
        })
        .collect();
    let mut perfect = vad_fake(&truth);
    assert!((balanced_accuracy(&mut perfect, &item).unwrap() - 1.0).abs() < 1e-12);
    let mut quiet = vad_fake(&[0.01]);
    assert!(false_alarms(&mut quiet, &non_speech()).unwrap() < vad::FALSE_ALARM_CEILING);

    let mut maybe = vad_fake(&[0.5]);
    let accuracy = balanced_accuracy(&mut maybe, &item).unwrap();
    assert!((accuracy - 0.5).abs() < 1e-12 && accuracy < vad::SMOKE_FLOOR, "{accuracy}");
    assert!((false_alarms(&mut maybe, &non_speech()).unwrap() - 1.0).abs() < 1e-12);

    let mut wrong_slot = FakeSession::scripted(asr_signature(), vec![]);
    assert!(balanced_accuracy(&mut wrong_slot, &item).is_err(), "a session of another slot is refused");
}

#[test]
fn the_asr_evaluation_passes_the_reference_and_fails_silence() {
    let audio = synth::tone(2.0, 220.0, 0.3);
    let said = "Pray do not kill me: you will find me a most useful bird.";
    let items = [Reading { utterances: vec![&audio], text: said }];
    let mut right = FakeSession::scripted(asr_signature(), vec![vec![text(said), text("en")]]);
    assert!(word_error_rate(&mut right, &items).unwrap().abs() < 1e-12);
    let mut close = FakeSession::scripted(
        asr_signature(),
        vec![vec![text("pray do not kill me you will find me a most useful bard"), text("en")]],
    );
    let wer = word_error_rate(&mut close, &items).unwrap();
    assert!(wer > 0.0 && wer < asr::SMOKE_CEILING, "{wer}");
    let mut deaf = FakeSession::scripted(asr_signature(), vec![vec![text(""), text("en")]]);
    let wer = word_error_rate(&mut deaf, &items).unwrap();
    assert!((wer - 1.0).abs() < 1e-12 && wer > asr::SMOKE_CEILING, "{wer}");
    assert_eq!(right.runs() + close.runs() + deaf.runs(), 3);
}

#[test]
fn sessions_are_opened_for_the_slot_signature_only() {
    let mut vad = vad_fake(&[0.2]);
    assert_eq!(vad.signature(), &vad_signature());
    let mut out = TensorBuf::for_outputs(vad.signature());
    let input = [0.0f32; 576];
    let state = [0.0f32; 256];
    vad.run(
        &[TensorRef::f32(&input, &[1, 576]), TensorRef::f32(&state, &[2, 1, 128]), TensorRef::i64(&[16_000], &[])],
        &mut out,
    )
    .unwrap();
    assert_eq!(out[0].as_f32(), Some(&[0.2f32][..]));
}

#[test]
fn kokoro_ships_its_graph_voices_and_the_gold_lexicons_only() {
    let manifests = speech_manifests();
    let kokoro = manifests.iter().find(|m| m.id.as_str() == "kokoro-82m").expect("Kokoro ships");
    kokoro.validate(Origin::Shipped).unwrap();
    assert_eq!((&kokoro.slot, kokoro.runtime.as_str()), (&tts_slot(), "kokoro"));
    assert_eq!(kokoro.card.licence.spdx.as_deref(), Some("Apache-2.0"));
    let files: Vec<&str> = kokoro.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(files[0], "model.onnx", "the graph is the main file");
    for needed in ["tokenizer.json", "us_gold.json", "gb_gold.json"] {
        assert!(files.contains(&needed), "{needed}");
    }
    // GPL-free by construction: nothing espeak-derived is ever fetched.
    for file in &kokoro.files {
        let name = file.name.to_lowercase();
        assert!(!name.contains("silver") && !name.contains("espeak"), "{}", file.name);
        assert!(file.urls.iter().all(|u| u.starts_with("https://huggingface.co/")), "{}", file.name);
    }
    let voices: Vec<&str> = kokoro.voices.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(voices, ["af_heart", "af_bella", "am_michael", "bf_emma"]);
    assert!(kokoro.voices.iter().all(|v| !v.needs_consent() && !v.languages.is_empty()));
    let spec = kokoro.tts.as_ref().expect("[tts]");
    assert!(spec.honours("speed") && spec.honours("expressiveness"));
    eprintln!("{} {} {}", kokoro.id, kokoro.version, kokoro.digest_text().trim_end());
}

/// A recogniser that hears `heard` for every utterance.
fn hears(heard: &[&str]) -> FakeSession {
    FakeSession::scripted(asr_signature(), heard.iter().map(|h| vec![text(h), text("en")]).collect())
}

#[test]
fn the_tts_round_trip_passes_speech_that_says_the_words_and_fails_mumbling() {
    let mut fake = FakeTts::new();
    let mut perfect = hears(&SENTENCES);
    assert!(round_trip_error(&mut fake, &mut perfect, &SENTENCES).unwrap().abs() < 1e-12);

    let mut slips = SENTENCES.to_vec();
    slips[0] = "Hello everyone, and welcome back to the dream.";
    let mut close = hears(&slips);
    let wer = round_trip_error(&mut fake, &mut close, &SENTENCES).unwrap();
    assert!(wer > 0.0 && wer < tts::SMOKE_CEILING, "{wer}");

    let mut mumbles = hears(&["um"]);
    let wer = round_trip_error(&mut fake, &mut mumbles, &SENTENCES).unwrap();
    assert!(wer > tts::SMOKE_CEILING, "{wer}");

    let mut wrong_slot = FakeSession::scripted(asr_signature(), vec![]);
    let mut asr = hears(&SENTENCES);
    assert!(round_trip_error(&mut wrong_slot, &mut asr, &SENTENCES).is_err(), "a session of another slot is refused");
    let mut silent = FakeTts::new();
    assert!(round_trip_error(&mut silent, &mut asr, &[""]).is_err(), "no speech is an error, not a score");
}

#[test]
fn a_tts_call_reports_what_the_session_said_and_refuses_bad_audio() {
    let mut fake = FakeTts::new().with_voices(&["warm"]);
    let mut call = TtsCall::new();
    assert_eq!(call.speak(&mut fake, &Utterance::plain("Hi chat!")).unwrap().len(), 11_520);
    let fast = Utterance { speed: 2.0, voice: "warm", ..Utterance::plain("Hi chat!") };
    assert_eq!(call.speak(&mut fake, &fast).unwrap().len(), 5_760);
    assert!(call.speak(&mut fake, &Utterance { voice: "cold", ..Utterance::plain("Hi") }).is_err());
    let nan = f32s(&[f32::NAN], &[1]);
    let mut broken = FakeSession::scripted(tts_signature(), vec![vec![nan]]);
    assert!(call.speak(&mut broken, &Utterance::plain("Hi")).is_err(), "NaN is not speech");
}
