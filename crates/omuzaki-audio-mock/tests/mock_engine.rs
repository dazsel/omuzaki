//! The mock `AudioEngine`: its stored graph and live setters, real tap and feed rings driven by `advance`, meters,
//! mic modes and events.

#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure
#![allow(clippy::float_cmp)] // silent paths and exact landings are exact by construction

use std::collections::BTreeMap;

use omuzaki_audio_api::device::{Binding, ChannelMap, DeviceId};
use omuzaki_audio_api::engine::{AudioEngine, AudioError};
use omuzaki_audio_api::graph::{BusSpec, Channels, GraphError, GraphSpec, MicMode, NodeSpec, SendSpec, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId, NodeId, NodeKindId};
use omuzaki_audio_api::node::{ConfigValue, NodeConfig, ParamIndex};
use omuzaki_audio_api::report::{AudioEvent, MeterPoint, RouteEnd};
use omuzaki_audio_api::tap::{DropPolicy, FeedRequest, TapFormat, TapPoint, TapRate, TapRequest, VoiceUse};
use omuzaki_audio_mock::MockEngine;
use omuzaki_core::time::Seconds;

fn bus_id(s: &str) -> BusId {
    BusId::new(s).unwrap()
}

fn role(s: &str) -> DeviceRole {
    DeviceRole::new(s).unwrap()
}

fn feed_id(s: &str) -> FeedId {
    FeedId::new(s).unwrap()
}

fn bus(id: &str, source: Option<SourceRef>) -> BusSpec {
    BusSpec {
        id: bus_id(id),
        channels: Channels::Mono,
        sources: source.into_iter().collect(),
        chain: vec![],
        gain_db: 0.0,
        muted: false,
        sends: vec![],
        outputs: vec![],
    }
}

#[allow(clippy::unnecessary_wraps)] // reads as the `source` argument of `bus`
fn feed_source(id: &str) -> Option<SourceRef> {
    Some(SourceRef::Feed { id: feed_id(id) })
}

fn null_node(latency: i64) -> NodeSpec {
    let mut config = NodeConfig::default();
    config.0.insert("latency".into(), ConfigValue::Int(latency));
    NodeSpec {
        id: NodeId::new("n").unwrap(),
        kind: NodeKindId::new("mock.null").unwrap(),
        config,
        params: BTreeMap::default(),
        enabled: true,
        sidechain: None,
    }
}

fn feed_request(id: &str, mono: bool) -> FeedRequest {
    FeedRequest { id: feed_id(id), rate: 48_000, mono, capacity: Seconds(1.0), consumer: "test".into() }
}

fn ring_request(point: TapPoint) -> TapRequest {
    TapRequest {
        point,
        format: TapFormat { rate: TapRate::Graph48k, mono: true },
        capacity: Seconds(1.0),
        policy: DropPolicy::DropNewest,
        consumer: "test".into(),
        leaves_device: false,
    }
}

/// One mono bus `a` fed by the feed `f`, playing on the role `out`.
fn one_bus() -> GraphSpec {
    let mut a = bus("a", feed_source("f"));
    a.outputs = vec![role("out")];
    GraphSpec { buses: vec![a], ..GraphSpec::default() }
}

#[test]
fn the_devices_are_a_fixed_list_and_binding_checks_the_id() {
    let engine = MockEngine::new();
    let devices = engine.devices().unwrap();
    assert_eq!(devices.len(), 4);
    let known =
        Binding::Fixed { id: devices[0].id.clone(), label: "x".into(), channels: ChannelMap::mono(0), speakers: false };
    let events = engine.subscribe();
    engine.bind(&role("voice"), known).unwrap();
    assert!(matches!(events.next(), Some(AudioEvent::DeviceStateChanged { .. })));
    assert_eq!(engine.status().devices.len(), 1);
    let ghost = Binding::Fixed {
        id: DeviceId { host: "mock".into(), id: "ghost".into() },
        label: "x".into(),
        channels: ChannelMap::mono(0),
        speakers: false,
    };
    assert!(matches!(engine.bind(&role("other"), ghost), Err(AudioError::Backend(_))));
    engine.unbind(&role("voice")).unwrap();
    engine.unbind(&role("voice")).unwrap();
    assert_eq!(engine.status().devices.len(), 0);
}

