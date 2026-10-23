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

/// Kokoro speaking through the TTS host into a mock engine that plays in real time (5 ms blocks on a wall-clock
/// schedule, the pump on its own thread): a turn of four chunks plays gapless with each chunk synthesised while the
/// one before it plays, and what came out of the `ai_voice` bus is understood by Whisper.
#[test]
#[allow(clippy::too_many_lines)]
fn kokoro_speaks_a_turn_through_the_tts_host_in_real_time() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use omuzaki_audio_api::engine::AudioEngine;
    use omuzaki_audio_api::graph::{BusSpec, Channels, GraphSpec, SourceRef};
    use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId};
    use omuzaki_audio_api::tap::{DropPolicy, TapFormat, TapPoint, TapRate, TapRequest};
    use omuzaki_audio_mock::MockEngine;
    use omuzaki_core::time::Seconds;
    use omuzaki_performer_api::ids::VoiceId;
    use omuzaki_performer_api::speech::{SpeakHow, SpeechHandle, SpeechState, Voice};
    use omuzaki_speech::tts::{FEEDS, TtsHost, TtsVoice};

    let Some(files) = tts_folders() else { return };
    let dir = temp("host");
    let registry = registry(files, &dir);
    install(&registry, &asr_slot(), "whisper-tiny-en");
    let kokoro = install(&registry, &tts_slot(), "kokoro-82m");

    let engine = Arc::new(MockEngine::new());
    let bus = BusSpec {
        id: BusId::new("ai_voice").unwrap(),
        channels: Channels::Mono,
        sources: FEEDS[..2].iter().map(|f| SourceRef::Feed { id: FeedId::new(*f).unwrap() }).collect(),
        chain: vec![],
        gain_db: 0.0,
        muted: false,
        sends: vec![],
        outputs: vec![DeviceRole::new("out").unwrap()],
    };
    engine.apply(&GraphSpec { buses: vec![bus], ..GraphSpec::default() }).unwrap();
    let request = TapRequest {
        point: TapPoint::BusPostFader(BusId::new("ai_voice").unwrap()),
        format: TapFormat { rate: TapRate::Graph48k, mono: true },
        capacity: Seconds(60.0),
        policy: DropPolicy::DropNewest,
        consumer: "test".into(),
        leaves_device: false,
    };
    let (mut tap, _) = engine.tap_ring(&request).unwrap();

    let voice = VoiceId::new("kokoro-82m/af_heart").unwrap();
    let session = registry.open(&tts_slot(), Some(&kokoro)).unwrap();
    let mut host =
        TtsHost::open(engine.clone() as Arc<dyn AudioEngine>, session, TtsVoice::new(voice.clone()), None).unwrap();
    host.spawn_pump().unwrap();

    // The engine's audio thread: 5 ms blocks on the wall clock.
    let running = Arc::new(AtomicBool::new(true));
    let audio = {
        let (engine, running) = (Arc::clone(&engine), Arc::clone(&running));
        std::thread::spawn(move || {
            let start = Instant::now();
            let mut blocks = 0u32;
            while running.load(Ordering::Acquire) {
                engine.advance(240);
                blocks += 1;
                let due = start + std::time::Duration::from_micros(5_000 * u64::from(blocks));
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
        })
    };

    let turn = [
        "Hello everyone, and welcome back to the stream.",
        "Today we are going to build a small wooden boat together.",
        "First, we cut the planks to the same length.",
        "Then we glue them side by side and let them dry overnight.",
    ];
    let started = Instant::now();
    let handles: Vec<_> = turn.iter().map(|s| host.speak(s, &voice, &SpeakHow::default()).unwrap()).collect();
    while !handles.iter().all(|h| h.state().is_final()) {
        assert!(started.elapsed().as_secs() < 120, "{:?}", handles.iter().map(SpeechHandle::state).collect::<Vec<_>>());
        std::thread::yield_now();
    }
    running.store(false, Ordering::Release);
    audio.join().unwrap();

    let states: Vec<SpeechState> = handles.iter().map(SpeechHandle::state).collect();
    for pair in states.windows(2) {
        let (SpeechState::Done { .. }, SpeechState::Playing { .. } | SpeechState::Done { .. }) = (pair[0], pair[1])
        else {
            panic!("{states:?}")
        };
    }
    let ends: Vec<f64> = handles
        .iter()
        .zip(&states)
        .map(|(_, s)| match s {
            SpeechState::Done { at, .. } => at.0,
            other => panic!("{other:?}"),
        })
        .collect();
    let counts = host.stats();
    eprintln!(
        "kokoro turn: {} chunks synthesised, {} late, ends at {:?} s",
        counts.synthesised,
        counts.late_chunks,
        ends.iter().map(|t| (t * 100.0).round() / 100.0).collect::<Vec<_>>()
    );
    assert_eq!(counts.synthesised, 4);
    assert_eq!(counts.late_chunks, 0, "every chunk was synthesised while the one before it played");

    let mut heard = vec![0.0f32; tap.available()];
    let read = tap.read(&mut heard).len;
    heard.truncate(read);
    write_wav48(&heard);
    let at16: Vec<f32> = resample(&heard, 48_000, 16_000);
    let first = at16.iter().position(|s| s.abs() > 0.01).unwrap_or(0).saturating_sub(1_600);
    let mut asr = registry.open(&asr_slot(), None).unwrap();
    let mut call = AsrCall::new();
    let mut text = String::new();
    // Whisper takes at most 30 s at once: the turn is about 14 s.
    for piece in at16[first..].chunks(16_000 * 28) {
        text.push_str(&call.transcribe(asr.as_mut(), piece, Some("en")).unwrap().text);
        text.push(' ');
    }
    let expected = spoken(&turn.join(" "));
    let errors = edit_distance(&spoken(&text), &expected);
    eprintln!("heard through the host: {text:?} ({errors} word errors of {})", expected.len());
    assert!(errors <= 2, "{text:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

fn write_wav48(pcm: &[f32]) {
    let Some(dir) = std::env::var_os("OMUZAKI_TTS_EVIDENCE").map(std::path::PathBuf::from) else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(dir.join("kokoro-host-turn.wav"), spec).unwrap();
    for &s in pcm {
        #[allow(clippy::cast_possible_truncation)] // clamped to 16 bits
        wav.write_sample((s * 32767.0).round().clamp(-32768.0, 32767.0) as i16).unwrap();
    }
    wav.finalize().unwrap();
}

/// Barge-in on the real VAD (#1458): Silero holds three frames after the start of speech our own TTS made, and never on
/// the synthetic non-speech set (silence, noise at two levels, mains hum, a whistle, keyboard clicks).
#[test]
fn silero_holds_on_kokoro_speech_and_never_on_the_non_speech_set() {
    use omuzaki_speech::host::SpeechEvent;
    use omuzaki_speech::segmenter::FRAME;
    use omuzaki_speech::slots::vad::non_speech;
    use omuzaki_speech::slots::vad_slot;
    use omuzaki_speech::{HostConfig, SegmenterConfig, SpeechHost};

    let Some(files) = tts_folders() else { return };
    let dir = temp("held");
    let registry = registry(files, &dir);
    install(&registry, &vad_slot(), "silero-vad");
    install(&registry, &asr_slot(), "whisper-tiny-en");
    install(&registry, &tts_slot(), "kokoro-82m");

    let mut tts = registry.open(&tts_slot(), None).unwrap();
    let mut call = TtsCall::new();
    let lines = ["Wait, hold on a second.", "Hey, can I jump in here?", "No no no, that's not right."];
    let silence = |tenths: usize| vec![0.0f32; tenths * 1_600];
    let mut audio = silence(5);
    for line in lines {
        let utterance = Utterance { voice: "af_heart", ..Utterance::plain(line) };
        audio.extend(resample(call.speak(tts.as_mut(), &utterance).unwrap(), TTS_RATE, 16_000));
        audio.extend(silence(10));
    }
    let noise_from = audio.len();
    for sound in non_speech() {
        audio.extend(sound);
        audio.extend(silence(5));
    }

    let config = HostConfig {
        segmenter: SegmenterConfig { min_silence_ms: 300, ..SegmenterConfig::default() },
        ..HostConfig::default()
    };
    let vad = registry.open(&vad_slot(), None).unwrap();
    let asr = registry.open(&asr_slot(), None).unwrap();
    let (mut host, mut input) = SpeechHost::open(vad, asr, &config).unwrap();
    for block in audio.chunks(160) {
        while input.free() < block.len() {
            std::thread::yield_now();
        }
        input.push(block);
    }
    host.stop();
    let events: Vec<SpeechEvent> = std::iter::from_fn(|| host.try_next_event()).collect();
    let frame = FRAME as f64 / 16_000.0;
    let noise_at = noise_from as f64 / 16_000.0;
    let (mut starts, mut helds) = (Vec::new(), Vec::new());
    for event in &events {
        match event {
            SpeechEvent::SpeechStarted { at } => starts.push(*at),
            SpeechEvent::Held { at } => helds.push(*at),
            _ => {}
        }
    }
    let speech_starts: Vec<f64> = starts.iter().copied().filter(|t| *t < noise_at).collect();
    let speech_helds: Vec<f64> = helds.iter().copied().filter(|t| *t < noise_at).collect();
    let noise_starts = starts.len() - speech_starts.len();
    let noise_helds = helds.len() - speech_helds.len();
    eprintln!(
        "held on Kokoro speech: starts {speech_starts:?}, held {speech_helds:?}; non-speech: {noise_starts} starts, \
         {noise_helds} held"
    );
    assert_eq!(speech_helds.len(), lines.len(), "one held per line: {events:?}");
    for (start, held) in speech_starts.iter().zip(&speech_helds) {
        assert!((held - start - 3.0 * frame).abs() < 1e-9, "held at frame 3 of the speech: {start} {held}");
    }
    assert_eq!(noise_helds, 0, "never on the non-speech set: {events:?}");
}
