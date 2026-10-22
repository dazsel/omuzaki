//! The engine's mixer core against the reference engine (design section 9.1, #1154, #909): random graphs (buses of
//! either width, device and feed sources, chains of `mock.null` nodes switched on and off, faders, mutes, pre- and
//! post-fader sends, outputs, ducking rules, the mic modes) rendered through both with random block plans. They agree
//! to 1e-6, and the engine is bit-identical across block plans.
#![allow(clippy::unwrap_used, clippy::panic, clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::collections::BTreeMap;

use omuzaki_audio_api::engine::{BlockPlan, OfflineInput, OfflineOutput, OfflineRender, Planar};
use omuzaki_audio_api::graph::{
    BusSpec, Channels, DuckRule, DuckTrigger, GraphSpec, MicMode, NodeSpec, SendSpec, SourceRef,
};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId, NodeId, NodeKindId};
use omuzaki_audio_api::node::{ConfigValue, NodeConfig};
use omuzaki_audio_api::tap::{TapFormat, TapPoint, TapRate, VoiceUse};
use omuzaki_audio_engine::OfflineMix;
use omuzaki_audio_mock::OfflineEngine;
use omuzaki_audio_mock::signals::{sine, white_noise};
use omuzaki_core::rng::Rng;

/// How many random graphs the property test renders.
const GRAPHS: u64 = 300;
/// Frames per render: past the 1,024-sample trigger delay and several fader ramps.
const FRAMES: usize = 6_000;
/// The agreement the design asks for.
const TOLERANCE: f32 = 1e-6;

fn below(rng: &mut Rng, n: u64) -> usize {
    (rng.next_u64() % n) as usize
}

fn chance(rng: &mut Rng, p: f32) -> bool {
    rng.next_f32() < p
}

fn role(name: &str) -> DeviceRole {
    DeviceRole::new(name).unwrap()
}

fn sources() -> [SourceRef; 3] {
    [
        SourceRef::Device { role: role("mic_in") },
        SourceRef::Device { role: role("line_in") },
        SourceRef::Feed { id: FeedId::new("music").unwrap() },
    ]
}

/// A random valid graph: sends only go to later buses, so the order exists.
fn graph(rng: &mut Rng) -> GraphSpec {
    let count = 1 + below(rng, 5);
    let mut names: Vec<String> = (0..count).map(|i| format!("bus{i}")).collect();
    if chance(rng, 0.6) {
        names[0] = "mic".into();
    }
    if count > 1 && chance(rng, 0.5) {
        names[1] = "ai_voice".into();
    }
    let all = sources();
    let mut buses = Vec::with_capacity(count);
    for (i, name) in names.iter().enumerate() {
        let channels = if chance(rng, 0.5) { Channels::Stereo } else { Channels::Mono };
        let sources: Vec<SourceRef> = all.iter().filter(|_| chance(rng, 0.4)).cloned().collect();
        let chain = (0..below(rng, 3))
            .map(|k| NodeSpec {
                id: NodeId::new(format!("n{k}")).unwrap(),
                kind: NodeKindId::new("mock.null").unwrap(),
                config: NodeConfig(BTreeMap::from([(
                    "latency".to_owned(),
                    ConfigValue::Int(i64::try_from(below(rng, 300)).unwrap()),
                )])),
                params: BTreeMap::from([("gain_db".to_owned(), rng.range_f32(-12.0, 6.0))]),
                enabled: chance(rng, 0.7),
                sidechain: None,
            })
            .collect();
        let mut sends = Vec::new();
        for to in &names[(i + 1)..count] {
            if chance(rng, 0.45) {
                sends.push(SendSpec {
                    to: BusId::new(to).unwrap(),
                    gain_db: rng.range_f32(-18.0, 0.0),
                    pre_fader: chance(rng, 0.4),
                });
            }
        }
        let outputs = ["main_out", "monitor_out"].into_iter().filter(|_| chance(rng, 0.5)).map(role).collect();
        buses.push(BusSpec {
            id: BusId::new(name).unwrap(),
            channels,
            sources,
            chain,
            gain_db: rng.range_f32(-24.0, 6.0),
            muted: chance(rng, 0.15),
            sends,
            outputs,
        });
    }
    let mut ducks = Vec::new();
    if count > 1 {
        for _ in 0..below(rng, 3) {
            let trigger = below(rng, count as u64);
            let target = (trigger + 1 + below(rng, count as u64 - 1)) % count;
            let (trigger_id, target_id) = (BusId::new(&names[trigger]).unwrap(), BusId::new(&names[target]).unwrap());
            if ducks
                .iter()
                .any(|d: &DuckRule| d.target == target_id && d.trigger == DuckTrigger::Bus { id: trigger_id.clone() })
            {
                continue;
            }
            ducks.push(DuckRule {
                trigger: DuckTrigger::Bus { id: trigger_id },
                target: target_id,
                depth_db: rng.range_f32(3.0, 24.0),
                threshold_db: rng.range_f32(-45.0, -10.0),
                attack_ms: rng.range_f32(0.5, 50.0),
                hold_ms: rng.range_f32(0.0, 200.0),
                release_ms: rng.range_f32(5.0, 300.0),
            });
        }
    }
    GraphSpec { buses, ducks, ..GraphSpec::default() }
}