#[test]
fn apply_stores_the_graph_and_a_bad_graph_changes_nothing() {
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    assert_eq!(engine.graph(), one_bus());
    let mut a = bus("a", None);
    a.sends = vec![SendSpec { to: bus_id("a"), gain_db: 0.0, pre_fader: false }];
    let bad = GraphSpec { buses: vec![a], ..GraphSpec::default() };
    assert!(matches!(engine.apply(&bad), Err(AudioError::Graph(GraphError::Cycle(_)))));
    assert_eq!(engine.graph(), one_bus(), "the running graph is untouched on error");
}

#[test]
fn the_live_setters_update_the_graph_and_check_their_arguments() {
    let engine = MockEngine::new();
    let mut a = one_bus();
    a.buses[0].chain = vec![null_node(0)];
    engine.apply(&a).unwrap();
    engine.set_bus_gain_db(&bus_id("a"), -6.0).unwrap();
    engine.set_bus_muted(&bus_id("a"), true).unwrap();
    engine.set_param(&bus_id("a"), &NodeId::new("n").unwrap(), ParamIndex(0), -3.0).unwrap();
    let graph = engine.graph();
    assert_eq!((graph.buses[0].gain_db, graph.buses[0].muted), (-6.0, true));
    assert_eq!(graph.buses[0].chain[0].params["gain_db"], -3.0);

    let ghost = bus_id("ghost");
    assert!(matches!(engine.set_bus_gain_db(&ghost, 0.0), Err(AudioError::UnknownBus(_))));
    assert!(matches!(engine.set_bus_muted(&ghost, true), Err(AudioError::UnknownBus(_))));
    assert!(matches!(
        engine.set_bus_gain_db(&bus_id("a"), 13.0),
        Err(AudioError::Graph(GraphError::GainOutOfRange { .. }))
    ));
    assert!(matches!(
        engine.set_param(&bus_id("a"), &NodeId::new("zzz").unwrap(), ParamIndex(0), 0.0),
        Err(AudioError::UnknownNode { .. })
    ));
    assert!(matches!(
        engine.set_param(&bus_id("a"), &NodeId::new("n").unwrap(), ParamIndex(9), 0.0),
        Err(AudioError::UnknownParam { index: 9, .. })
    ));
}

#[test]
fn a_feed_plays_through_the_graph_into_a_tap_ring() {
    // Feed 0.5, bus gain 0 dB: the post-fader tap reads 0.5 for every sample, the stream starts at sample 0.
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    let mut writer = engine.feed(&feed_request("f", true)).unwrap();
    let (mut reader, info) = engine.tap_ring(&ring_request(TapPoint::BusPostFader(bus_id("a")))).unwrap();
    assert_eq!((info.first_sample, info.rate, info.honours_mute), (0, 48_000, true));
    assert_eq!(writer.push(&[0.5; 480]), 480);
    engine.advance(480);
    let mut out = [0.0f32; 600];
    let read = reader.read(&mut out);
    assert_eq!((read.len, read.start_sample, read.gap_before), (480, 0, 0));
    assert!(out[..480].iter().all(|&s| s == 0.5));
    assert_eq!(engine.clock().read().sample, 480);
    assert!((engine.clock().time_of(480).0 - 0.01).abs() < 1e-12);
}

#[test]
fn a_short_feed_plays_silence_and_is_counted() {
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    let mut writer = engine.feed(&feed_request("f", true)).unwrap();
    let (mut reader, _) = engine.tap_ring(&ring_request(TapPoint::BusPostFader(bus_id("a")))).unwrap();
    writer.push(&[0.5; 100]);
    engine.advance(300);
    let mut out = [1.0f32; 300];
    assert_eq!(reader.read(&mut out).len, 300);
    assert!(out[..100].iter().all(|&s| s == 0.5) && out[100..].iter().all(|&s| s == 0.0));
    assert_eq!(engine.feed_underruns(&feed_id("f")), Some(200));
    drop(writer);
    engine.advance(10);
    assert_eq!(engine.feed_underruns(&feed_id("f")), None, "a closed feed is gone");
}

