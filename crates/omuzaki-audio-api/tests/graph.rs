//! Graph validation and the TOML round trips of the cold types.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{bus_id, default_graph, kinds, node};
use omuzaki_audio_api::graph::{Channels, GraphError, GraphSpec, SendSpec};
use omuzaki_audio_api::{FxPreset, NodeId};

fn with(edit: impl FnOnce(&mut GraphSpec)) -> GraphSpec {
    let mut g = default_graph();
    edit(&mut g);
    g
}

fn bus_mut<'a>(g: &'a mut GraphSpec, id: &str) -> &'a mut omuzaki_audio_api::BusSpec {
    g.buses.iter_mut().find(|b| b.id.as_str() == id).unwrap()
}

fn send(to: &str) -> SendSpec {
    SendSpec { to: bus_id(to), gain_db: 0.0, pre_fader: false }
}

#[test]
fn the_default_graph_is_valid_and_orders_program_last() {
    let g = default_graph();
    assert_eq!(g.validate(&kinds), Ok(()));
    let order: Vec<String> = g.bus_order().unwrap().iter().map(ToString::to_string).collect();
    assert_eq!(order, ["mic", "ai_voice", "soundboard", "music", "program"]);
}

#[test]
fn a_sidechain_is_not_an_edge_so_it_never_makes_a_cycle() {
    // `mic` sends to `program` and its echo canceller reads `program`'s previous block: legal.
    assert_eq!(default_graph().validate_structure(), Ok(()));
}

