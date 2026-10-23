//! The TTS host and the AI mouth drive together on the mock engine (#1311, `ai-performer-voice-face.md` §4): the
//! host's lead and real play log drive `FakeVoiceToFace`, and every tick lands on the program time its audio played
//! on the `ai_voice` bus, with the mouth following the words. A cut closes it.

#![allow(
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]

use std::sync::Arc;
use std::time::Duration;

use omuzaki_ai_api::{StreamSpec, TensorBuf};
use omuzaki_ai_mock::{FACE_BLOCK, FACE_RATE, FakeTts, FakeVoiceToFace, InlineStream, voice_to_face_signature};
use omuzaki_anim_api::{Channel, Layer};
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::{BusSpec, Channels, GraphSpec, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId};
use omuzaki_audio_api::tap::{DropPolicy, TapFormat, TapPoint, TapRate, TapReader, TapRequest};
use omuzaki_audio_mock::MockEngine;
use omuzaki_core::time::Seconds;
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::speech::{SpeakHow, Voice};
use omuzaki_performer_api::voice::VoiceLead;
use omuzaki_speech::tts::{FEEDS, TtsHost, TtsVoice};
use omuzaki_track_voice::ai_mouth::{EnergyVisemes, MouthConfig, MouthDrive, MouthHandle};

const BLOCK: usize = 240;

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

struct Rig {
    engine: Arc<MockEngine>,
    host: TtsHost,
    mouth: MouthHandle,
    tap: TapReader,
    heard: Vec<f32>,
    /// (stamp, jawOpen) of every released item.
    released: Vec<(f64, f32)>,
}

impl Rig {
    fn new() -> Self {
        let engine = Arc::new(MockEngine::new());
        let graph = GraphSpec {
            buses: vec![bus("ai_voice", &FEEDS[..2]), bus("ai_voice_private", &FEEDS[2..])],
            ..GraphSpec::default()
        };
        engine.apply(&graph).unwrap();
        let request = TapRequest {
            point: TapPoint::BusPostFader(BusId::new("ai_voice").unwrap()),
            format: TapFormat { rate: TapRate::Graph48k, mono: true },
            capacity: Seconds(30.0),
            policy: DropPolicy::DropNewest,
            consumer: "test".into(),
            leaves_device: false,
        };
        let tap = engine.tap_ring(&request).unwrap().0;
        let spec = StreamSpec { sample_rate: FACE_RATE, block: FACE_BLOCK, latency: 0 };
        let (sink, source) = InlineStream::open(Box::new(FakeVoiceToFace::new()), spec, 1, 1_000);
        let drive = MouthDrive::new(
            sink,
            source,
            spec,
            TensorBuf::for_outputs(&voice_to_face_signature()),
            Box::new(EnergyVisemes),
            MouthConfig::default(),
        );
        let mouth = MouthHandle::new(drive);
        let lead: Box<dyn VoiceLead> = Box::new(mouth.clone());
        let voice = TtsVoice::new(VoiceId::new("fake-tts/warm").unwrap());
        let host =
            TtsHost::open(engine.clone() as Arc<dyn AudioEngine>, Box::new(FakeTts::new()), voice, Some(lead)).unwrap();
        Self { engine, host, mouth, tap, heard: Vec::new(), released: Vec::new() }
    }

    /// One 5 ms block: the engine plays, the pump runs, the mouth polls twice (at the block's middle and end).
    fn block(&mut self) {
        assert!(self.host.wait_synthesis(Duration::from_secs(10)));
        self.engine.advance(BLOCK);
        self.host.pump();
        let mut buf = [0.0f32; BLOCK];
        let read = self.tap.read(&mut buf).len;
        self.heard.extend_from_slice(&buf[..read]);
        let now = self.engine.clock().read().time.0;
        let released = &mut self.released;
        for t in [now - 0.0025, now] {
            self.mouth.poll(Seconds(t), &mut |stamp, layer: &Layer| {
                released.push((stamp.0, layer.values.get(Channel::JAW_OPEN)));
            });
        }
    }

    fn run(&mut self, seconds: f64) {
        for _ in 0..(seconds * 48_000.0 / BLOCK as f64).round() as usize {
            self.block();
        }
    }
}

#[test]
fn the_mouth_lands_on_the_audio_the_viewers_hear() {
    let mut rig = Rig::new();
    rig.run(0.1);
    rig.host.speak("Hi chat!", &VoiceId::new("fake-tts/warm").unwrap(), &SpeakHow::default()).unwrap();
    rig.run(1.5);
    let audible = rig.heard.iter().position(|s| s.abs() > 1e-4).unwrap() as f64 / 48_000.0;
    // The first tick is where the bus started playing the voice: its first sample is a zero crossing, so the first
    // audible sample is the next one.
    let start = rig.released[0].0;
    assert!(
        (audible - start) > 0.0 && (audible - start) <= 1.0 / 48_000.0 + 1e-9,
        "first tick {start}, audio {audible}"
    );
    let ticks: Vec<(f64, f32)> = rig.released.iter().map(|&(t, jaw)| (t - start, jaw)).collect();
    // Ticks every 10 ms of voice, on the bus's clock; `H` (0 to 60 ms) at 0.141, `i` (60 to 120 ms) at 0.530.
    let at = |ms: f64| ticks.iter().find(|t| (t.0 * 1000.0 - ms).abs() < 1e-6).map(|t| (t.1 * 1000.0).round() / 1000.0);
    assert_eq!([0.0, 20.0, 40.0, 60.0, 80.0, 100.0].map(at), [0.141, 0.141, 0.141, 0.53, 0.53, 0.53].map(Some));
    assert!(rig.released.windows(2).all(|w| w[1].0 > w[0].0), "stamps rise");
    let stats = rig.mouth.stats();
    assert_eq!((stats.late_keys, stats.refused, stats.history_lost), (0, 0, 0), "{stats:?}");
    // The reply ends: the mouth comes to rest.
    assert_eq!(rig.released.last().unwrap().1, 0.0);
}

#[test]
fn a_cut_closes_the_mouth_after_the_last_word_that_played() {
    let mut rig = Rig::new();
    rig.run(0.1);
    let h =
        rig.host.speak("Hi chat, how are you?", &VoiceId::new("fake-tts/warm").unwrap(), &SpeakHow::default()).unwrap();
    rig.run(0.4);
    h.cut();
    let cut_at = rig.engine.clock().read().time.0;
    rig.run(0.5);
    let start = rig.heard.iter().position(|s| s.abs() > 1e-4).unwrap() as f64 / 48_000.0;
    let silent = rig.heard.iter().rposition(|s| s.abs() > 1e-4).unwrap() as f64 / 48_000.0;
    assert!(silent < cut_at + 0.011, "the audio stopped within a block and the fade");
    // No tick depicts audio after the silence; the close follows, six items, ending at rest.
    let after: Vec<&(f64, f32)> = rig.released.iter().filter(|r| r.0 > silent + 1e-6).collect();
    assert_eq!(after.len(), 6, "{after:?}");
    assert_eq!(after.last().unwrap().1, 0.0);
    assert!(rig.released.iter().filter(|r| r.0 <= silent).count() >= ((silent - start) / 0.010) as usize - 1);
    assert_eq!(rig.mouth.stats().refused, 0);
}