#[test]
fn a_stereo_feed_is_mixed_down_by_the_tap_and_a_zero_rate_is_refused() {
    // Frames (1, 1) and (2, 2) down-mixed: (L + R) * (1/sqrt 2) = 2 * 0.70710678 = 1.41421356 and 2.82842712.
    let engine = MockEngine::new();
    let mut a = bus("a", feed_source("f"));
    a.channels = Channels::Stereo;
    engine.apply(&GraphSpec { buses: vec![a], ..GraphSpec::default() }).unwrap();
    let mut writer = engine.feed(&feed_request("f", false)).unwrap();
    let (mut reader, _) = engine.tap_ring(&ring_request(TapPoint::BusPostFader(bus_id("a")))).unwrap();
    writer.push(&[1.0, 1.0, 2.0, 2.0]);
    engine.advance(2);
    let mut out = [0.0f32; 2];
    let run = reader.read(&mut out);
    assert_eq!((run.len, run.start_sample), (2, 0));
    assert!(
        (out[0] - std::f32::consts::SQRT_2).abs() < 1e-6 && (out[1] - 2.0 * std::f32::consts::SQRT_2).abs() < 1e-6,
        "{out:?}"
    );

    // One scalar ring cannot carry two channels with frame-true positions, so it is refused.
    let mut both = ring_request(TapPoint::BusPostFader(bus_id("a")));
    both.format.mono = false;
    assert!(matches!(engine.tap_ring(&both), Err(AudioError::TapUnavailable(_))));

    let mut none = feed_request("g", true);
    none.rate = 0;
    assert!(matches!(engine.feed(&none), Err(AudioError::Internal(_))));
}

/// A 24 kHz feed (a TTS voice) through the graph: its playhead places an impulse on the exact output sample, a close
/// fades it out over 120 samples and silences it for good, and a feed reopened under the same id plays at once.
#[test]
fn a_feed_at_its_own_rate_reports_where_it_played_and_fades_when_closed() {
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    let mut request = feed_request("f", true);
    request.rate = 24_000;
    let mut writer = engine.feed(&request).unwrap();
    let (mut reader, _) = engine.tap_ring(&ring_request(TapPoint::BusPostFader(bus_id("a")))).unwrap();
    let mut pcm = vec![0.0f32; 4_000];
    pcm[1_234] = 1.0;
    writer.push(&pcm);
    engine.advance(10_000);
    let mut out = vec![0.0f32; 10_000];
    assert_eq!(reader.read(&mut out).len, 10_000);
    let peak = out.iter().enumerate().fold((0, 0.0f32), |m, (i, &s)| if s > m.1 { (i, s) } else { m }).0;
    let playhead = writer.playhead();
    assert_eq!(playhead.place(1_234, 0), Some(peak as u64));
    assert_eq!(engine.feed_underruns(&feed_id("f")).map(|u| u > 0), Some(true), "the feed ran dry after 4000 samples");

    writer.push(&vec![0.5; 24_000]);
    engine.advance(480);
    writer.closer().close();
    engine.advance(480);
    let mut tail = vec![0.0f32; 960];
    assert_eq!(reader.read(&mut tail).len, 960);
    let cut = playhead.cut_at_sample();
    assert_eq!(cut, 10_480 + 120, "the fade ends 120 samples into the block after the close");
    assert!(tail[600..].iter().all(|&s| s == 0.0));
    assert!(tail[480..600].iter().all(|&s| s <= 0.5 + 1e-6));
    engine.advance(16);
    assert_eq!(reader.read(&mut [0.0f32; 16]).len, 16);
    assert_eq!(engine.feed_underruns(&feed_id("f")), None, "a cut feed is gone at the next block");

    let mut again = engine.feed(&request).unwrap();
    again.push(&vec![0.25; 2_400]);
    engine.advance(480);
    let mut fresh = vec![0.0f32; 480];
    assert_eq!(reader.read(&mut fresh).len, 480);
    assert!(fresh[10..].iter().all(|&s| (s - 0.25).abs() < 1e-6), "the reopened feed plays at once");
}

