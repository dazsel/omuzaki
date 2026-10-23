//! The live engine on the scripted fake host: the microphone reaches taps on its bus at 48 and 16 kHz, mute silences
//! what honours it, feeds mix in, a vanished device is marked lost and comes back, and the mouth voice tap (what lip
//! sync reads) carries the mic.

#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // sample counts and test tones

use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_audio_api::device::{Binding, ChannelMap, DeviceState};
use omuzaki_audio_api::engine::{AudioEngine, AudioError};
use omuzaki_audio_api::graph::{BusSpec, Channels, GraphSpec, MIC_BUS, MicMode, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId};
use omuzaki_audio_api::report::{AudioEvent, ClockSource};
use omuzaki_audio_api::tap::{DropPolicy, FeedRequest, TapFormat, TapPoint, TapRate, TapReader, TapRequest, VoiceUse};
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

#[test]
fn the_mouth_voice_tap_carries_the_microphone_and_honours_mute() {
    // What lip sync reads (Tracking's `VoiceListener`): the PF-12 `Voice(Mouth)` tap, resolved to the heard mic.
    let (backend, engine) = engine_with_mic();
    engine.apply(&mic_graph()).unwrap();
    let (mut mouth, info) = engine.tap_ring(&request(TapPoint::Voice(VoiceUse::Mouth), TapRate::Graph48k)).unwrap();
    assert_eq!(info.rate, 48_000);
    backend.step(48_000);
    let heard = drain(&mut mouth);
    assert!(heard.len() >= 47_000, "{} samples", heard.len());
    let expected = TONE_AMPLITUDE / std::f64::consts::SQRT_2;
    assert!((rms(&heard[heard.len() / 2..]) - expected).abs() < 0.01, "mouth tap rms {}", rms(&heard));

    engine.set_mic_mode(MicMode::Muted).unwrap();
    backend.step(48_000);
    let muted = drain(&mut mouth);
    assert!(rms(&muted[muted.len() / 2..]) < 1e-4, "the mouth tap is silent while muted: {}", rms(&muted));
}

#[test]
fn a_loaded_sound_plays_on_its_player_and_unloads_without_a_trace() {
    use omuzaki_audio_api::ids::{PlayerId, SoundId};
    use omuzaki_audio_api::player::Sound;

    let (backend, engine) = engine_with_mic();
    let board = PlayerId::new("board").unwrap();
    let mut sfx = bus("sfx", SourceRef::Player { id: board.clone() });
    sfx.channels = Channels::Stereo;
    engine.apply(&GraphSpec { buses: vec![sfx], ..GraphSpec::default() }).unwrap();
    // A new bus fades in over its first 64 samples; let it.
    backend.step(1_024);
    let ding = SoundId::new("ding").unwrap();
    let sound = Arc::new(Sound::new(vec![vec![0.5f32; 12_000], vec![0.5f32; 12_000]]).unwrap());
    engine.load_sound(&ding, Arc::clone(&sound)).unwrap();
    let missing = SoundId::new("gong").unwrap();
    assert!(matches!(engine.play(&board, &missing, 0.0, false), Err(AudioError::UnknownSound(_))));

    let (mut tap, _) = engine.tap_ring(&request(TapPoint::BusPostFader(bus_id("sfx")), TapRate::Graph48k)).unwrap();
    engine.play(&board, &ding, -6.0, false).unwrap();
    backend.step(24_000);
    let heard = drain(&mut tap);
    // The mono tap down-mixes the stereo bus: (L + R) / sqrt(2).
    let side = 0.5 * omuzaki_audio_api::semantics::db_to_gain(-6.0);
    let level = 2.0 * side * omuzaki_audio_api::semantics::MONO_TO_STEREO_GAIN;
    let loud = heard.iter().filter(|s| (**s - level).abs() < 1e-4).count();
    assert_eq!(loud, 12_000, "every frame of the sound, at its gain");

    // Unloaded, it can't be played, and the engine lets go of it: the audio thread hands it to the housekeeper,
    // which drops it, leaving only the test's reference.
    engine.unload_sound(&ding).unwrap();
    assert!(matches!(engine.play(&board, &ding, 0.0, false), Err(AudioError::UnknownSound(_))));
    backend.step(1_024);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Arc::strong_count(&sound) > 1 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(Arc::strong_count(&sound), 1, "nobody else holds the unloaded sound");
    // The id loads again, into a slot the bank has freed.
    engine.load_sound(&ding, Arc::new(Sound::new(vec![vec![0.0; 10]]).unwrap())).unwrap();
    engine.play(&board, &ding, 0.0, false).unwrap();
}

