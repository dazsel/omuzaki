//! The reference offline engine against values worked out by hand, then its determinism and its refusals.
//!
//! The arithmetic is in the comments next to each expected value. Tolerances are 1e-6, the design's bound (section 3.6).

#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure
#![allow(clippy::float_cmp)] // the landings of a ramp and of a silent path are exact by construction

use std::collections::BTreeMap;

use omuzaki_audio_api::engine::{AudioError, BlockPlan, OfflineInput, OfflineOutput, OfflineRender, Planar};
use omuzaki_audio_api::graph::{
    BusSpec, Channels, DuckRule, DuckTrigger, GraphError, GraphSpec, MicMode, NodeSpec, SendSpec, SourceRef,
};
use omuzaki_audio_api::ids::{BusId, DeviceRole, NodeId, NodeKindId};
use omuzaki_audio_api::node::{ConfigValue, NodeConfig};
use omuzaki_audio_api::tap::{TapFormat, TapPoint, TapRate, VoiceUse};
use omuzaki_audio_mock::{Change, OfflineEngine, TimedChange};

const EPS: f32 = 1e-6;

fn bus_id(s: &str) -> BusId {
    BusId::new(s).unwrap()
}

fn role(s: &str) -> DeviceRole {
    DeviceRole::new(s).unwrap()
}

fn source(name: &str) -> SourceRef {
    SourceRef::Device { role: role(name) }
}

fn bus(id: &str, channels: Channels, source_role: Option<&str>) -> BusSpec {
    BusSpec {
        id: bus_id(id),
        channels,
        sources: source_role.map(source).into_iter().collect(),
        chain: vec![],
        gain_db: 0.0,
        muted: false,
        sends: vec![],
        outputs: vec![],
    }
}

fn send(to: &str, gain_db: f32, pre_fader: bool) -> SendSpec {
    SendSpec { to: bus_id(to), gain_db, pre_fader }
}

fn graph(buses: Vec<BusSpec>) -> GraphSpec {
    GraphSpec { buses, ..GraphSpec::default() }
}

fn input(frames: usize, sources: &[(&str, Planar)]) -> OfflineInput {
    OfflineInput {
        frames,
        sources: sources.iter().map(|(name, audio)| (source(name), audio.clone())).collect(),
        ..OfflineInput::default()
    }
}

fn constant(value: f32, frames: usize) -> Planar {
    vec![vec![value; frames]]
}

fn render(graph: &GraphSpec, input: &OfflineInput) -> OfflineOutput {
    OfflineEngine.render(graph, input, BlockPlan::Fixed { frames: 256 }).unwrap()
}

fn close(actual: f32, expected: f32, what: &str) {
    assert!((actual - expected).abs() < EPS, "{what}: {actual} against {expected}");
}

fn null_node(id: &str, latency: i64, enabled: bool) -> NodeSpec {
    let mut config = NodeConfig::default();
    config.0.insert("latency".into(), ConfigValue::Int(latency));
    NodeSpec {
        id: NodeId::new(id).unwrap(),
        kind: NodeKindId::new("mock.null").unwrap(),
        config,
        params: BTreeMap::new(),
        enabled,
        sidechain: None,
    }
}

#[test]
fn a_gain_change_ramps_over_240_samples() {
    // Gain 0 dB (factor 1) goes to minus infinity (factor 0) from sample 100. Sample k (1 to 240) after the change is
    // 1 + (0 - 1) * (k / 240) = 1 - k/240, so output[100] is k = 1: 239/240; output[100 + 119] is k = 120: 1/2;
    // output[100 + 239] is k = 240: exactly 0.
    let mut a = bus("a", Channels::Mono, Some("in"));
    a.outputs = vec![role("out")];
    let change = TimedChange { at: 100, change: Change::BusGainDb { bus: bus_id("a"), db: f32::NEG_INFINITY } };
    let out = OfflineEngine
        .render_with_changes(
            &graph(vec![a]),
            &input(400, &[("in", constant(1.0, 400))]),
            BlockPlan::Fixed { frames: 64 },
            &[change],
        )
        .unwrap();
    let y = &out.outputs[&role("out")][0];
    assert_eq!(y[99], 1.0);
    close(y[100], 239.0 / 240.0, "first ramp sample");
    close(y[219], 0.5, "halfway");
    assert_eq!(y[339], 0.0);
    assert!(y[340..].iter().all(|&s| s == 0.0));
}