#[test]
fn a_ring_capacity_that_cannot_be_made_is_refused() {
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    for seconds in [f64::INFINITY, f64::NAN, -1.0, 1.0e12] {
        let mut request = ring_request(TapPoint::BusPostFader(bus_id("a")));
        request.capacity = Seconds(seconds);
        assert!(matches!(engine.tap_ring(&request), Err(AudioError::Internal(_))), "tap {seconds}");
        let mut feed = feed_request("f", true);
        feed.capacity = Seconds(seconds);
        assert!(matches!(engine.feed(&feed), Err(AudioError::Internal(_))), "feed {seconds}");
    }
}

#[test]
fn samples_a_sink_does_not_take_are_counted_as_dropped() {
    struct Half;
    impl omuzaki_audio_api::tap::TapSink for Half {
        fn push(&mut self, samples: &[f32]) -> usize {
            samples.len() / 2
        }
    }
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    let mut request = ring_request(TapPoint::BusPostFader(bus_id("a")));
    request.consumer = "half".into();
    let (_handle, _) = engine.tap(&request, Box::new(Half)).unwrap();
    engine.advance(100);
    let drops = engine.status().tap_drops;
    assert_eq!((drops[0].consumer.as_str(), drops[0].dropped), ("half", 50));
}

#[test]
fn an_overwrite_ring_reports_the_samples_its_reader_fell_behind_by() {
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    let mut request = ring_request(TapPoint::BusPostFader(bus_id("a")));
    request.policy = DropPolicy::Overwrite;
    request.capacity = Seconds(100.0 / 48_000.0);
    let (mut reader, _) = engine.tap_ring(&request).unwrap();
    engine.advance(100);
    assert_eq!(engine.status().tap_drops[0].dropped, 0, "a ring that just fits has lost nothing");
    // The reader is 150 samples behind a ring of 100 after 250 samples.
    engine.advance(150);
    assert_eq!((engine.status().tap_drops[0].consumer.as_str(), engine.status().tap_drops[0].dropped), ("test", 150));
    let mut out = [0.0f32; 256];
    assert_eq!(reader.read(&mut out).len, 100);
    assert_eq!(reader.overwritten(), 150, "the reader's own count agrees with the engine's");
    engine.advance(60);
    assert_eq!(engine.status().tap_drops[0].dropped, 150, "a reader that caught up loses nothing more");
}

#[test]
fn a_negative_ring_length_is_refused_even_when_it_rounds_to_zero() {
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    for seconds in [-1.0e-6, -1.0e-9, -0.4 / 48_000.0] {
        let mut request = ring_request(TapPoint::BusPostFader(bus_id("a")));
        request.capacity = Seconds(seconds);
        assert!(matches!(engine.tap_ring(&request), Err(AudioError::Internal(_))), "tap {seconds}");
        let mut feed = feed_request("f", true);
        feed.capacity = Seconds(seconds);
        assert!(matches!(engine.feed(&feed), Err(AudioError::Internal(_))), "feed {seconds}");
    }
    let mut zero = ring_request(TapPoint::BusPostFader(bus_id("a")));
    zero.capacity = Seconds(0.0);
    assert!(engine.tap_ring(&zero).is_ok(), "zero stays a one-sample ring");
}

