//! The speech host on the engine's voice tap (#1155): the same synthetic utterances pushed straight into
//! `AudioInput`, and played through a `MockEngine` bus into a 16 kHz tap whose sink is the `AudioInput`, give the same
//! speech events (the engine's decimator delays the tap by about a millisecond, well inside one VAD frame).

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod common;

use std::f32::consts::TAU;
use std::time::Duration;

use common::{FakeRecognizer, drain, fake_host, feed_all};
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::{BusSpec, Channels, GraphSpec, SourceRef};
use omuzaki_audio_api::ids::{BusId, FeedId};
use omuzaki_audio_api::tap::{DropPolicy, FeedRequest, TapFormat, TapPoint, TapRate, TapRequest};
use omuzaki_audio_mock::MockEngine;
use omuzaki_core::time::Seconds;
use omuzaki_speech::host::SpeechEvent;
use omuzaki_speech::{HostConfig, SegmenterConfig};

fn config() -> HostConfig {
    let segmenter = SegmenterConfig { min_silence_ms: 200, ..SegmenterConfig::default() };
    HostConfig { segmenter, ..HostConfig::default() }
}

/// Three "utterances" (220 Hz tones of 0.4, 0.8 and 0.3 s) between silences, at `rate`.
fn utterances(rate: u32) -> Vec<f32> {
    let n = |seconds: f32| (seconds * rate as f32) as usize;
    let tone = |seconds: f32| (0..n(seconds)).map(|i| 0.3 * (TAU * 220.0 * i as f32 / rate as f32).sin()).collect();
    let silence = |seconds: f32| vec![0.0; n(seconds)];
    [silence(0.5), tone(0.4), silence(0.6), tone(0.8), silence(0.7), tone(0.3), silence(1.0)].concat()
}

/// What happened, and when to the nearest VAD frame (32 ms).
fn summary(events: &[SpeechEvent]) -> Vec<(&'static str, i64)> {
    let frame = |t: f64| (t / 0.032).round() as i64;
    events
        .iter()
        .map(|e| match e {
            SpeechEvent::SpeechStarted { at } => ("started", frame(*at)),
            SpeechEvent::SpeechEnded { end, .. } => ("ended", frame(*end)),
            SpeechEvent::Final { end, .. } => ("final", frame(*end)),
            SpeechEvent::SpeechRejected { end, .. } => ("rejected", frame(*end)),
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

#[test]
fn the_tap_and_a_direct_push_give_the_same_events() {
    let (mut direct, mut input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    feed_all(&mut input, &utterances(16_000));
    direct.stop();
    let expected = summary(&drain(&direct));

    let engine = MockEngine::new();
    let bus = BusSpec {
        id: BusId::new("mic").unwrap(),
        channels: Channels::Mono,
        sources: vec![SourceRef::Feed { id: FeedId::new("voice").unwrap() }],
        chain: vec![],
        gain_db: 0.0,
        muted: false,
        sends: vec![],
        outputs: vec![],
    };
    engine.apply(&GraphSpec { buses: vec![bus], ..GraphSpec::default() }).unwrap();
    let request = FeedRequest {
        id: FeedId::new("voice").unwrap(),
        rate: 48_000,
        mono: true,
        capacity: Seconds(1.0),
        consumer: "test".into(),
    };
    let mut voice = engine.feed(&request).unwrap();
    let (mut tapped, input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    let tap = TapRequest {
        point: TapPoint::BusPostFader(BusId::new("mic").unwrap()),
        format: TapFormat { rate: TapRate::Speech16k, mono: true },
        capacity: Seconds(1.0),
        policy: DropPolicy::DropNewest,
        consumer: "speech".into(),
        leaves_device: false,
    };
    let (handle, info) = engine.tap(&tap, Box::new(input)).unwrap();
    assert_eq!(info.rate, 16_000);
    for block in utterances(48_000).chunks(480) {
        assert_eq!(voice.push(block), block.len());
        engine.advance(block.len());
    }
    // Ten more milliseconds flush the decimator's delay line.
    engine.advance(480);
    drop(handle);
    let stats = tapped.stop();
    let got = summary(&drain(&tapped));
    assert_eq!(got.len(), expected.len(), "{got:?} vs {expected:?}");
    for (g, e) in got.iter().zip(&expected) {
        assert!(g.0 == e.0 && (g.1 - e.1).abs() <= 1, "{got:?} vs {expected:?}");
    }
    assert_eq!(stats.samples_dropped, 0);
    assert_eq!(got.iter().filter(|e| e.0 == "final").count(), 3, "{got:?}");
}
