//! The TTS host on the mock engine and `FakeTts` (#1309, `ai-performer-voice-face.md` §7 "TTS host"): it passes the
//! performer contract's voice checks; `from` is where the audio actually started, within one block; a cut is silent
//! within one block plus the 2.5 ms fade and the next speech uses the other feed; a resume re-uses the cut PCM with no
//! second synthesis; +4 semitones declares 30,238 Hz and keeps the duration; a late chunk lengthens the pause and is
//! counted; private speech reaches only its own feed; style tags change the model's speed and the gain; the lead is
//! pushed with the PCM and its probe follows the cut.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)] // test arithmetic on small, positive sample counts

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_ai_mock::{FakeTts, SAMPLES_PER_CHAR};
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::{BusSpec, Channels, GraphSpec, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId};
use omuzaki_audio_api::tap::{DropPolicy, TapFormat, TapPoint, TapRate, TapReader, TapRequest};
use omuzaki_audio_mock::MockEngine;
use omuzaki_core::time::Seconds;
use omuzaki_performer_api::conformance::{self, VoiceRig};
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::intent::StyleTag;
use omuzaki_performer_api::speech::{SpeakHow, SpeechHandle, SpeechState, Voice};
use omuzaki_performer_api::voice::{CutProbe, CutState, VoiceLead};
use omuzaki_speech::tts::{FEEDS, StyleControls, TtsHost, TtsVoice, pad_after, trim};

/// The fake engine's block: 5 ms.
const BLOCK: usize = 240;
const RATE: f64 = 48_000.0;

fn voice_id() -> VoiceId {
    VoiceId::new("fake-tts/warm").unwrap()
}

fn bus(id: &str, feeds: &[&str]) -> BusSpec {
    BusSpec {
        id: BusId::new(id).unwrap(),
        channels: Channels::Mono,
        sources: feeds.iter().map(|f| SourceRef::Feed { id: FeedId::new(*f).unwrap() }).collect(),
        chain: vec![],
        gain_db: 0.0,
        muted: false,
        sends: vec![],
        outputs: vec![DeviceRole::new("out").unwrap()],
    }
}

fn tap(engine: &MockEngine, bus: &str) -> TapReader {
    let request = TapRequest {
        point: TapPoint::BusPostFader(BusId::new(bus).unwrap()),
        format: TapFormat { rate: TapRate::Graph48k, mono: true },
        capacity: Seconds(30.0),
        policy: DropPolicy::DropNewest,
        consumer: "test".into(),
        leaves_device: false,
    };
    engine.tap_ring(&request).unwrap().0
}

/// What the lead saw.
#[derive(Default)]
struct Seen {
    attached: Vec<CutProbe>,
    pushed: Vec<(Seconds, u32, usize, bool)>,
    cuts: Vec<Seconds>,
}

struct Lead(Arc<Mutex<Seen>>);

impl VoiceLead for Lead {
    fn attach(&mut self, probe: CutProbe) {
        self.0.lock().unwrap().attached.push(probe);
    }
    fn push(&mut self, at: Seconds, rate: u32, pcm: &[f32], private: bool) {
        self.0.lock().unwrap().pushed.push((at, rate, pcm.len(), private));
    }
    fn cut(&mut self, at: Seconds) {
        self.0.lock().unwrap().cuts.push(at);
    }
}

/// The host on a mock engine whose `ai_voice` and `ai_voice_private` buses are tapped, driven 5 ms at a time.
struct Rig {
    engine: Arc<MockEngine>,
    host: TtsHost,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    public: TapReader,
    private: TapReader,
    heard: Vec<f32>,
    heard_private: Vec<f32>,
    seen: Arc<Mutex<Seen>>,
}

impl Rig {
    fn new() -> Self {
        Self::with(FakeTts::new(), TtsVoice::new(voice_id()))
    }

    fn with(tts: FakeTts, voice: TtsVoice) -> Self {
        let engine = Arc::new(MockEngine::new());
        let graph = GraphSpec {
            buses: vec![bus("ai_voice", &FEEDS[..2]), bus("ai_voice_private", &FEEDS[2..])],
            ..GraphSpec::default()
        };
        engine.apply(&graph).unwrap();
        let public = tap(&engine, "ai_voice");
        let private = tap(&engine, "ai_voice_private");
        let calls = tts.calls();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let lead: Box<dyn VoiceLead> = Box::new(Lead(Arc::clone(&seen)));
        let host = TtsHost::open(engine.clone() as Arc<dyn AudioEngine>, Box::new(tts), voice, Some(lead)).unwrap();
        Self { engine, host, calls, public, private, heard: Vec::new(), heard_private: Vec::new(), seen }
    }