#[test]
fn apply_closes_a_two_channel_tap_whose_bus_became_stereo() {
    let engine = MockEngine::new();
    let mut graph = one_bus();
    engine.apply(&graph).unwrap();
    let mut request = ring_request(TapPoint::BusPostFader(bus_id("a")));
    request.format.mono = false;
    let (reader, _) = engine.tap_ring(&request).unwrap();
    let (mono_reader, _) = engine.tap_ring(&ring_request(TapPoint::BusPostFader(bus_id("a")))).unwrap();
    graph.buses[0].channels = Channels::Stereo;
    engine.apply(&graph).unwrap();
    assert!(reader.writer_gone(), "a scalar ring cannot carry the stereo bus: it is closed");
    assert!(!mono_reader.writer_gone(), "a mono down-mix of the same bus stays open");
    assert_eq!(engine.status().tap_drops.len(), 1);
}

#[test]
fn apply_closes_a_tap_whose_capture_offset_changed() {
    let engine = MockEngine::new();
    let mut graph = one_bus();
    graph.buses[0].chain = vec![null_node(12)];
    engine.apply(&graph).unwrap();
    let request = ring_request(TapPoint::BusPostChain(bus_id("a")));
    let (reader, info) = engine.tap_ring(&request).unwrap();
    assert!((info.capture_offset.0 - 12.0 / 48_000.0).abs() < 1e-12);
    engine.apply(&graph).unwrap();
    assert!(!reader.writer_gone(), "the same graph keeps the tap");
    graph.buses[0].chain = vec![null_node(20)];
    engine.apply(&graph).unwrap();
    assert!(reader.writer_gone(), "the offset the tap was told is stale: it is closed");
    let (_reopened, info) = engine.tap_ring(&request).unwrap();
    assert!((info.capture_offset.0 - 20.0 / 48_000.0).abs() < 1e-12, "a new tap gets the new offset");
}

#[test]
fn a_voice_taps_change_moves_the_open_taps() {
    // `mic` plays a 0.5 feed with its fader at minus infinity. `captions` defaults to raw (before the chain: 0.5);
    // once `voice_taps` says heard (after the fader: 0), the tap that is already open follows.
    let engine = MockEngine::new();
    let mut mic = bus("mic", feed_source("f"));
    mic.gain_db = f32::NEG_INFINITY;
    let mut graph = GraphSpec { buses: vec![mic], ..GraphSpec::default() };
    engine.apply(&graph).unwrap();
    let mut writer = engine.feed(&feed_request("f", true)).unwrap();
    let (mut reader, _) = engine.tap_ring(&TapRequest::voice(VoiceUse::Captions, "asr")).unwrap();
    writer.push(&[0.5; 960]);
    engine.advance(480);
    let mut out = [9.0f32; 160];
    assert_eq!(reader.read(&mut out).len, 160);
    assert!(out.iter().all(|&s| s == 0.5), "raw hears the input");

    graph.voice_taps.set(VoiceUse::Captions, omuzaki_audio_api::tap::VoiceSignal::Heard);
    engine.apply(&graph).unwrap();
    engine.advance(480);
    assert_eq!(reader.read(&mut out).len, 160);
    assert!(out.iter().all(|&s| s == 0.0), "heard is after the fader");
}

#[test]
fn a_tap_whose_reader_or_handle_is_dropped_closes() {
    struct Count(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl omuzaki_audio_api::tap::TapSink for Count {
        fn push(&mut self, samples: &[f32]) -> usize {
            self.0.fetch_add(samples.len(), std::sync::atomic::Ordering::SeqCst);
            samples.len()
        }
    }
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let request = ring_request(TapPoint::BusPostFader(bus_id("a")));
    let (handle, _) = engine.tap(&request, Box::new(Count(seen.clone()))).unwrap();
    let (reader, _) = engine.tap_ring(&request).unwrap();
    engine.advance(100);
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 100);
    assert_eq!(engine.status().tap_drops.len(), 2);
    drop(handle);
    drop(reader);
    engine.advance(100);
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 100, "a closed tap gets nothing more");
    assert_eq!(engine.status().tap_drops.len(), 0);
}