/// Speech-like bursts on the mic, a tone on the line, stereo noise as music: loud enough to trigger the ducks.
fn input(rng: &mut Rng, graph: &GraphSpec) -> OfflineInput {
    let seed = rng.next_u64();
    let mut bursts = white_noise(FRAMES, seed, 0.5);
    for (i, s) in bursts.iter_mut().enumerate() {
        if (i / 1500) % 2 == 1 {
            *s = 0.0;
        }
    }
    let [mic, line, music] = sources();
    let sources = BTreeMap::from([
        (mic, vec![bursts]),
        (line, vec![sine(FRAMES - 700, 440.0, 0.3)]),
        (music, vec![white_noise(FRAMES, seed ^ 1, 0.2), white_noise(FRAMES, seed ^ 2, 0.2)]),
    ]);
    let mut taps = BTreeMap::new();
    let first = &graph.buses[0].id;
    taps.insert(TapPoint::BusPostFader(first.clone()), TapFormat { rate: TapRate::Graph48k, mono: false });
    taps.insert(TapPoint::BusPreChain(first.clone()), TapFormat { rate: TapRate::Graph48k, mono: true });
    taps.insert(TapPoint::Input(role("line_in")), TapFormat { rate: TapRate::Graph48k, mono: true });
    if graph.buses.iter().any(|b| b.id.as_str() == "mic") {
        taps.insert(TapPoint::Voice(VoiceUse::Mouth), TapFormat { rate: TapRate::Graph48k, mono: true });
        taps.insert(TapPoint::Voice(VoiceUse::Listen), TapFormat { rate: TapRate::Graph48k, mono: true });
    }
    let mic_mode = [MicMode::Live, MicMode::Muted, MicMode::CohostOnly][below(rng, 3)];
    OfflineInput { frames: FRAMES, sources, taps, mic_mode }
}

/// The largest difference between two planar signals, which must have the same shape.
fn worst(a: &Planar, b: &Planar, what: &str) -> f32 {
    assert_eq!(a.len(), b.len(), "{what}: channel counts");
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            assert_eq!(x.len(), y.len(), "{what}: lengths");
            x.iter().zip(y).map(|(p, q)| (p - q).abs()).fold(0.0, f32::max)
        })
        .fold(0.0, f32::max)
}

fn compare(engine: &OfflineOutput, reference: &OfflineOutput, label: &str) -> f32 {
    assert_eq!(engine.buses.keys().collect::<Vec<_>>(), reference.buses.keys().collect::<Vec<_>>(), "{label}");
    assert_eq!(engine.outputs.keys().collect::<Vec<_>>(), reference.outputs.keys().collect::<Vec<_>>(), "{label}");
    assert_eq!(engine.taps.keys().collect::<Vec<_>>(), reference.taps.keys().collect::<Vec<_>>(), "{label}");
    let mut most = 0.0f32;
    for (id, bus) in &engine.buses {
        most = most.max(worst(bus, &reference.buses[id], &format!("{label} bus {id}")));
    }
    for (role, out) in &engine.outputs {
        most = most.max(worst(out, &reference.outputs[role], &format!("{label} output {role}")));
    }
    for (point, samples) in &engine.taps {
        let theirs = &reference.taps[point];
        most = most.max(worst(&vec![samples.clone()], &vec![theirs.clone()], &format!("{label} tap {point:?}")));
    }
    most
}

fn bits(output: &OfflineOutput) -> Vec<u32> {
    let planar = output.buses.values().chain(output.outputs.values());
    planar.flatten().flatten().map(|s| s.to_bits()).chain(output.taps.values().flatten().map(|s| s.to_bits())).collect()
}

