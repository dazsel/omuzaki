//! A spoken short on the real stack (#2395): Kokoro-82M installed through the registry (after its `tts@1` round trip
//! through the shipped Whisper), each line said through [`TtsSpeaker`], laid out by the length of its sound, and the
//! mouth read from that sound.
//!
//! Needs the pinned files in prepared folders, under the last part of their URLs; nothing is downloaded, and every
//! file is checked against its pinned size and SHA-256:
//! - `OMUZAKI_ORT_CACHE`: the ONNX Runtime archive (`onnxruntime-linux-x64-1.28.3.tgz`);
//! - `OMUZAKI_SPEECH_MODELS`: `ggml-tiny.en.bin` (the round trip's recogniser);
//! - `OMUZAKI_SPEECH_DATA`: the public-domain clips the recogniser's own smoke test reads (`fables_03_05_aesop_64kb.mp3`,
//!   ...);
//! - `OMUZAKI_TTS_MODELS`: Kokoro's files (`model.onnx`, `tokenizer.json`, `us_gold.json`, `gb_gold.json`, the voices).
//!
//! Skips with a message when one is unset. With `OMUZAKI_SHORT_EVIDENCE` set to a folder, writes the voice there as
//! `spoken-short.wav`.

#![allow(clippy::disallowed_methods, reason = "the test finds the model files on this machine")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)] // a panic is a test failure

use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Arc;

use omuzaki_ai::{Registry, RegistrySettings};
use omuzaki_ai_api::{
    FetchError, FileSource, ModelRegistry, NoProgress, PinnedFile, Progress, Sha256, SlotRef, Version,
};
use omuzaki_char_api::CharId;
use omuzaki_short::ShortDef;
use omuzaki_short::encode::wav_bytes;
use omuzaki_short::speech::{SpeechServices, TtsSpeaker};
use omuzaki_short::voice::{RATE, Synthesis, voice_for};
use omuzaki_speech::slots::{asr_slot, tts_slot};
use sha2::Digest as _;

/// Verified files from the prepared folders; never downloads.
struct Folders(Vec<PathBuf>);

impl FileSource for Folders {
    fn get(&self, pin: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
        let name = pin.urls.first().and_then(|url| url.rsplit('/').next()).unwrap_or_default();
        let Some(path) = self.0.iter().map(|dir| dir.join(name)).find(|p| p.is_file()) else {
            return Err(FetchError::Io { path: name.to_owned(), reason: "not in the prepared folders".into() });
        };
        let io = |e: std::io::Error| FetchError::Io { path: name.to_owned(), reason: e.to_string() };
        let size = std::fs::metadata(&path).map_err(io)?.len();
        if size != pin.size {
            return Err(FetchError::Size { url: name.to_owned(), expected: pin.size, actual: size });
        }
        let mut file = std::fs::File::open(&path).map_err(io)?;
        let mut hasher = sha2::Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = file.read(&mut buffer).map_err(io)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let actual = Sha256(hasher.finalize().into());
        if actual != pin.sha256 {
            return Err(FetchError::Hash { url: name.to_owned(), expected: pin.sha256, actual });
        }
        Ok(path)
    }
}

fn folders() -> Option<Folders> {
    let mut dirs = Vec::new();
    for var in ["OMUZAKI_ORT_CACHE", "OMUZAKI_SPEECH_MODELS", "OMUZAKI_SPEECH_DATA", "OMUZAKI_TTS_MODELS"] {
        let Some(dir) = std::env::var_os(var) else {
            eprintln!("SKIPPED: set {var} (see the file's docs)");
            return None;
        };
        dirs.push(PathBuf::from(dir));
    }
    Some(Folders(dirs))
}

fn install(registry: &Registry, slot: &SlotRef, id: &str) {
    let model = registry.models(Some(slot)).into_iter().find(|m| m.model.id.as_str() == id).unwrap().model;
    registry.install(&model, &NoProgress).unwrap_or_else(|e| panic!("installing {id}: {e}"));
}

const SHORT: &str = r#"
character = "kit"

[voice]
kind = "speech"
voice = "kokoro-82m/af_heart"

[[line]]
text = "Hi everyone, {happy} welcome back to the stream!"

[[line]]
text = "Today we are building a tiny wooden boat."
"#;

#[test]
fn a_two_line_short_is_said_by_kokoro_and_timed_by_its_sound() {
    let Some(files) = folders() else { return };
    omuzaki_ai_ort::link();
    omuzaki_short::link();
    let data = std::env::temp_dir().join(format!("omuzaki-short-speech-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).unwrap();
    let registry = Registry::new(&data, Arc::new(files), Version::new(0, 1, 0), RegistrySettings::default()).unwrap();
    install(&registry, &asr_slot(), "whisper-tiny-en");
    install(&registry, &tts_slot(), "kokoro-82m");

    let def = ShortDef::from_toml(SHORT).unwrap();
    let services = SpeechServices { registry: Some(&registry as &dyn ModelRegistry), ..SpeechServices::default() };
    let kit = CharId::new("kit").unwrap();
    let unknown = TtsSpeaker::for_op(&services, &kit, "kokoro-82m/nobody", 1.0).err().expect("an unknown voice");
    assert!(unknown.to_string().contains("nobody"), "{unknown}");
    let mut speaker = TtsSpeaker::for_op(&services, &kit, "kokoro-82m/af_heart", 1.0).unwrap();
    let started = std::time::Instant::now();
    let voice = voice_for(&def, |_| unreachable!("no recording"), Synthesis::Speak(&mut speaker)).unwrap();
    let took = started.elapsed().as_secs_f32();

    let [a, b] = voice.spans[..] else { panic!("{:?}", voice.spans) };
    eprintln!(
        "spans {:.3}..{:.3} and {:.3}..{:.3}, {:.3} s in all, said in {took:.2} s",
        a.start, a.end, b.start, b.end, voice.duration
    );
    assert!(a.len() > 1.0 && a.len() < 6.0 && b.len() > 1.0 && b.len() < 6.0, "{:?}", voice.spans);
    assert!((b.start - (a.end + def.lines[0].pause)).abs() < 1e-4);
    assert!((voice.pcm.len() as f32 / RATE as f32 - voice.duration).abs() < 1e-3);
    let speaking = |from: f32, to: f32| voice.frames.iter().filter(|(t, f)| (from..to).contains(t) && f.voiced).count();
    assert!(speaking(a.start, a.end) > 20 && speaking(b.start, b.end) > 20, "the mouth moves with the words");
    assert_eq!(speaking(b.end + 0.2, voice.duration), 0, "and stops after them");

    if let Some(dir) = std::env::var_os("OMUZAKI_SHORT_EVIDENCE").map(PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spoken-short.wav");
        std::fs::write(&path, wav_bytes(&voice.pcm).unwrap()).unwrap();
        eprintln!("wrote {} ({:.3} s)", path.display(), voice.duration);
    }
    let _ = std::fs::remove_dir_all(&data);
}
