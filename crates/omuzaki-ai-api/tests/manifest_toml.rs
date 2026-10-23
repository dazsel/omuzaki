//! The `model.toml` format as people and the shipped catalogue write it: it parses, validates by origin, round-trips,
//! and has a digest that covers the files and nothing else.

use omuzaki_ai_api::{
    Better, LineageKind, ManifestError, ModelManifest, Origin, Provider, StreamSpec, TestSetKind, Version,
};

/// Silero VAD as `omuzaki-speech` will ship it (the pin is the one in the speech host's catalogue).
const SILERO: &str = r#"
id = "silero-vad"
version = "6.2.3"
name = "Silero VAD"
slot = "vad@1"
runtime = "onnx"
min_app_version = "0.1.0"
providers = ["cpu"]

[[files]]
name = "silero_vad.onnx"
sha256 = "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"
size = 2327524
urls = [
  "https://raw.githubusercontent.com/snakers4/silero-vad/5cd7945676eb32225748052e2e6a0580e4686a08/src/silero_vad/data/silero_vad.onnx",
  "https://mirror.invalid/silero-vad/6.2.3/silero_vad.onnx",
]

[card]
register = "Silero VAD"
authors = "Silero Team"
homepage = "https://github.com/snakers4/silero-vad"
opset = 16

[card.licence]
spdx = "MIT"

[[card.lineage]]
kind = "code"
name = "silero-vad v6.2.3"
licence = "MIT"

[[card.metrics]]
name = "golden_max_abs_diff"
value = 6e-8
better = "lower"
set = { id = "aesop-52-frames-1", kind = "real" }
"#;

/// A voice-conversion model a person adds (ML-14): streaming, GPU first, licence as its author states it.
const USER_VC: &str = r#"
id = "user-my-rvc-voice"
version = "0.0.0"
name = "My RVC voice"
slot = "voice_conversion@1"
runtime = "onnx"
providers = ["cuda", "directml", "cpu"]

[[files]]
name = "voice.onnx"
sha256 = "abababababababababababababababababababababababababababababababab"
size = 110000000

[stream]
sample_rate = 40000
block = 4000
latency = 2000

[card]
imitates = "Nobody Real"

[card.licence]
text = "personal use only"
"#;

#[test]
fn a_shipped_manifest_parses_validates_and_round_trips() {
    let m = ModelManifest::from_toml(SILERO).unwrap();
    assert_eq!(m.version, Version::new(6, 2, 3));
    assert_eq!((m.slot.slot.as_str(), m.slot.version), ("vad", 1));
    assert_eq!(m.providers, vec![Provider::Cpu]);
    assert_eq!(m.card.lineage[0].kind, LineageKind::Code);
    assert_eq!((m.card.metrics[0].better, m.card.metrics[0].set.kind), (Better::Lower, TestSetKind::Real));
    assert_eq!(m.validate(Origin::Shipped), Ok(()));
    assert_eq!(m.size(), 2_327_524);
    let pin = m.files[0].pin().unwrap();
    assert_eq!((pin.size, pin.sha256, pin.urls.len()), (m.files[0].size, m.files[0].sha256, 2));
    assert!(pin.urls[0].contains("snakers4"), "the upstream is tried first, the mirror after it");
    assert!(!pin.resumable(), "2 MB is downloaded in one go");

    let again = ModelManifest::from_toml(&m.to_toml().unwrap()).unwrap();
    assert_eq!(again, m);
}

#[test]
fn a_streaming_user_manifest_keeps_its_stream_and_declared_terms() {
    let m = ModelManifest::from_toml(USER_VC).unwrap();
    assert_eq!(m.stream, Some(StreamSpec { sample_rate: 40_000, block: 4_000, latency: 2_000 }));
    assert_eq!(m.card.imitates.as_deref(), Some("Nobody Real"));
    assert!(!m.card.licence.is_unknown());
    assert_eq!(m.validate(Origin::User), Ok(()));
    // As a shipped manifest it would be refused: no URL, no register row.
    let errors = m.validate(Origin::Shipped).unwrap_err();
    assert!(errors.contains(&ManifestError::NoUrl("voice.onnx".into())));
    assert!(errors.contains(&ManifestError::Unregistered));
}

