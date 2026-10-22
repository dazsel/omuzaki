//! Cost of the mixer core on a studio graph: what the audio thread pays per 256-sample block (5.3 ms of audio) for
//! the mic, music, sound effects and an AI voice summed into the program with the music ducked under the mic, every
//! bus metered. Run through the offline mixer, the same `Mix` the live executor runs, so the number includes the
//! offline staging copy and is an upper bound on the callback's own cost.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use omuzaki_audio_api::engine::{BlockPlan, OfflineInput, OfflineRender};
use omuzaki_audio_api::graph::{
    BusSpec, Channels, DuckRule, DuckTrigger, GraphSpec, MicMode, NodeSpec, PROGRAM_BUS, Params, SendSpec, SourceRef,
};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId, NodeId, NodeKindId};
use omuzaki_audio_api::node::{ConfigValue, NodeConfig};
use omuzaki_audio_engine::OfflineMix;
use omuzaki_audio_mock::signals::{sine, white_noise};

const FRAMES: usize = 48_000;
const BLOCK: usize = 256;

fn id(name: &str) -> BusId {
    BusId::new(name).unwrap()
}

fn bus(name: &str, channels: Channels, source: Option<SourceRef>, chain: usize) -> BusSpec {
    BusSpec {
        id: id(name),
        channels,
        sources: source.into_iter().collect(),
        chain: (0..chain)
            .map(|k| NodeSpec {
                id: NodeId::new(format!("n{k}")).unwrap(),
                kind: NodeKindId::new("mock.null").unwrap(),
                config: NodeConfig(BTreeMap::from([("latency".to_owned(), ConfigValue::Int(0))])),
                params: Params::default(),
                enabled: true,
                sidechain: None,
            })
            .collect(),
        gain_db: -3.0,
        muted: false,
        sends: if name == PROGRAM_BUS {
            Vec::new()
        } else {
            vec![SendSpec { to: id(PROGRAM_BUS), gain_db: 0.0, pre_fader: false }]
        },
        outputs: if name == PROGRAM_BUS { vec![DeviceRole::new("main_out").unwrap()] } else { Vec::new() },
    }
}

fn studio(c: &mut Criterion) {
    omuzaki_audio_mock::link();
    let feed = |name: &str| SourceRef::Feed { id: FeedId::new(name).unwrap() };
    let mic = SourceRef::Device { role: DeviceRole::new("mic_in").unwrap() };
    let graph = GraphSpec {
        buses: vec![
            bus("mic", Channels::Mono, Some(mic.clone()), 3),
            bus("music", Channels::Stereo, Some(feed("music")), 1),
            bus("sfx", Channels::Stereo, Some(feed("sfx")), 1),
            bus("ai_voice", Channels::Mono, Some(feed("tts")), 1),
            bus(PROGRAM_BUS, Channels::Stereo, None, 1),
        ],
        ducks: vec![DuckRule {
            trigger: DuckTrigger::Bus { id: id("mic") },
            target: id("music"),
            depth_db: 12.0,
            threshold_db: -30.0,
            attack_ms: 10.0,
            hold_ms: 100.0,
            release_ms: 200.0,
        }],
        ..GraphSpec::default()
    };
    let tone = sine(FRAMES, 220.0, 0.25);
    let input = OfflineInput {
        frames: FRAMES,
        sources: BTreeMap::from([
            (mic, vec![white_noise(FRAMES, 3, 0.3)]),
            (feed("music"), vec![tone.clone(), tone.clone()]),
            (feed("sfx"), vec![tone.clone(), tone.clone()]),
            (feed("tts"), vec![tone]),
        ]),
        taps: BTreeMap::new(),
        mic_mode: MicMode::Live,
    };
    let mut group = c.benchmark_group("mix");
    group.throughput(Throughput::Elements(FRAMES as u64));
    group.bench_function("studio_5_buses_1s_in_256_blocks", |b| {
        b.iter(|| black_box(OfflineMix.render(&graph, &input, BlockPlan::Fixed { frames: BLOCK })));
    });
    group.finish();
}

criterion_group!(benches, studio);
criterion_main!(benches);
