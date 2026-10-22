//! The live engine on the scripted fake host: the microphone reaches taps on its bus at 48 and 16 kHz, mute silences
//! what honours it, feeds mix in, and a vanished device is marked lost and comes back.

#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // sample counts and test tones

use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_audio_api::device::{Binding, ChannelMap, DeviceState};
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::{BusSpec, Channels, GraphSpec, MIC_BUS, MicMode, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId};
use omuzaki_audio_api::report::{AudioEvent, ClockSource};
use omuzaki_audio_api::tap::{DropPolicy, FeedRequest, TapFormat, TapPoint, TapRate, TapReader, TapRequest};
use omuzaki_audio_engine::LiveEngine;
use omuzaki_audio_mock::backend::{Event, FakeBackend, FakeDevice, InputSource, Script, fake_id};
use omuzaki_core::time::Seconds;

const TONE_HZ: f64 = 440.0;
const TONE_AMPLITUDE: f64 = 0.5;

fn role(s: &str) -> DeviceRole {
    DeviceRole::new(s).unwrap()
}

fn bus_id(s: &str) -> BusId {
    BusId::new(s).unwrap()
}

fn tone(i: u64) -> f32 {
    (TONE_AMPLITUDE * (2.0 * std::f64::consts::PI * TONE_HZ * i as f64 / 48_000.0).sin()) as f32
}

/// A host with one mono microphone playing a 440 Hz tone, and an engine on it with the mic bound.
fn engine_with_mic() -> (FakeBackend, LiveEngine) {
    let mic = FakeDevice::input("mic", "Test microphone", 1)
        .default_device()
        .with_input(InputSource::Function(Arc::new(tone)));
    let backend = FakeBackend::new(vec![mic], Script::new());
    let engine = LiveEngine::new(Arc::new(backend.clone()), None).unwrap();
    let binding =
        Binding::Fixed { id: fake_id("mic"), label: "mic".into(), channels: ChannelMap::mono(0), speakers: false };
    engine.bind(&role("mic"), binding).unwrap();
    (backend, engine)
}

fn bus(id: &str, source: SourceRef) -> BusSpec {
    BusSpec {
        id: bus_id(id),
        channels: Channels::Mono,
        sources: vec![source],
        chain: vec![],
        gain_db: 0.0,
        muted: false,
        sends: vec![],
        outputs: vec![],
    }
}

fn mic_graph() -> GraphSpec {
    GraphSpec { buses: vec![bus(MIC_BUS, SourceRef::Device { role: role("mic") })], ..GraphSpec::default() }
}

fn request(point: TapPoint, rate: TapRate) -> TapRequest {
    TapRequest {
        point,
        format: TapFormat { rate, mono: true },
        capacity: Seconds(2.0),
        policy: DropPolicy::DropNewest,
        consumer: "test".into(),
        leaves_device: false,
    }
}

fn drain(reader: &mut TapReader) -> Vec<f32> {
    let mut all = Vec::new();
    let mut chunk = vec![0.0f32; 4096];
    loop {
        let n = reader.pop_into(&mut chunk);
        if n == 0 {
            return all;
        }
        all.extend_from_slice(&chunk[..n]);
    }
}

fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / samples.len() as f64).sqrt()
}

/// Waits (with a deadline) until `events` carries one that `want` accepts. The housekeeper notices device changes on
/// its own schedule, so the test waits on the event itself, never on a fixed sleep.
fn wait_for(events: &omuzaki_audio_api::report::AudioEvents, want: impl Fn(&AudioEvent) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        while let Some(event) = events.next() {
            if want(&event) {
                return true;
            }
        }
        std::thread::yield_now();
    }
    false
}