#[test]
fn a_full_drop_newest_ring_counts_what_it_refused() {
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    let mut request = ring_request(TapPoint::BusPostFader(bus_id("a")));
    request.capacity = Seconds(100.0 / 48_000.0);
    let (_reader, _) = engine.tap_ring(&request).unwrap();
    engine.advance(250);
    let status = engine.status();
    assert_eq!((status.tap_drops[0].consumer.as_str(), status.tap_drops[0].dropped), ("test", 150));
    assert!(matches!(
        engine.tap_ring(&ring_request(TapPoint::BusPostFader(bus_id("ghost")))),
        Err(AudioError::TapUnavailable(_))
    ));
}

#[test]
fn the_report_sums_declared_latency_along_each_route() {
    // `a` has nodes of 3 and 4 samples and plays on `out` and sends to `b` (a 5-sample node), which plays on `b_out`.
    let mut a = one_bus().buses.remove(0);
    a.chain = vec![null_node(3), NodeSpec { id: NodeId::new("m").unwrap(), ..null_node(4) }];
    a.sends = vec![SendSpec { to: bus_id("b"), gain_db: 0.0, pre_fader: false }];
    let mut b = bus("b", None);
    b.chain = vec![null_node(5)];
    b.outputs = vec![role("b_out")];
    let engine = MockEngine::new();
    engine.apply(&GraphSpec { buses: vec![a, b], ..GraphSpec::default() }).unwrap();
    let (_reader, info) = engine.tap_ring(&ring_request(TapPoint::BusPostChain(bus_id("b")))).unwrap();
    let report = engine.report();
    let to = |end: &RouteEnd| report.routes.iter().find(|r| &r.to == end).map(|r| r.graph_samples);
    assert_eq!(to(&RouteEnd::Output { role: role("out") }), Some(7));
    assert_eq!(to(&RouteEnd::Output { role: role("b_out") }), Some(3 + 4 + 5));
    assert_eq!(to(&RouteEnd::Tap { point: TapPoint::BusPostChain(bus_id("b")) }), Some(12));
    assert_eq!(report.buses.iter().map(|b| b.chain_samples).collect::<Vec<_>>(), [7, 5]);
    assert!((info.capture_offset.0 - 12.0 / 48_000.0).abs() < 1e-12, "the tap's offset is its route's latency");
}

#[test]
fn meters_are_the_last_blocks_levels() {
    // A constant 0.5: peak 20 * log10(0.5) = -6.0206 dBFS, RMS 10 * log10(0.25) = -6.0206 dBFS.
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    let at = MeterPoint::Bus(bus_id("a"));
    assert_eq!(engine.meters(&at).map(|m| m.peak_db), Some(f32::NEG_INFINITY));
    let mut writer = engine.feed(&feed_request("f", true)).unwrap();
    writer.push(&[0.5; 128]);
    engine.advance(128);
    let meters = engine.meters(&at).unwrap();
    assert!((meters.peak_db + 6.0206).abs() < 1e-3 && (meters.rms_db + 6.0206).abs() < 1e-3, "{meters:?}");
    engine.advance(128);
    assert_eq!(engine.meters(&at).unwrap().peak_db, f32::NEG_INFINITY, "the feed ran dry: the last block is silent");
    assert_eq!(engine.meters(&MeterPoint::Bus(bus_id("ghost"))), None);
    assert_eq!(engine.meters(&MeterPoint::Input(role("voice"))), None);
}

#[test]
fn the_mic_modes_follow_the_design() {
    let engine = MockEngine::new();
    let events = engine.subscribe();
    assert_eq!(engine.set_mic_mode(MicMode::Muted).unwrap(), MicMode::Muted);
    assert!(matches!(engine.set_mic_mode(MicMode::CohostOnly), Err(AudioError::NeedsMonitorOutput)));
    let monitor = Binding::SystemDefault { channels: ChannelMap::stereo(0, 1), speakers: false };
    engine.bind(&role("monitor_out"), monitor).unwrap();
    assert_eq!(engine.set_mic_mode(MicMode::CohostOnly).unwrap(), MicMode::CohostOnly);
    assert_eq!(engine.set_mic_mode(MicMode::Live).unwrap(), MicMode::Muted, "leaving co-host lands on Muted");
    assert_eq!(engine.set_mic_mode(MicMode::Live).unwrap(), MicMode::Live);
    assert_eq!(engine.mic_mode(), MicMode::Live);
    let modes: Vec<MicMode> = events
        .drain()
        .into_iter()
        .filter_map(|e| if let AudioEvent::MicModeChanged { mode } = e { Some(mode) } else { None })
        .collect();
    assert_eq!(modes, [MicMode::Muted, MicMode::CohostOnly, MicMode::Muted, MicMode::Live]);
}