/// The engine on a host whose callbacks are `block` frames, with a mono bus `tts` fed by the feed `tts`, a 48 kHz tap
/// on that bus, and the feed opened at `rate` Hz.
fn feed_rig(block: u32, rate: u32) -> (FakeBackend, LiveEngine, omuzaki_audio_api::tap::FeedWriter, TapReader, u64) {
    let mic = FakeDevice::input("mic", "Test microphone", 1)
        .default_device()
        .with_callback_frames(block)
        .with_input(InputSource::Function(Arc::new(tone)));
    let backend = FakeBackend::new(vec![mic], Script::new());
    let engine = LiveEngine::new(Arc::new(backend.clone()), None).unwrap();
    let binding =
        Binding::Fixed { id: fake_id("mic"), label: "mic".into(), channels: ChannelMap::mono(0), speakers: false };
    engine.bind(&role("mic"), binding).unwrap();
    let tts = bus("tts", SourceRef::Feed { id: FeedId::new("tts").unwrap() });
    engine
        .apply(&GraphSpec {
            buses: vec![bus(MIC_BUS, SourceRef::Device { role: role("mic") }), tts],
            ..GraphSpec::default()
        })
        .unwrap();
    let wanted = FeedRequest {
        id: FeedId::new("tts").unwrap(),
        rate,
        mono: true,
        capacity: Seconds(1.0),
        consumer: "test".into(),
    };
    let feed = engine.feed(&wanted).unwrap();
    let (tap, info) = engine.tap_ring(&request(TapPoint::BusPostFader(bus_id("tts")), TapRate::Graph48k)).unwrap();
    (backend, engine, feed, tap, info.first_sample)
}

/// #1248: the playhead says on which engine sample every feed sample played, exactly, at the block sizes a host uses
/// and at a TTS voice's 24 kHz as well as 48 kHz.
#[test]
fn the_feed_playhead_places_every_sample_exactly_at_any_block_size() {
    for block in [64u32, 256, 1024] {
        for rate in [48_000u32, 24_000, 30_238] {
            let (backend, _engine, mut feed, mut tap, first) = feed_rig(block, rate);
            let mut pcm = vec![0.0f32; 6_000];
            pcm[4_321] = 1.0;
            assert_eq!(feed.push(&pcm), pcm.len());
            backend.step(48_000);
            let heard = drain(&mut tap);
            let peak = heard.iter().enumerate().fold((0, 0.0f32), |m, (i, &s)| if s > m.1 { (i, s) } else { m }).0;
            let placed = feed.playhead().place(4_321, 0).unwrap();
            assert_eq!(placed, first + peak as u64, "block {block}, {rate} Hz");
        }
    }
}

/// #1248: a close from another thread silences the feed within one block plus the 120-sample fade, with no sample
/// above the fade's envelope, and the playhead says where the silence began.
#[test]
fn a_closed_feed_is_silent_within_one_block_plus_the_fade() {
    for block in [64u32, 256, 1024] {
        let (backend, _engine, mut feed, mut tap, first) = feed_rig(block, 24_000);
        assert_eq!(feed.push(&vec![0.5f32; 20_000]), 20_000);
        backend.step(4_800);
        let closer = feed.closer();
        std::thread::spawn(move || closer.close()).join().unwrap();
        let raised_at = first + drain(&mut tap).len() as u64;
        backend.step(4_800);
        let after = drain(&mut tap);
        let cut = feed.playhead().cut_at_sample();
        assert!(cut != u64::MAX, "block {block}: the feed was cut");
        let within = cut - raised_at;
        assert!(within <= u64::from(block) + 120, "block {block}: silent {within} samples after the close");
        let fade_start = cut - 120 - raised_at;
        for (k, &s) in after[fade_start as usize..(cut - raised_at) as usize].iter().enumerate() {
            let envelope = 0.5 * (1.0 - (k as f32 + 1.0) / 120.0);
            assert!(s <= envelope + 1e-6, "block {block}: fade sample {k} is {s}, above {envelope}");
        }
        assert!(after[(cut - raised_at) as usize..].iter().all(|&s| s == 0.0), "block {block}: silent after the cut");
    }
}