#[test]
fn user_manifests_are_refused_for_urls_unsafe_names_and_shipped_ids() {
    let mut m = ModelManifest::from_toml(USER_VC).unwrap();
    m.id = "my-rvc-voice".parse().unwrap();
    m.files[0].urls = vec!["https://example.com/voice.onnx".into()];
    m.files.push(m.files[0].clone());
    m.files[1].name = "../evil.onnx".into();
    m.files[1].urls.clear();
    m.files[1].size = 0;
    m.stream = Some(StreamSpec { sample_rate: 40_000, block: 0, latency: 0 });
    m.io.insert("audio".into(), String::new());
    let errors = m.validate(Origin::User).unwrap_err();
    for expected in [
        ManifestError::UserId("my-rvc-voice".into()),
        ManifestError::UserUrl("voice.onnx".into()),
        ManifestError::FileName("../evil.onnx".into()),
        ManifestError::EmptyFile("../evil.onnx".into()),
        ManifestError::Stream,
        ManifestError::Io("audio".into()),
    ] {
        assert!(errors.contains(&expected), "missing {expected:?} in {errors:?}");
    }
}

/// The `MediaPipe` face pipeline as two stages (detector, then landmarks with blendshapes) behind one session.
const STAGED: &str = r#"
id = "mediapipe-face"
version = "0.10.14"
name = "MediaPipe face"
slot = "face_tracker@1"
runtime = "onnx"

[[files]]
name = "face_detector.onnx"
sha256 = "0101010101010101010101010101010101010101010101010101010101010101"
size = 230000
urls = ["https://example.invalid/face_detector.onnx"]

[[files]]
name = "face_landmarks_detector.onnx"
sha256 = "0202020202020202020202020202020202020202020202020202020202020202"
size = 2400000
urls = ["https://example.invalid/face_landmarks_detector.onnx"]

[[stages]]
name = "detector"
file = "face_detector.onnx"
inputs = { input = "image" }

[[stages]]
name = "landmarks"
file = "face_landmarks_detector.onnx"
inputs = { input_12 = "detector.crop" }

[io]
blendshapes = "landmarks.blendshapes"
head = "landmarks.head"

[card]
register = "ONNX conversions of face_landmarker.task"

[card.licence]
spdx = "Apache-2.0"
"#;

#[test]
fn a_staged_manifest_runs_its_graphs_in_order_and_maps_outputs_into_stages() {
    let m = ModelManifest::from_toml(STAGED).unwrap();
    assert_eq!(m.stages.len(), 2);
    assert_eq!(m.stages[1].inputs["input_12"], "detector.crop");
    assert_eq!(m.validate(Origin::Shipped), Ok(()));
    assert_eq!(ModelManifest::from_toml(&m.to_toml().unwrap()).unwrap(), m);

    let mut bad = m.clone();
    bad.stages[0].inputs.insert("extra".into(), "landmarks.score".into()); // a later stage
    bad.stages[1].file = "missing.onnx".into();
    bad.stages.push(bad.stages[0].clone()); // a repeated name
    bad.io.insert("score".into(), "score".into()); // not <stage>.<port>
    let errors = bad.validate(Origin::Shipped).unwrap_err();
    for expected in [
        ManifestError::StageSource { stage: "detector".into(), reads: "landmarks.score".into() },
        ManifestError::StageFile { stage: "landmarks".into(), file: "missing.onnx".into() },
        ManifestError::StageName("detector".into()),
        ManifestError::StageOutput("score".into()),
    ] {
        assert!(errors.contains(&expected), "missing {expected:?} in {errors:?}");
    }
}

#[test]
fn shipped_models_keep_a_cpu_path() {
    let mut m = ModelManifest::from_toml(SILERO).unwrap();
    m.providers = vec![Provider::Cuda];
    assert_eq!(m.validate(Origin::Shipped), Err(vec![ManifestError::NoCpu]));
}