    fn speak(&mut self, text: &str) -> SpeechHandle {
        self.speak_how(text, &SpeakHow::default())
    }

    fn speak_how(&mut self, text: &str, how: &SpeakHow) -> SpeechHandle {
        self.host.speak(text, &voice_id(), how).unwrap()
    }

    /// One 5 ms block: synthesis keeps up, the engine plays, the pump runs.
    fn block(&mut self) {
        assert!(self.host.wait_synthesis(Duration::from_secs(10)));
        self.engine.advance(BLOCK);
        self.host.pump();
        for (reader, out) in [(&mut self.public, &mut self.heard), (&mut self.private, &mut self.heard_private)] {
            let mut buf = [0.0f32; BLOCK];
            let read = reader.read(&mut buf).len;
            out.extend_from_slice(&buf[..read]);
        }
    }

    fn run(&mut self, seconds: f64) {
        for _ in 0..(seconds * RATE / BLOCK as f64).round() as usize {
            self.block();
        }
    }

    fn run_until(&mut self, h: &SpeechHandle, done: fn(&SpeechState) -> bool) -> SpeechState {
        for _ in 0..4_000 {
            if done(&h.state()) {
                return h.state();
            }
            self.block();
        }
        panic!("stuck in {:?}", h.state());
    }

    /// The engine sample now.
    fn now(&self) -> usize {
        self.heard.len()
    }
}

/// The contract's own rig: the host, advanced in 5 ms blocks.
struct ConformanceRig(Rig);

impl VoiceRig for ConformanceRig {
    fn voice(&mut self) -> &mut dyn Voice {
        &mut self.0.host
    }
    fn voice_id(&self) -> VoiceId {
        voice_id()
    }
    fn advance(&mut self, by: Seconds) {
        self.0.run(by.0);
    }
}

#[test]
fn the_host_passes_the_performer_voice_contract() {
    conformance::voice(&mut || Box::new(ConformanceRig(Rig::new()))).unwrap();
}

#[test]
fn a_chunk_reports_where_it_actually_started_within_one_block() {
    let mut rig = Rig::new();
    rig.run(0.1);
    let h = rig.speak("Hi chat!");
    let SpeechState::Playing { from, duration, .. } = rig.run_until(&h, |s| matches!(s, SpeechState::Playing { .. }))
    else {
        unreachable!()
    };
    // "Hi chat!": seven sounding characters of 60 ms, then the silent "!" trimmed to 40 ms.
    let expected = (7 * SAMPLES_PER_CHAR + 960) as f64 / 24_000.0;
    assert!((duration.0 - expected).abs() < 0.001, "duration {} against {expected}", duration.0);
    let SpeechState::Done { at, .. } = rig.run_until(&h, SpeechState::is_final) else { panic!("{:?}", h.state()) };
    let truth = rig.heard.iter().position(|s| s.abs() > 1e-4).unwrap() as f64 / RATE;
    assert!((from.0 - truth).abs() <= BLOCK as f64 / RATE, "from {} against the audio's start {truth}", from.0);
    let pad = pad_after("Hi chat!").0;
    assert!((at.0 - (from.0 + duration.0 + pad)).abs() < 1e-3, "done at {} = from + duration + pad", at.0);
    assert_eq!(rig.host.stats().synthesised, 1);
}