#[test]
fn a_mute_is_its_own_ramp_and_unmuting_returns_to_the_current_gain() {
    // Gain -20 dB is a factor 0.1. Mute at sample 0 ramps the mute factor 1 -> 0: output[n] = 0.1 * (1 - (n + 1)/240),
    // so output[119] = 0.1 * 0.5 = 0.05 and output[239] = 0. Unmute at 480 ramps 0 -> 1: output[480 + j] =
    // 0.1 * (j + 1)/240, so output[599] = 0.05 and output[719] = 0.1 (the gain, not unity).
    let mut a = bus("a", Channels::Mono, Some("in"));
    a.gain_db = -20.0;
    a.outputs = vec![role("out")];
    let changes = [
        TimedChange { at: 0, change: Change::BusMuted { bus: bus_id("a"), muted: true } },
        TimedChange { at: 480, change: Change::BusMuted { bus: bus_id("a"), muted: false } },
    ];
    let out = OfflineEngine
        .render_with_changes(
            &graph(vec![a]),
            &input(800, &[("in", constant(1.0, 800))]),
            BlockPlan::Fixed { frames: 100 },
            &changes,
        )
        .unwrap();
    let y = &out.outputs[&role("out")][0];
    close(y[0], 0.1 * 239.0 / 240.0, "first muting sample");
    close(y[119], 0.05, "muting, halfway");
    assert_eq!(y[239], 0.0);
    assert_eq!(y[479], 0.0);
    close(y[599], 0.05, "unmuting, halfway");
    close(y[719], 0.1, "unmuted at the bus gain");
    close(y[799], 0.1, "stays there");
}

#[test]
fn a_mono_bus_sends_to_a_stereo_bus_centred_at_minus_three_db() {
    // A mono 1.0 through a 0 dB send into a stereo bus puts 1.0 * (1/sqrt 2) = 0.70710678 on each side.
    let mut m = bus("m", Channels::Mono, Some("in"));
    m.sends = vec![send("s", 0.0, false)];
    let mut s = bus("s", Channels::Stereo, None);
    s.outputs = vec![role("out")];
    let out = render(&graph(vec![m, s]), &input(64, &[("in", constant(1.0, 64))]));
    let y = &out.outputs[&role("out")];
    assert_eq!(y.len(), 2);
    for side in y {
        assert!(side.iter().all(|&x| (x - std::f32::consts::FRAC_1_SQRT_2).abs() < EPS));
    }
}

#[test]
fn a_stereo_bus_sends_to_a_mono_bus_as_the_scaled_sum() {
    // Left 1.0 and right 0.5: (1.0 + 0.5) * (1/sqrt 2) = 1.06066017.
    let mut s = bus("s", Channels::Stereo, Some("in"));
    s.sends = vec![send("m", 0.0, false)];
    let mut m = bus("m", Channels::Mono, None);
    m.outputs = vec![role("out")];
    let stereo = vec![vec![1.0; 32], vec![0.5; 32]];
    let out = render(&graph(vec![s, m]), &input(32, &[("in", stereo)]));
    assert!(out.outputs[&role("out")][0].iter().all(|&x| (x - 1.060_660_2).abs() < EPS));
}