#[test]
fn the_microphone_reaches_its_bus_at_both_tap_rates() {
    let (backend, engine) = engine_with_mic();
    engine.apply(&mic_graph()).unwrap();
    let (mut full, info) =
        engine.tap_ring(&request(TapPoint::BusPostFader(bus_id(MIC_BUS)), TapRate::Graph48k)).unwrap();
    let (mut speech, speech_info) =
        engine.tap_ring(&request(TapPoint::BusPostFader(bus_id(MIC_BUS)), TapRate::Speech16k)).unwrap();
    assert_eq!((info.rate, speech_info.rate), (48_000, 16_000));
    assert!(speech_info.capture_offset.0 > info.capture_offset.0, "the decimator's delay is declared");

    backend.step(48_000);
    let status = engine.status();
    assert_eq!(status.clock_source, ClockSource::Device);
    assert!(status.devices.iter().any(|(r, s)| r == &role("mic") && *s == DeviceState::Running), "{status:?}");

    let full = drain(&mut full);
    let speech = drain(&mut speech);
    // One second of input, give or take a callback in flight.
    assert!((47_000..=48_000).contains(&full.len()), "{} samples at 48 kHz", full.len());
    assert!((15_600..=16_000).contains(&speech.len()), "{} samples at 16 kHz", speech.len());
    let expected = TONE_AMPLITUDE / std::f64::consts::SQRT_2;
    let tail = &full[full.len() / 2..];
    assert!((rms(tail) - expected).abs() < 0.01, "48 kHz tap rms {}", rms(tail));
    let tail = &speech[speech.len() / 2..];
    assert!((rms(tail) - expected).abs() < 0.01, "16 kHz tap rms {}", rms(tail));
    let meters = engine.meters(&omuzaki_audio_api::report::MeterPoint::Bus(bus_id(MIC_BUS))).unwrap();
    assert!((meters.peak_db - (20.0 * TONE_AMPLITUDE.log10()) as f32).abs() < 0.2, "{meters:?}");
}

#[test]
fn muting_silences_the_mic_bus_and_unmuting_brings_it_back() {
    let (backend, engine) = engine_with_mic();
    engine.apply(&mic_graph()).unwrap();
    let (mut tap, _) = engine.tap_ring(&request(TapPoint::BusPostFader(bus_id(MIC_BUS)), TapRate::Graph48k)).unwrap();
    backend.step(4_800);
    drain(&mut tap);

    assert_eq!(engine.set_mic_mode(MicMode::Muted).unwrap(), MicMode::Muted);
    backend.step(9_600);
    let muted = drain(&mut tap);
    // The fader ramps down over its fade, then the bus is silent.
    let quiet = &muted[muted.len() / 2..];
    assert!(quiet.iter().all(|s| *s == 0.0), "a muted mic bus is silent (rms {})", rms(quiet));

    engine.set_mic_mode(MicMode::Live).unwrap();
    backend.step(9_600);
    let live = drain(&mut tap);
    assert!(rms(&live[live.len() / 2..]) > 0.3, "the voice is back after unmuting");
}

#[test]
fn a_feed_mixes_into_its_bus() {
    let (backend, engine) = engine_with_mic();
    let tts = bus("tts", SourceRef::Feed { id: FeedId::new("tts").unwrap() });
    engine
        .apply(&GraphSpec {
            buses: vec![bus(MIC_BUS, SourceRef::Device { role: role("mic") }), tts],
            ..GraphSpec::default()
        })
        .unwrap();
    let request_feed = FeedRequest {
        id: FeedId::new("tts").unwrap(),
        rate: 48_000,
        mono: true,
        capacity: Seconds(1.0),
        consumer: "test".into(),
    };
    let mut feed = engine.feed(&request_feed).unwrap();
    let (mut tap, _) = engine.tap_ring(&request(TapPoint::BusPostFader(bus_id("tts")), TapRate::Graph48k)).unwrap();
    let pulse = vec![0.25f32; 24_000];
    assert_eq!(feed.push(&pulse), pulse.len());
    backend.step(48_000);
    let heard = drain(&mut tap);
    let loud = heard.iter().filter(|s| (**s - 0.25).abs() < 1e-3).count();
    assert!(loud >= 23_000, "{loud} of the feed's 24000 samples came through at their level");
}

#[test]
fn a_vanished_microphone_is_lost_then_reconnected() {
    let (backend, engine) = engine_with_mic();
    engine.apply(&mic_graph()).unwrap();
    let events = engine.subscribe();
    backend.step(4_800);
    backend.schedule(backend.now(), Event::Vanish(fake_id("mic")));
    backend.step(256);
    assert!(
        wait_for(&events, |e| matches!(e, AudioEvent::DeviceStateChanged { state: DeviceState::Lost, .. })),
        "the engine marks the mic lost"
    );
    assert_eq!(engine.status().clock_source, ClockSource::Timer, "the timer keeps the graph running");
    backend.schedule(backend.now(), Event::Return(fake_id("mic")));
    backend.step(256);
    assert!(wait_for(&events, |e| matches!(e, AudioEvent::Reconnected { .. })), "the engine reopens the mic");
    assert_eq!(engine.status().clock_source, ClockSource::Device);
}