#[test]
fn a_cut_is_silent_within_one_block_and_the_fade_and_the_next_speech_uses_the_other_feed() {
    let mut rig = Rig::new();
    let first = FeedId::new(FEEDS[0]).unwrap();
    assert_eq!(rig.host.active_feed(), first);
    let text = "It took me a whole week to learn and I still fold it wrong.";
    let h = rig.speak(text);
    rig.run_until(&h, |s| matches!(s, SpeechState::Playing { .. }));
    rig.run(0.3);
    assert!(rig.heard[rig.now() - BLOCK..].iter().any(|s| s.abs() > 0.01), "it is speaking");
    let cut_called = rig.now();
    h.cut();
    assert!(matches!(h.state(), SpeechState::Cut { .. }), "a cut is reported at once");
    rig.run(0.2);
    // One block for the audio thread to see the flag, then 120 samples of fade.
    let silent_from = cut_called + BLOCK + 120;
    let loud_after = rig.heard[silent_from..].iter().position(|s| s.abs() > 0.0);
    assert_eq!(loud_after, None, "silent from {silent_from} on");
    let SpeechState::Cut { at, spoken_chars, .. } = h.state() else { panic!("{:?}", h.state()) };
    assert!(at.0 * RATE <= silent_from as f64 + 1.0 && at.0 * RATE > cut_called as f64, "cut at {}", at.0 * RATE);
    let chars = text.chars().count() as u32;
    assert!(spoken_chars > 0 && spoken_chars < chars, "spoke {spoken_chars} of {chars}");
    assert_eq!(rig.host.active_feed(), FeedId::new(FEEDS[1]).unwrap(), "new speech goes to the other feed");
    assert_eq!(rig.host.stats().cuts, 1);

    let next = rig.speak("Anyway, where was I?");
    let SpeechState::Playing { from, .. } = rig.run_until(&next, |s| matches!(s, SpeechState::Playing { .. })) else {
        unreachable!()
    };
    assert!(from.0 > at.0);
    rig.run_until(&next, SpeechState::is_final);
    assert!(matches!(next.state(), SpeechState::Done { .. }));
    let seen = rig.seen.lock().unwrap();
    assert_eq!(seen.cuts.len(), 1, "the lead heard the cut");
    assert_eq!(seen.attached.len(), 2, "one probe per feed used");
    assert!(matches!(seen.attached[0].state(), CutState::Cut { .. }), "the first probe saw its feed cut");
    assert_eq!(seen.attached[1].state(), CutState::Live);
}

#[test]
fn a_resume_re_uses_the_cut_speech_without_a_second_synthesis() {
    let mut rig = Rig::new();
    let text = "Wait, I have to cough.";
    let h = rig.speak(text);
    rig.run_until(&h, |s| matches!(s, SpeechState::Playing { .. }));
    rig.run(0.2);
    h.cut();
    rig.run(0.05);
    assert_eq!(rig.calls.load(Ordering::SeqCst), 1);
    let again = rig.speak(text);
    assert!(matches!(again.state(), SpeechState::Ready { .. }), "ready at once: {:?}", again.state());
    rig.run_until(&again, SpeechState::is_final);
    assert!(matches!(again.state(), SpeechState::Done { .. }));
    assert_eq!(rig.calls.load(Ordering::SeqCst), 1, "no second synthesis");
    assert_eq!(rig.host.stats().resumed, 1);
}

#[test]
fn pitch_is_the_declared_rate_and_keeps_the_duration() {
    let plain = Rig::new();
    let mut voice = TtsVoice::new(voice_id());
    voice.pitch_semitones = 4.0;
    let mut pitched = Rig::with(FakeTts::new(), voice);
    assert_eq!((plain.host.feed_rate(), pitched.host.feed_rate()), (24_000, 30_238));
    let mut plain = plain;
    let durations: Vec<f64> = [&mut plain, &mut pitched]
        .into_iter()
        .map(|rig| {
            let h = rig.speak("Hi chat!");
            match rig.run_until(&h, |s| matches!(s, SpeechState::Ready { .. } | SpeechState::Playing { .. })) {
                SpeechState::Ready { duration, .. } | SpeechState::Playing { duration, .. } => duration.0,
                other => panic!("{other:?}"),
            }
        })
        .collect();
    assert!((durations[0] - durations[1]).abs() / durations[0] < 0.01, "{durations:?}");
}

#[test]
fn a_late_chunk_lengthens_the_pause_and_is_counted() {
    let (go, gate) = std::sync::mpsc::channel();
    let mut rig = Rig::with(FakeTts::new().gated(gate), TtsVoice::new(voice_id()));
    let a = rig.speak("Hi chat!");
    let b = rig.speak("Fold it and see!");
    go.send(()).unwrap(); // only the first synthesis may finish
    // Drive without waiting on synthesis: the second call is held at the gate.
    let step = |rig: &mut Rig| {
        rig.engine.advance(BLOCK);
        rig.host.pump();
        let mut buf = [0.0f32; BLOCK];
        let read = rig.public.read(&mut buf).len;
        rig.heard.extend_from_slice(&buf[..read]);
    };
    while !a.state().is_final() {
        step(&mut rig);
    }
    let SpeechState::Done { at: a_done, .. } = a.state() else { panic!() };
    for _ in 0..20 {
        step(&mut rig); // 100 ms with nothing to play
    }
    assert!(matches!(b.state(), SpeechState::Queued));
    assert_eq!(rig.host.stats().late_chunks, 1);
    drop(go); // a closed gate lets every call through
    let SpeechState::Playing { from, .. } = rig.run_until(&b, |s| matches!(s, SpeechState::Playing { .. })) else {
        unreachable!()
    };
    assert!(from.0 >= a_done.0 + 0.1, "the pause grew: {} after {}", from.0, a_done.0);
    assert_eq!(rig.host.stats().late_chunks, 1, "counted once");
}