#[test]
fn the_digest_covers_the_files_in_name_order_and_nothing_else() {
    let mut m = ModelManifest::from_toml(USER_VC).unwrap();
    let mut extra = m.files[0].clone();
    extra.name = "index.bin".into();
    m.files.push(extra);
    let text = m.digest_text();
    assert!(text.starts_with(&format!("{} 110000000 index.bin\n", m.files[0].sha256)), "{text}");
    let renamed = ModelManifest { name: "another name".into(), ..m.clone() };
    assert_eq!(renamed.digest_text(), text, "the card and name are not identity");
    m.files[1].size += 1;
    assert_ne!(m.digest_text(), text);
}

#[test]
fn unreadable_manifests_say_so() {
    assert!(matches!(ModelManifest::from_toml("id = 3"), Err(ManifestError::Parse(_))));
    let bad_slot = SILERO.replace("vad@1", "vad");
    assert!(matches!(ModelManifest::from_toml(&bad_slot), Err(ManifestError::Parse(e)) if e.contains("vad@1")));
}

/// A text-to-speech model a person adds, with its controls and voices (`ai-performer-voice-face.md` §3.4).
const USER_TTS: &str = r#"
id = "user-narrator"
version = "1.0.0"
name = "Narrator"
slot = "tts@1"
runtime = "onnx"
providers = ["cpu"]

[[files]]
name = "narrator.onnx"
sha256 = "abababababababababababababababababababababababababababababababab"
size = 1000

[[files]]
name = "deep.bin"
sha256 = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"
size = 10

[tts]
honours = ["speed", "expressiveness"]

[[voices]]
id = "plain"
languages = ["en-US"]

[[voices]]
id = "deep"
languages = ["en-US", "en-GB"]
file = "deep.bin"

[[voices]]
id = "like-a-star"
imitates = "A Famous Person"

[[voices]]
id = "me"
cloned = true

[card.licence]
text = "my own recordings"
"#;

#[test]
fn a_tts_manifest_lists_its_voices_and_which_need_consent() {
    let m = ModelManifest::from_toml(USER_TTS).unwrap();
    m.validate(Origin::User).unwrap();
    let ids: Vec<&str> = m.voices.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(ids, ["plain", "deep", "like-a-star", "me"]);
    assert_eq!(m.voices[1].languages, ["en-US", "en-GB"]);
    assert_eq!(m.voices[1].file.as_deref(), Some("deep.bin"));
    let consent: Vec<bool> = m.voices.iter().map(omuzaki_ai_api::VoiceRow::needs_consent).collect();
    assert_eq!(consent, [false, false, true, true], "imitating a real person or cloned from a recording needs consent");
    let tts = m.tts.as_ref().unwrap();
    assert!(tts.honours("speed") && tts.honours("expressiveness") && !tts.honours("pitch"));
    assert_eq!(ModelManifest::from_toml(&m.to_toml().unwrap()).unwrap(), m, "round-trips");

    // A model without voices writes none, and old manifests read as before.
    let silero = ModelManifest::from_toml(SILERO).unwrap();
    assert!(silero.voices.is_empty() && silero.tts.is_none());
    assert!(!silero.to_toml().unwrap().contains("voices"));

    let broken =
        USER_TTS.replace("id = \"me\"", "id = \"plain\"").replace("file = \"deep.bin\"", "file = \"gone.bin\"");
    let errors = ModelManifest::from_toml(&broken).unwrap().validate(Origin::User).unwrap_err();
    assert!(errors.contains(&ManifestError::VoiceId("plain".into())), "{errors:?}");
    assert!(errors.contains(&ManifestError::VoiceFile { voice: "deep".into(), file: "gone.bin".into() }), "{errors:?}");
    let spaced = USER_TTS.replace("id = \"plain\"", "id = \"two words\"");
    let errors = ModelManifest::from_toml(&spaced).unwrap().validate(Origin::User).unwrap_err();
    assert_eq!(errors, [ManifestError::VoiceId("two words".into())]);
    assert!(ModelManifest::from_toml(&USER_TTS.replace("cloned = true", "cloned = true\nage = 3")).is_err());
}