#[test]
fn a_pre_fader_send_ignores_the_gain_but_not_the_mute() {
    // Bus `a` has its gain at minus infinity: its own output is silent, but a pre-fader send carries
    // pre * mute * send = 1 * 1 * 0.5 (send -6.0206 dB) and a post-fader send carries post * send = 0.
    // Muting `a` as well cuts the pre-fader send: pre * 0 * send = 0.
    let build = |muted: bool| {
        let mut a = bus("a", Channels::Mono, Some("in"));
        a.gain_db = f32::NEG_INFINITY;
        a.muted = muted;
        a.sends = vec![send("pre", -6.020_6, true), send("post", -6.020_6, false)];
        let mut pre = bus("pre", Channels::Mono, None);
        pre.outputs = vec![role("pre_out")];
        let mut post = bus("post", Channels::Mono, None);
        post.outputs = vec![role("post_out")];
        graph(vec![a, pre, post])
    };
    let audio = input(32, &[("in", constant(1.0, 32))]);
    let open = render(&build(false), &audio);
    assert!(open.outputs[&role("pre_out")][0].iter().all(|&x| (x - 0.5).abs() < 1e-4));
    assert!(open.outputs[&role("post_out")][0].iter().all(|&x| x == 0.0));
    let muted = render(&build(true), &audio);
    assert!(muted.outputs[&role("pre_out")][0].iter().all(|&x| x == 0.0));
}

#[test]
fn ducking_reads_its_trigger_1024_samples_late() {
    // `voice` holds 0.5, so its level follower sits at 0.5, over the -20 dB threshold (0.1), from sample 0. The rule
    // reads the trigger 1024 samples late, so `music` is untouched up to sample 1023 and the rule first sees `true` at
    // sample 1024. Attack 1 ms is 48 steps down to -20 dB, step k at -20 * k/48 dB:
    //   music[1024] = 10^(-20/48/20) = 10^(-1/48) = 0.95316188
    //   music[1025] = 10^(-2/48)     = 0.90851758
    //   music[1071] = 10^(-20/20)    = 0.1 (step 48 snaps to the target), and it stays there.
    let voice = bus("voice", Channels::Mono, Some("v"));
    let mut music = bus("music", Channels::Mono, Some("m"));
    music.outputs = vec![role("out")];
    let mut g = graph(vec![voice, music]);
    g.ducks = vec![DuckRule {
        trigger: DuckTrigger::Bus { id: bus_id("voice") },
        target: bus_id("music"),
        depth_db: 20.0,
        threshold_db: -20.0,
        attack_ms: 1.0,
        hold_ms: 0.0,
        release_ms: 100.0,
    }];
    let audio = input(2000, &[("v", constant(0.5, 2000)), ("m", constant(1.0, 2000))]);
    let out = render(&g, &audio);
    let y = &out.outputs[&role("out")][0];
    assert!(y[..1024].iter().all(|&s| s == 1.0), "nothing ducks before the trigger is seen");
    close(y[1024], 0.953_161_9, "duck step 1");
    close(y[1025], 0.908_517_6, "duck step 2");
    close(y[1071], 0.1, "duck step 48");
    assert!(y[1071..].iter().all(|&s| (s - 0.1).abs() < EPS));
    // The ducked bus's own output is what a bus trigger listens to, and `voice` was never ducked.
    assert!(out.buses[&bus_id("voice")][0].iter().all(|&s| s == 0.5));
}

#[test]
fn a_ducking_gate_never_opens_offline() {
    let voice = bus("voice", Channels::Mono, None);
    let mut music = bus("music", Channels::Mono, Some("m"));
    music.outputs = vec![role("out")];
    let mut g = graph(vec![voice, music]);
    g.ducks = vec![DuckRule {
        trigger: DuckTrigger::Gate { id: omuzaki_audio_api::ids::GateId::new("sound").unwrap() },
        target: bus_id("music"),
        depth_db: 20.0,
        threshold_db: -20.0,
        attack_ms: 1.0,
        hold_ms: 0.0,
        release_ms: 100.0,
    }];
    let out = render(&g, &input(2000, &[("m", constant(1.0, 2000))]));
    assert!(out.outputs[&role("out")][0].iter().all(|&s| s == 1.0));
}

