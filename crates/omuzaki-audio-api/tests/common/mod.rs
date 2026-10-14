//! Shared fixtures: the five default buses of a new project (design doc section 3.1).

#![allow(dead_code, clippy::unwrap_used)] // each test binary uses a different part; a panic is a clear test failure

use omuzaki_audio_api::graph::{BusSpec, Channels, DuckRule, DuckTrigger, GraphSpec, NodeSpec, SendSpec, SourceRef};
use omuzaki_audio_api::node::{ChannelSupport, LatencyRange, NodeConfig, NodeKindInfo, ParamSpec, Unit};
use omuzaki_audio_api::{BusId, DeviceRole, FeedId, GateId, NodeId, NodeKindId, PlayerId};

pub fn bus_id(s: &str) -> BusId {
    BusId::new(s).unwrap()
}

pub fn node(id: &str, kind: &str) -> NodeSpec {
    NodeSpec {
        id: NodeId::new(id).unwrap(),
        kind: NodeKindId::new(kind).unwrap(),
        config: NodeConfig::default(),
        params: std::collections::BTreeMap::default(),
        enabled: true,
        sidechain: None,
    }
}

fn send(to: &str) -> SendSpec {
    SendSpec { to: bus_id(to), gain_db: 0.0, pre_fader: false }
}

fn bus(id: &str, channels: Channels, sources: Vec<SourceRef>, chain: Vec<NodeSpec>, sends: &[&str]) -> BusSpec {
    BusSpec {
        id: bus_id(id),
        channels,
        sources,
        chain,
        gain_db: 0.0,
        muted: false,
        sends: sends.iter().map(|t| send(t)).collect(),
        outputs: vec![],
    }
}

/// `mic`, `ai_voice`, soundboard, music and program, with the echo canceller on `mic` (its reference is `program`), the
/// limiter on `program`, and the two default ducks.
pub fn default_graph() -> GraphSpec {
    let mut aec = node("aec", "fx.aec");
    aec.sidechain = Some(bus_id("program"));
    aec.params.insert("suppression".into(), 0.5);
    let mut program = bus("program", Channels::Stereo, vec![], vec![node("limiter", "fx.limiter")], &[]);
    program.outputs = vec![DeviceRole::new("program_out").unwrap()];
    GraphSpec {
        buses: vec![
            bus(
                "mic",
                Channels::Mono,
                vec![SourceRef::Device { role: DeviceRole::new("voice").unwrap() }],
                vec![aec, node("gate", "fx.gate"), node("eq", "fx.eq")],
                &["program"],
            ),
            bus(
                "ai_voice",
                Channels::Mono,
                vec![SourceRef::Feed { id: FeedId::new("tts").unwrap() }],
                vec![],
                &["program"],
            ),
            bus(
                "soundboard",
                Channels::Stereo,
                vec![SourceRef::Player { id: PlayerId::new("soundboard").unwrap() }],
                vec![],
                &["program"],
            ),
            bus(
                "music",
                Channels::Stereo,
                vec![SourceRef::Player { id: PlayerId::new("music").unwrap() }],
                vec![],
                &["program"],
            ),
            program,
        ],
        voice_taps: omuzaki_audio_api::tap::VoiceTaps::default(),
        aec: omuzaki_audio_api::graph::AecMode::Auto,
        ducks: vec![
            DuckRule {
                trigger: DuckTrigger::Gate { id: GateId::new("sound").unwrap() },
                target: bus_id("mic"),
                depth_db: 12.0,
                threshold_db: -40.0,
                attack_ms: 10.0,
                hold_ms: 100.0,
                release_ms: 300.0,
            },
            DuckRule {
                trigger: DuckTrigger::Bus { id: bus_id("ai_voice") },
                target: bus_id("music"),
                depth_db: 9.0,
                threshold_db: -45.0,
                attack_ms: 20.0,
                hold_ms: 200.0,
                release_ms: 500.0,
            },
        ],
    }
}

fn spec(id: &'static str, channels: ChannelSupport, sidechain: bool) -> NodeKindInfo {
    NodeKindInfo {
        id,
        summary: "test kind",
        channels,
        params: &[ParamSpec {
            name: "suppression",
            unit: Unit::Percent,
            min: 0.0,
            max: 1.0,
            default: 0.5,
            smooth_ms: 5.0,
        }],
        latency: LatencyRange { min: 0, max: 480 },
        sidechain,
    }
}

/// The kinds of `default_graph`, as a lookup: `fx.aec` takes a sidechain and runs mono; `fx.limiter` runs on stereo.
pub fn kinds(id: &NodeKindId) -> Option<NodeKindInfo> {
    match id.as_str() {
        "fx.aec" => Some(spec("fx.aec", ChannelSupport::Mono, true)),
        "fx.gate" | "fx.eq" => Some(spec("fx.eq", ChannelSupport::Mono, false)),
        "fx.limiter" => Some(spec("fx.limiter", ChannelSupport::Any, false)),
        _ => None,
    }
}