#[test]
fn the_engine_agrees_with_the_reference_on_random_graphs_and_block_plans() {
    omuzaki_audio_mock::link();
    let mut rng = Rng::new(0x0A0D_10E5);
    let mut most = 0.0f32;
    let mut ducked = 0;
    for g in 0..GRAPHS {
        let graph = graph(&mut rng);
        let input = input(&mut rng, &graph);
        ducked += usize::from(!graph.ducks.is_empty());
        let plans = [
            BlockPlan::Fixed { frames: 1024 },
            BlockPlan::Random { seed: rng.next_u64(), max_frames: 1024 },
            BlockPlan::Random { seed: rng.next_u64(), max_frames: 37 },
        ];
        let reference = OfflineEngine.render(&graph, &input, plans[0]).unwrap_or_else(|e| panic!("graph {g}: {e}"));
        let mut first_bits = None;
        for plan in plans {
            let engine = OfflineMix.render(&graph, &input, plan).unwrap_or_else(|e| panic!("graph {g}: {e}"));
            let diff = compare(&engine, &reference, &format!("graph {g} {plan:?}"));
            assert!(diff <= TOLERANCE, "graph {g} {plan:?}: off by {diff}\n{graph:#?}");
            most = most.max(diff);
            let these = bits(&engine);
            match &first_bits {
                None => first_bits = Some(these),
                Some(first) => assert!(*first == these, "graph {g}: {plan:?} is not bit-identical to the first plan"),
            }
        }
    }
    println!("offline parity: {GRAPHS} graphs ({ducked} with ducking), 3 block plans each, worst difference {most:e}");
}

#[test]
fn a_tap_on_a_missing_bus_is_refused_by_both() {
    omuzaki_audio_mock::link();
    let mut rng = Rng::new(3);
    let graph = graph(&mut rng);
    let mut input = input(&mut rng, &graph);
    input.taps.insert(
        TapPoint::BusPostFader(BusId::new("nowhere").unwrap()),
        TapFormat { rate: TapRate::Graph48k, mono: true },
    );
    let plan = BlockPlan::Fixed { frames: 256 };
    assert!(OfflineMix.render(&graph, &input, plan).is_err());
    assert!(OfflineEngine.render(&graph, &input, plan).is_err());
}

#[test]
fn speech_on_the_mic_ducks_the_music_by_the_rules_depth_after_the_trigger_delay() {
    let mic = BusId::new("mic").unwrap();
    let music = BusId::new("music").unwrap();
    let bus = |id: &BusId, channels, source: SourceRef| BusSpec {
        id: id.clone(),
        channels,
        sources: vec![source],
        chain: Vec::new(),
        gain_db: 0.0,
        muted: false,
        sends: Vec::new(),
        outputs: vec![role("main_out")],
    };
    let [mic_in, _, feed] = sources();
    let mut graph = GraphSpec {
        buses: vec![bus(&mic, Channels::Mono, mic_in.clone()), bus(&music, Channels::Stereo, feed.clone())],
        ..GraphSpec::default()
    };
    let frames = 72_000;
    // Silence for 0.25 s, speech for 0.5 s, silence again.
    let speech: Vec<f32> = white_noise(frames, 9, 0.5)
        .into_iter()
        .enumerate()
        .map(|(i, s)| if (12_000..36_000).contains(&i) { s } else { 0.0 })
        .collect();
    let tone = sine(frames, 220.0, 0.25);
    let input = OfflineInput {
        frames,
        sources: BTreeMap::from([(mic_in, vec![speech]), (feed, vec![tone.clone(), tone])]),
        taps: BTreeMap::new(),
        mic_mode: MicMode::Live,
    };
    let plan = BlockPlan::Fixed { frames: 480 };
    let dry = OfflineMix.render(&graph, &input, plan).unwrap();
    graph.ducks.push(DuckRule {
        trigger: DuckTrigger::Bus { id: mic.clone() },
        target: music.clone(),
        depth_db: 12.0,
        threshold_db: -30.0,
        attack_ms: 5.0,
        hold_ms: 50.0,
        release_ms: 100.0,
    });
    let ducked = OfflineMix.render(&graph, &input, plan).unwrap();
    let ratio = |at: usize| ducked.buses[&music][0][at] / dry.buses[&music][0][at];
    // Before the speech, and before the trigger delay (1,024 samples) has passed after it starts: untouched.
    assert!((ratio(6_000) - 1.0).abs() < 1e-6, "{}", ratio(6_000));
    assert!((ratio(12_000 + 1_000) - 1.0).abs() < 1e-6, "{}", ratio(13_000));
    // Mid-speech: down by the depth, 12 dB.
    let depth = omuzaki_audio_api::semantics::db_to_gain(-12.0);
    for at in [20_000, 30_000, 35_000] {
        assert!((ratio(at) - depth).abs() < 1e-4, "at {at}: {} not {depth}", ratio(at));
    }
    // After the speech the level follower falls (50 ms), the rule holds (50 ms) and releases (100 ms): by 0.5 s later,
    // back to unity.
    assert!((ratio(60_000) - 1.0).abs() < 1e-6, "{}", ratio(60_000));
    // The mic itself is never touched by its own rule.
    assert_eq!(ducked.buses[&mic], dry.buses[&mic]);
}