#[test]
fn a_node_delays_by_its_latency_and_a_disabled_one_keeps_it() {
    // An impulse at sample 0 through `mock.null` with latency 3 comes out at sample 3; disabling the node must not
    // move it (design section 3.4, rule 3). Two nodes add: 3 + 2 = 5.
    let impulse: Planar = vec![(0..16).map(|n| if n == 0 { 1.0 } else { 0.0 }).collect()];
    let at = |chain: Vec<NodeSpec>| {
        let mut a = bus("a", Channels::Mono, Some("in"));
        a.chain = chain;
        a.outputs = vec![role("out")];
        let out = render(&graph(vec![a]), &input(16, &[("in", impulse.clone())]));
        out.outputs[&role("out")][0].iter().position(|&s| s == 1.0)
    };
    assert_eq!(at(vec![null_node("n", 3, true)]), Some(3));
    assert_eq!(at(vec![null_node("n", 3, false)]), Some(3));
    assert_eq!(at(vec![null_node("n", 3, true), null_node("m", 2, false)]), Some(5));
}

#[test]
fn taps_read_the_points_of_the_bus() {
    // `a` has a 2-sample node, gain -inf at the fader. Pre-chain sees the input, post-chain the delayed input, and
    // post-fader silence. The 16 kHz tap of the ramp 0..9 is the mean of each aligned group of three:
    // (0+1+2)/3 = 1, (3+4+5)/3 = 4, (6+7+8)/3 = 7 (the ninth sample, 9, starts a group that never completes).
    let mut a = bus("a", Channels::Mono, Some("in"));
    a.chain = vec![null_node("n", 2, true)];
    a.gain_db = f32::NEG_INFINITY;
    let ramp: Planar = vec![(0u8..10).map(f32::from).collect()];
    let mut i = input(10, &[("in", ramp)]);
    let mono48 = TapFormat { rate: TapRate::Graph48k, mono: true };
    let mono16 = TapFormat { rate: TapRate::Speech16k, mono: true };
    i.taps = BTreeMap::from([
        (TapPoint::BusPreChain(bus_id("a")), mono48),
        (TapPoint::BusPostChain(bus_id("a")), mono48),
        (TapPoint::BusPostFader(bus_id("a")), mono48),
        (TapPoint::Input(role("in")), mono16),
    ]);
    let out = render(&graph(vec![a]), &i);
    let pre: Vec<f32> = (0u8..10).map(f32::from).collect();
    assert_eq!(out.taps[&TapPoint::BusPreChain(bus_id("a"))], pre);
    assert_eq!(out.taps[&TapPoint::BusPostChain(bus_id("a"))], [0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
    assert!(out.taps[&TapPoint::BusPostFader(bus_id("a"))].iter().all(|&s| s == 0.0));
    assert_eq!(out.taps[&TapPoint::Input(role("in"))], [1.0, 4.0, 7.0]);
}

#[test]
fn a_tap_that_is_not_mixed_down_is_interleaved() {
    let mut s = bus("s", Channels::Stereo, Some("in"));
    s.outputs = vec![role("out")];
    let mut i = input(2, &[("in", vec![vec![1.0, 2.0], vec![-1.0, -2.0]])]);
    let both = TapFormat { rate: TapRate::Graph48k, mono: false };
    let mixed = TapFormat { rate: TapRate::Graph48k, mono: true };
    i.taps = BTreeMap::from([(TapPoint::BusPostFader(bus_id("s")), both), (TapPoint::Input(role("in")), mixed)]);
    let out = render(&graph(vec![s]), &i);
    assert_eq!(out.taps[&TapPoint::BusPostFader(bus_id("s"))], [1.0, -1.0, 2.0, -2.0]);
    // (1 + -1) * (1/sqrt 2) = 0 for both frames.
    assert_eq!(out.taps[&TapPoint::Input(role("in"))], [0.0, 0.0]);
}

#[test]
fn the_mic_mode_silences_the_voice_taps_and_cuts_the_mic_and_the_co_host() {
    // `mic` -> program; `ai_voice` -> program and monitor. Muted: the mic reaches nothing and every voice tap is
    // silent. CohostOnly: the same, except `listen` (raw, before the chain: it hears 0.5), and `ai_voice` reaches
    // `monitor_out` only.
    let mut mic = bus("mic", Channels::Mono, Some("voice"));
    mic.sends = vec![send("program", 0.0, false)];
    let mut ai = bus("ai_voice", Channels::Mono, Some("tts"));
    ai.sends = vec![send("program", 0.0, false), send("monitor", 0.0, false)];
    let mut program = bus("program", Channels::Mono, None);
    program.outputs = vec![role("program_out")];
    let mut monitor = bus("monitor", Channels::Mono, None);
    monitor.outputs = vec![role("monitor_out")];
    let g = graph(vec![mic, ai, program, monitor]);
    let run = |mode: MicMode| {
        let mut i = input(480, &[("voice", constant(0.5, 480)), ("tts", constant(0.25, 480))]);
        i.mic_mode = mode;
        i.taps = BTreeMap::from([
            (TapPoint::Voice(VoiceUse::Listen), VoiceUse::Listen.format()),
            (TapPoint::Voice(VoiceUse::Captions), VoiceUse::Captions.format()),
            (TapPoint::Voice(VoiceUse::Mouth), VoiceUse::Mouth.format()),
        ]);
        render(&g, &i)
    };
    let live = run(MicMode::Live);
    assert!(live.outputs[&role("program_out")][0].iter().all(|&s| (s - 0.75).abs() < EPS), "0.5 + 0.25");
    assert_eq!(live.taps[&TapPoint::Voice(VoiceUse::Listen)], vec![0.5; 160]);

    let muted = run(MicMode::Muted);
    assert!(muted.outputs[&role("program_out")][0].iter().all(|&s| (s - 0.25).abs() < EPS), "only the co-host");
    for use_ in [VoiceUse::Listen, VoiceUse::Captions, VoiceUse::Mouth] {
        assert!(muted.taps[&TapPoint::Voice(use_)].iter().all(|&s| s == 0.0), "{use_:?} is silent when muted");
    }

    let cohost = run(MicMode::CohostOnly);
    assert!(cohost.outputs[&role("program_out")][0].iter().all(|&s| s == 0.0), "nothing reaches the program");
    assert!(
        cohost.outputs[&role("monitor_out")][0].iter().all(|&s| (s - 0.25).abs() < EPS),
        "the co-host is monitored"
    );
    assert_eq!(cohost.taps[&TapPoint::Voice(VoiceUse::Listen)], vec![0.5; 160]);
    assert!(cohost.taps[&TapPoint::Voice(VoiceUse::Captions)].iter().all(|&s| s == 0.0));
    assert!(cohost.taps[&TapPoint::Voice(VoiceUse::Mouth)].iter().all(|&s| s == 0.0));
}

/// A graph that uses everything the arithmetic has: a chain, a mono-to-stereo send, a pre-fader send, a duck rule.
fn busy_graph() -> GraphSpec {
    let mut voice = bus("mic", Channels::Mono, Some("v"));
    voice.chain = vec![null_node("n", 7, true)];
    voice.gain_db = -3.0;
    voice.sends = vec![send("program", -1.5, false), send("music", -80.0, true)];
    let mut music = bus("music", Channels::Stereo, Some("m"));
    music.sends = vec![send("program", 0.0, false)];
    let mut program = bus("program", Channels::Stereo, None);
    program.outputs = vec![role("out")];
    let mut g = graph(vec![voice, music, program]);
    g.ducks = vec![DuckRule {
        trigger: DuckTrigger::Bus { id: bus_id("mic") },
        target: bus_id("music"),
        depth_db: 12.0,
        threshold_db: -30.0,
        attack_ms: 5.0,
        hold_ms: 20.0,
        release_ms: 50.0,
    }];
    g
}

fn noise(seed: u64, frames: usize, channels: usize, loud: usize) -> Planar {
    let mut rng = omuzaki_core::rng::Rng::new(seed);
    (0..channels)
        .map(|_| {
            (0..frames)
                .map(|n| if (n / loud).is_multiple_of(2) { rng.range_f32(-0.8, 0.8) } else { 0.001 * rng.next_f32() })
                .collect()
        })
        .collect()
}

fn bits(out: &OfflineOutput) -> Vec<u32> {
    let planar = out.buses.values().chain(out.outputs.values());
    planar.flat_map(|p| p.iter().flatten()).chain(out.taps.values().flatten()).map(|s| s.to_bits()).collect()
}

#[test]
fn the_result_is_bit_identical_across_runs_and_block_plans() {
    let g = busy_graph();
    let mut i = input(6000, &[("v", noise(1, 6000, 1, 1500)), ("m", noise(2, 6000, 2, 3000))]);
    i.taps = BTreeMap::from([
        (TapPoint::BusPostFader(bus_id("music")), TapFormat { rate: TapRate::Graph48k, mono: false }),
        (TapPoint::Voice(VoiceUse::Captions), VoiceUse::Captions.format()),
    ]);
    let plans = [
        BlockPlan::Fixed { frames: 256 },
        BlockPlan::Fixed { frames: 1 },
        BlockPlan::Fixed { frames: 7 },
        BlockPlan::Fixed { frames: 64 },
        BlockPlan::Fixed { frames: 1024 },
        BlockPlan::Random { seed: 3, max_frames: 1024 },
        BlockPlan::Random { seed: 4, max_frames: 33 },
    ];
    let reference = OfflineEngine.render(&g, &i, plans[0]).unwrap();
    assert_eq!(bits(&reference), bits(&OfflineEngine.render(&g, &i, plans[0]).unwrap()), "two runs differ");
    let music = &reference.buses[&bus_id("music")];
    assert!(music[0].iter().any(|&s| s != 0.0), "the test must not be vacuous");
    assert!(
        music[0].iter().zip(&i.sources[&source("m")][0]).any(|(out, inp)| (out.abs() - inp.abs()).abs() > 1e-3),
        "the duck must have moved the music"
    );
    for plan in &plans[1..] {
        assert_eq!(bits(&reference), bits(&OfflineEngine.render(&g, &i, *plan).unwrap()), "{plan:?} differs");
    }
}

#[test]
fn a_cycle_is_refused() {
    let mut a = bus("a", Channels::Mono, Some("in"));
    a.sends = vec![send("b", 0.0, false)];
    let mut b = bus("b", Channels::Mono, None);
    b.sends = vec![send("a", 0.0, false)];
    let error = OfflineEngine.render(&graph(vec![a, b]), &input(8, &[]), BlockPlan::Fixed { frames: 4 }).unwrap_err();
    assert!(matches!(error, AudioError::Graph(GraphError::Cycle(_))), "{error:?}");
}

#[test]
fn an_unknown_kind_a_bad_parameter_and_a_missing_tap_bus_are_refused() {
    let plan = BlockPlan::Fixed { frames: 4 };
    let mut a = bus("a", Channels::Mono, Some("in"));
    let mut unknown = null_node("n", 0, true);
    unknown.kind = NodeKindId::new("nothing.here").unwrap();
    a.chain = vec![unknown];
    let error = OfflineEngine.render(&graph(vec![a]), &input(8, &[]), plan).unwrap_err();
    assert!(matches!(error, AudioError::Graph(GraphError::UnknownKind { .. })), "{error:?}");

    let mut a = bus("a", Channels::Mono, Some("in"));
    let mut node = null_node("n", 0, true);
    node.params.insert("nope".into(), 1.0);
    a.chain = vec![node];
    let error = OfflineEngine.render(&graph(vec![a]), &input(8, &[]), plan).unwrap_err();
    assert!(matches!(error, AudioError::Node(_)), "{error:?}");

    let mut i = input(8, &[]);
    i.taps =
        BTreeMap::from([(TapPoint::BusPostFader(bus_id("ghost")), TapFormat { rate: TapRate::Graph48k, mono: true })]);
    let error = OfflineEngine.render(&graph(vec![bus("a", Channels::Mono, None)]), &i, plan).unwrap_err();
    assert!(matches!(error, AudioError::TapUnavailable(_)), "{error:?}");

    let change = TimedChange { at: 0, change: Change::BusMuted { bus: bus_id("ghost"), muted: true } };
    let error = OfflineEngine
        .render_with_changes(&graph(vec![bus("a", Channels::Mono, None)]), &input(8, &[]), plan, &[change])
        .unwrap_err();
    assert!(matches!(error, AudioError::UnknownBus(_)), "{error:?}");
}

#[test]
fn a_node_parameter_in_the_spec_is_applied() {
    // `gain_db` of mock.null at -20 dB is a factor 0.1.
    let mut a = bus("a", Channels::Mono, Some("in"));
    let mut node = null_node("n", 0, true);
    node.params.insert("gain_db".into(), -20.0);
    a.chain = vec![node];
    a.outputs = vec![role("out")];
    let out = render(&graph(vec![a]), &input(8, &[("in", constant(1.0, 8))]));
    assert!(out.outputs[&role("out")][0].iter().all(|&s| (s - 0.1).abs() < EPS));
}

#[test]
fn the_co_host_reaches_only_the_monitor_output_directly_too() {
    // `ai_voice` plays on `program_out` and on `monitor_out` directly. In CohostOnly only the monitor hears it.
    let mut ai = bus("ai_voice", Channels::Mono, Some("tts"));
    ai.outputs = vec![role("program_out"), role("monitor_out")];
    let g = graph(vec![ai]);
    let run = |mode: MicMode| {
        let mut i = input(480, &[("tts", constant(0.25, 480))]);
        i.mic_mode = mode;
        render(&g, &i)
    };
    let live = run(MicMode::Live);
    assert!(live.outputs[&role("program_out")][0].iter().all(|&s| (s - 0.25).abs() < EPS));
    let cohost = run(MicMode::CohostOnly);
    assert!(cohost.outputs[&role("program_out")][0].iter().all(|&s| s == 0.0));
    assert!(cohost.outputs[&role("monitor_out")][0].iter().all(|&s| (s - 0.25).abs() < EPS));
}

#[test]
fn a_timed_gain_is_checked_like_the_live_one_and_a_zero_frame_render_answers_every_role() {
    let mut a = bus("a", Channels::Stereo, Some("in"));
    a.outputs = vec![role("out")];
    let g = graph(vec![a]);
    for db in [f32::NAN, 12.5] {
        let change = TimedChange { at: 0, change: Change::BusGainDb { bus: bus_id("a"), db } };
        let error = OfflineEngine
            .render_with_changes(&g, &input(8, &[]), BlockPlan::Fixed { frames: 4 }, &[change])
            .unwrap_err();
        assert!(matches!(error, AudioError::Graph(GraphError::GainOutOfRange { .. })), "{db}: {error:?}");
    }
    let empty = render(&g, &input(0, &[]));
    assert_eq!(empty.outputs[&role("out")], vec![Vec::<f32>::new(), Vec::new()]);
    assert_eq!(empty.buses[&bus_id("a")].len(), 2);
}