#[test]
fn table_of_invalid_graphs() {
    type Case = (&'static str, GraphSpec, GraphError);
    let cases: Vec<Case> = vec![
        ("duplicate bus", with(|g| g.buses.push(g.buses[0].clone())), GraphError::DuplicateBus(bus_id("mic"))),
        (
            "duplicate node in a bus",
            with(|g| bus_mut(g, "mic").chain.push(node("gate", "fx.gate"))),
            GraphError::DuplicateNode { bus: bus_id("mic"), node: NodeId::new("gate").unwrap() },
        ),
        (
            "send to an unknown bus",
            with(|g| bus_mut(g, "music").sends.push(send("nowhere"))),
            GraphError::UnknownSendTarget { bus: bus_id("music"), to: bus_id("nowhere") },
        ),
        (
            "a bus that sends to itself",
            with(|g| bus_mut(g, "music").sends.push(send("music"))),
            GraphError::Cycle(vec![bus_id("music")]),
        ),
        (
            "a three-bus cycle",
            with(|g| {
                bus_mut(g, "program").sends.push(send("soundboard"));
                bus_mut(g, "soundboard").sends = vec![send("music")];
                bus_mut(g, "music").sends = vec![send("program")];
            }),
            GraphError::Cycle(vec![bus_id("program"), bus_id("soundboard"), bus_id("music")]),
        ),
        (
            "a duck on a bus that does not exist",
            with(|g| g.ducks[0].target = bus_id("ghost")),
            GraphError::UnknownDuckTarget(bus_id("ghost")),
        ),
        (
            "a duck triggered by a bus that does not exist",
            with(|g| g.ducks[1].trigger = omuzaki_audio_api::DuckTrigger::Bus { id: bus_id("ghost") }),
            GraphError::UnknownDuckTrigger(bus_id("ghost")),
        ),
        (
            "a sidechain on an unknown bus",
            with(|g| bus_mut(g, "mic").chain[0].sidechain = Some(bus_id("ghost"))),
            GraphError::UnknownSidechain {
                bus: bus_id("mic"),
                node: NodeId::new("aec").unwrap(),
                sidechain: bus_id("ghost"),
            },
        ),
        (
            "a duck rule repeated",
            with(|g| g.ducks.push(g.ducks[0].clone())),
            GraphError::DuplicateDuck {
                trigger: common::default_graph().ducks[0].trigger.clone(),
                target: bus_id("mic"),
            },
        ),
        (
            "a fader above +12 dB",
            with(|g| bus_mut(g, "music").gain_db = 12.5),
            GraphError::GainOutOfRange { bus: bus_id("music"), db: 12.5 },
        ),
    ];
    for (name, graph, want) in cases {
        let got = graph.validate_structure();
        // NaN-free cases compare directly; the cycle starts at whichever bus the walk reaches first, so compare as sets.
        match (&got, &want) {
            (Err(GraphError::Cycle(a)), GraphError::Cycle(b)) => {
                let (mut a, mut b) = (a.clone(), b.clone());
                a.sort();
                b.sort();
                assert_eq!(a, b, "{name}");
            }
            _ => assert_eq!(got, Err(want), "{name}"),
        }
    }
}

#[test]
fn a_nan_gain_is_out_of_range() {
    let g = with(|g| bus_mut(g, "music").sends[0].gain_db = f32::NAN);
    assert!(matches!(g.validate_structure(), Err(GraphError::GainOutOfRange { .. })));
}

#[test]
fn a_cycle_is_reported_in_send_order() {
    let g = with(|g| {
        bus_mut(g, "program").sends.push(send("soundboard"));
        bus_mut(g, "soundboard").sends = vec![send("music")];
        bus_mut(g, "music").sends = vec![send("program")];
    });
    let Err(GraphError::Cycle(path)) = g.bus_order() else { panic!("expected a cycle") };
    for (i, from) in path.iter().enumerate() {
        let to = &path[(i + 1) % path.len()];
        assert!(g.bus(from).unwrap().sends.iter().any(|s| &s.to == to), "{from} does not send to {to} in {path:?}");
    }
}

#[test]
fn node_kinds_are_checked_against_the_lookup() {
    let unknown = with(|g| bus_mut(g, "mic").chain.push(node("mystery", "fx.mystery")));
    assert!(matches!(unknown.validate(&kinds), Err(GraphError::UnknownKind { .. })));

    let no_sidechain = with(|g| bus_mut(g, "mic").chain[1].sidechain = Some(bus_id("program")));
    assert!(matches!(no_sidechain.validate(&kinds), Err(GraphError::SidechainNotSupported { .. })));

    // `fx.aec` is mono-only in the fixture.
    let wrong_channels = with(|g| bus_mut(g, "mic").channels = Channels::Stereo);
    assert!(matches!(wrong_channels.validate(&kinds), Err(GraphError::UnsupportedChannels { .. })));

    // A kind that takes a sidechain may run without one.
    let without = with(|g| bus_mut(g, "mic").chain[0].sidechain = None);
    assert_eq!(without.validate(&kinds), Ok(()));
}

#[test]
fn the_default_graph_round_trips_through_toml() {
    let g = default_graph();
    let text = toml::to_string(&g).unwrap();
    assert_eq!(toml::from_str::<GraphSpec>(&text).unwrap(), g, "{text}");
}

#[test]
fn minus_infinity_survives_toml() {
    let g = with(|g| {
        bus_mut(g, "music").gain_db = f32::NEG_INFINITY;
        bus_mut(g, "music").muted = true;
    });
    let text = toml::to_string(&g).unwrap();
    let back: GraphSpec = toml::from_str(&text).unwrap();
    assert_eq!(back.bus(&bus_id("music")).unwrap().gain_db, f32::NEG_INFINITY, "{text}");
}

#[test]
fn a_short_hand_written_graph_fills_in_defaults() {
    let text = r#"
        [[buses]]
        id = "mic"
        channels = "mono"
        sources = [{ type = "device", role = "voice" }]
        sends = [{ to = "program" }]

        [[buses]]
        id = "program"
        channels = "stereo"
        outputs = ["program_out"]
    "#;
    let g: GraphSpec = toml::from_str(text).unwrap();
    assert_eq!(g.validate_structure(), Ok(()));
    let mic = g.bus(&bus_id("mic")).unwrap();
    assert_eq!((mic.gain_db, mic.muted, mic.sends[0].gain_db, mic.sends[0].pre_fader), (0.0, false, 0.0, false));
}

#[test]
fn an_fx_preset_round_trips_through_toml_and_keeps_its_shape() {
    let mut radio = node("radio", "fx.radio");
    radio.params.insert("low_cut_hz".into(), 300.0);
    radio.params.insert("high_cut_hz".into(), 3400.0);
    let preset = FxPreset { name: "Telephone".into(), chain: vec![node("gate", "fx.gate"), radio] };
    let text = toml::to_string(&preset).unwrap();
    assert_eq!(toml::from_str::<FxPreset>(&text).unwrap(), preset, "{text}");

    let mut retuned = preset.clone();
    retuned.chain[1].params.insert("low_cut_hz".into(), 400.0);
    assert!(retuned.keeps_shape_of(&preset.chain), "a parameter change keeps the shape");
    let mut added = preset.clone();
    added.chain.push(node("pitch", "fx.pitch"));
    assert!(!added.keeps_shape_of(&preset.chain), "a new node is structural");
}

#[test]
fn the_cold_types_have_json_schemas() {
    for schema in [schemars::schema_for!(GraphSpec), schemars::schema_for!(FxPreset)] {
        assert!(serde_json::to_value(schema).unwrap()["$schema"].is_string());
    }
}

#[test]
fn duck_numbers_are_checked_against_their_ranges() {
    type Edit = fn(&mut omuzaki_audio_api::DuckRule, f32);
    let fields: [(&str, Edit, [f32; 2]); 5] = [
        ("depth_db", |r, v| r.depth_db = v, [-0.1, 60.1]),
        ("threshold_db", |r, v| r.threshold_db = v, [-80.1, 0.1]),
        ("attack_ms", |r, v| r.attack_ms = v, [0.9, 1000.1]),
        ("hold_ms", |r, v| r.hold_ms = v, [-1.0, 5000.1]),
        ("release_ms", |r, v| r.release_ms = v, [0.9, 5000.1]),
    ];
    for (name, set, bad) in fields {
        for value in bad.into_iter().chain([f32::NAN]) {
            let g = with(|g| set(&mut g.ducks[0], value));
            assert!(
                matches!(g.validate_structure(), Err(GraphError::DuckOutOfRange { field, .. }) if field == name),
                "{name} = {value}"
            );
        }
    }
    // The edges themselves are allowed.
    let edges = with(|g| {
        let r = &mut g.ducks[0];
        (r.depth_db, r.threshold_db, r.attack_ms, r.hold_ms, r.release_ms) = (60.0, -80.0, 1.0, 0.0, 5000.0);
    });
    assert_eq!(edges.validate_structure(), Ok(()));
}

#[test]
fn voice_taps_and_aec_default_when_a_project_omits_them() {
    use omuzaki_audio_api::graph::AecMode;
    use omuzaki_audio_api::tap::{VoiceSignal, VoiceTaps, VoiceUse};
    let g: GraphSpec = toml::from_str("").unwrap();
    assert_eq!((g.aec, g.voice_taps.clone()), (AecMode::Auto, VoiceTaps::default()));
    assert_eq!(g.voice_taps.signal(VoiceUse::Captions), VoiceSignal::Raw);
    let text = toml::to_string(&default_graph()).unwrap();
    assert_eq!(toml::from_str::<GraphSpec>(&text).unwrap().aec, AecMode::Auto);
}