/// The timing of #909's acceptance case, to the sample: depth 12 dB, attack 10 ms, hold 100 ms, release 200 ms. The
/// duck factor starts falling exactly 1,024 samples after the trigger rises and is exactly -12 dB 480 samples later;
/// after the trigger ends it stays at -12 dB for exactly 4,800 samples and is exactly 0 dB 9,600 samples after that.
/// The output is bit-identical under five block plans and with the bus order reversed.
#[test]
fn a_duck_moves_on_the_exact_samples_whatever_the_blocks_and_the_bus_order() {
    use omuzaki_audio_api::semantics::{LevelFollower, TRIGGER_DELAY, db_to_gain};

    let [mic_in, _, feed] = sources();
    let (trigger, target) = (BusId::new("voice").unwrap(), BusId::new("bed").unwrap());
    let bus = |id: &BusId, source: SourceRef| BusSpec {
        id: id.clone(),
        channels: Channels::Mono,
        sources: vec![source],
        chain: Vec::new(),
        gain_db: 0.0,
        muted: false,
        sends: Vec::new(),
        outputs: Vec::new(),
    };
    let rule = DuckRule {
        trigger: DuckTrigger::Bus { id: trigger.clone() },
        target: target.clone(),
        depth_db: 12.0,
        threshold_db: -30.0,
        attack_ms: 10.0,
        hold_ms: 100.0,
        release_ms: 200.0,
    };
    let frames = 48_000;
    let (rise, fall) = (6_000, 18_000);
    let voice: Vec<f32> = (0..frames).map(|i| if (rise..fall).contains(&i) { 0.5 } else { 0.0 }).collect();
    // The trigger is active while the follower is over the threshold: from `rise` until its 50 ms release passes -30 dB.
    let threshold = db_to_gain(-30.0);
    let mut follower = LevelFollower::new();
    let last_active = (0..frames).filter(|&i| follower.step(voice[i]) > threshold).last().unwrap();
    let input = OfflineInput {
        frames,
        sources: BTreeMap::from([(mic_in.clone(), vec![voice]), (feed.clone(), vec![vec![1.0; frames]])]),
        taps: BTreeMap::new(),
        mic_mode: MicMode::Live,
    };
    let render = |reversed: bool, plan| {
        let mut buses = vec![bus(&trigger, mic_in.clone()), bus(&target, feed.clone())];
        if reversed {
            buses.reverse();
        }
        let graph = GraphSpec { buses, ducks: vec![rule.clone()], ..GraphSpec::default() };
        OfflineMix.render(&graph, &input, plan).unwrap().buses[&target][0].clone()
    };
    let factor = render(false, BlockPlan::Fixed { frames: 480 });

    let depth = db_to_gain(-12.0);
    let start = rise + TRIGGER_DELAY;
    assert!(
        factor[..start].iter().all(|&g| g.to_bits() == 1.0f32.to_bits()),
        "untouched before the trigger delay has passed"
    );
    assert!(factor[start] < 1.0, "falling from sample {start}");
    assert!(factor[start + 478] > depth);
    let held_until = last_active + TRIGGER_DELAY + 4_800;
    assert!(
        factor[start + 479..=held_until].iter().all(|&g| g.to_bits() == depth.to_bits()),
        "exactly -12 dB from 480 samples in"
    );
    assert!(factor[held_until + 1] > depth, "rising after exactly 4,800 samples of hold");
    assert!(factor[held_until + 9_599] < 1.0);
    assert!(
        factor[held_until + 9_600..].iter().all(|&g| g.to_bits() == 1.0f32.to_bits()),
        "exactly 0 dB 9,600 samples later"
    );

    let plans = [
        BlockPlan::Fixed { frames: 1 },
        BlockPlan::Fixed { frames: 37 },
        BlockPlan::Fixed { frames: 1_024 },
        BlockPlan::Random { seed: 5, max_frames: 1_024 },
        BlockPlan::Random { seed: 11, max_frames: 64 },
    ];
    for plan in plans {
        for reversed in [false, true] {
            let other = render(reversed, plan);
            let same = other.iter().zip(&factor).all(|(a, b)| a.to_bits() == b.to_bits());
            assert!(same, "{plan:?}, reversed {reversed}: not bit-identical");
        }
    }
}