#[test]
fn muting_silences_and_flags_the_voice_taps_but_listen_stays_open_in_cohost_only() {
    let engine = MockEngine::new();
    let mut mic = bus("mic", feed_source("f"));
    mic.outputs = vec![role("out")];
    engine.apply(&GraphSpec { buses: vec![mic], ..GraphSpec::default() }).unwrap();
    engine
        .bind(&role("monitor_out"), Binding::SystemDefault { channels: ChannelMap::mono(0), speakers: false })
        .unwrap();
    let mut writer = engine.feed(&feed_request("f", true)).unwrap();
    let (mut mouth, mouth_info) = engine.tap_ring(&TapRequest::voice(VoiceUse::Mouth, "lip sync")).unwrap();
    let (mut listen, _) = engine.tap_ring(&TapRequest::voice(VoiceUse::Listen, "performer")).unwrap();
    assert!(mouth_info.honours_mute);
    let mut read = |writer: &mut omuzaki_audio_api::tap::FeedWriter| {
        writer.push(&[0.5; 300]);
        engine.advance(300);
        let mut m = [1.0f32; 300];
        let mut l = [1.0f32; 100];
        (mouth.read(&mut m), m, listen.read(&mut l), l)
    };

    let (m, samples, l, _) = read(&mut writer);
    assert_eq!((m.muted, m.private, l.muted, l.private), (false, false, false, false));
    assert!(samples.iter().all(|&s| s == 0.5));

    engine.set_mic_mode(MicMode::Muted).unwrap();
    let (m, samples, l, listened) = read(&mut writer);
    assert!(m.muted && l.muted, "mute silences every voice tap, the listener included");
    assert!(samples.iter().all(|&s| s == 0.0) && listened.iter().all(|&s| s == 0.0));

    engine.set_mic_mode(MicMode::CohostOnly).unwrap();
    let (m, samples, l, listened) = read(&mut writer);
    assert!(m.muted && samples.iter().all(|&s| s == 0.0));
    assert!(!l.muted && l.private, "listen hears the private talk, flagged private");
    assert!(listened.iter().all(|&s| s == 0.5));
    assert_eq!(engine.status().mic_mode, MicMode::CohostOnly);
}

#[test]
fn the_status_reports_leaving_taps_and_auto_aec() {
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    assert!(!engine.status().aec_on);
    engine
        .bind(&role("program_out"), Binding::SystemDefault { channels: ChannelMap::stereo(0, 1), speakers: true })
        .unwrap();
    assert!(engine.status().aec_on, "Auto follows a bound output with speakers");
    let mut request = ring_request(TapPoint::BusPostFader(bus_id("a")));
    request.leaves_device = true;
    request.consumer = "stream".into();
    let (_reader, _) = engine.tap_ring(&request).unwrap();
    let leaving = engine.status().leaving_device;
    assert_eq!((leaving.len(), leaving[0].consumer.as_str()), (1, "stream"));
    engine.set_changer_latency(None).unwrap();
    assert_eq!(engine.changer_latency(), None);
}

#[test]
fn an_apply_closes_the_taps_whose_bus_went_away() {
    let engine = MockEngine::new();
    engine.apply(&one_bus()).unwrap();
    let (_reader, _) = engine.tap_ring(&ring_request(TapPoint::BusPostFader(bus_id("a")))).unwrap();
    assert_eq!(engine.status().tap_drops.len(), 1);
    engine.apply(&GraphSpec { buses: vec![bus("b", None)], ..GraphSpec::default() }).unwrap();
    assert_eq!(engine.status().tap_drops.len(), 0);
}