#[test]
fn private_speech_reaches_only_its_own_feed_and_moves_no_face() {
    let mut rig = Rig::new();
    let h = rig.speak_how("Just between us.", &SpeakHow::default().private());
    rig.run_until(&h, SpeechState::is_final);
    assert!(matches!(h.state(), SpeechState::Done { .. }));
    assert!(rig.heard.iter().all(|&s| s == 0.0), "nothing on the public bus");
    assert!(rig.heard_private.iter().any(|s| s.abs() > 0.01), "the private bus spoke");
    let seen = rig.seen.lock().unwrap();
    assert!(!seen.pushed.is_empty() && seen.pushed.iter().all(|p| p.3), "the lead is marked private");
}

#[test]
fn style_tags_set_the_speed_expressiveness_and_gain() {
    assert_eq!(StyleControls::of(&[]), StyleControls { speed: 1.0, expressiveness: 0.5, gain_db: 0.0 });
    let whisper = StyleControls::of(&[StyleTag::Whisper]);
    assert_eq!((whisper.expressiveness, whisper.gain_db), (0.2, -8.0));
    let both = StyleControls::of(&[StyleTag::Excited, StyleTag::Fast]);
    assert!((both.speed - 1.08 * 1.15).abs() < 1e-6 && (both.expressiveness - 0.8).abs() < 1e-6);
    let voice = TtsVoice::new(voice_id());
    assert_eq!(voice.model_speed(&StyleControls::of(&[StyleTag::Fast, StyleTag::Fast, StyleTag::Fast])), 1.4);

    let mut rig = Rig::new();
    let plain = rig.speak("Hello there.");
    rig.run_until(&plain, SpeechState::is_final);
    let loud = rig.heard.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let mut rig = Rig::new();
    let quiet = rig.speak_how("Hello there.", &SpeakHow::new(vec![StyleTag::Whisper]));
    rig.run_until(&quiet, SpeechState::is_final);
    let soft = rig.heard.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let db = 20.0 * (soft / loud).log10();
    assert!((db + 8.0).abs() < 0.1, "whisper is {db} dB");
    // Whisper is also slower: 0.9 of the speed, so longer.
    let (SpeechState::Done { .. }, SpeechState::Done { .. }) = (plain.state(), quiet.state()) else { panic!() };
}

#[test]
fn the_lead_gets_every_sample_ahead_of_playback() {
    let mut rig = Rig::new();
    let h = rig.speak("Hi chat!");
    let SpeechState::Ready { duration, .. } = rig.run_until(&h, |s| matches!(s, SpeechState::Ready { .. })) else {
        unreachable!()
    };
    rig.run_until(&h, SpeechState::is_final);
    let seen = rig.seen.lock().unwrap();
    let pushed: usize = seen.pushed.iter().map(|p| p.2).sum();
    assert_eq!(pushed, (duration.0 * 24_000.0).round() as usize, "every PCM sample, once");
    assert!(seen.pushed.iter().all(|p| p.1 == 24_000 && !p.3));
    assert!(seen.pushed.windows(2).all(|w| w[1].0.0 >= w[0].0.0), "stamps never go back");
    assert_eq!(seen.attached.len(), 1);
}

#[test]
fn trimming_keeps_40_ms_around_the_speech_and_pads_follow_punctuation() {
    let mut pcm = vec![0.0f32; 24_000];
    pcm[10_000..12_000].fill(0.5);
    let kept = trim(&pcm, 24_000);
    assert_eq!(kept.len(), 2_000 + 2 * 960);
    assert_eq!(trim(&[0.0; 100], 24_000), &[] as &[f32]);
    assert_eq!(pad_after("Done."), Seconds(0.18));
    assert_eq!(pad_after("Really?! "), Seconds(0.18));
    assert_eq!(pad_after("First,"), Seconds(0.08));
    assert_eq!(pad_after("and then"), Seconds(0.0));
}
