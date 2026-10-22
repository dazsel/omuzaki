//! `audio.bus.*` under the egress rule (design section 7.2, #1157): for each op, as an agent and as a person, the
//! cases that grow, shrink or keep where captured audio goes; the gain ceiling; undo of an agent's mute; and a
//! property test that no sequence of agent ops ever sends captured audio anywhere new.

#![allow(clippy::unwrap_used, clippy::panic, clippy::cast_possible_truncation)]

use std::collections::BTreeSet;

use omuzaki_audio_api::graph::{
    Bindings, BusSpec, CapturedSource, Channels, DuckTrigger, Egress, GraphSpec, MicMode, SendSpec, SourceRef,
    egress_pairs,
};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId};
use omuzaki_audio_engine::bus_ops::{
    BusCreate, BusDelete, BusDuck, BusList, BusMute, BusRoute, BusSetGain, graph_path, stored_graph,
};
use omuzaki_core::rng::Rng;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError, ServicesBuilder};
use serde_json::{Value, json};

fn id(s: &str) -> BusId {
    BusId::new(s).unwrap()
}

fn role(s: &str) -> DeviceRole {
    DeviceRole::new(s).unwrap()
}

fn device(s: &str) -> SourceRef {
    SourceRef::Device { role: role(s) }
}

fn send(to: &str) -> SendSpec {
    SendSpec { to: id(to), gain_db: 0.0, pre_fader: false }
}

fn bus(name: &str, channels: Channels, sources: Vec<SourceRef>, sends: &[&str], outputs: &[&str]) -> BusSpec {
    BusSpec {
        id: id(name),
        channels,
        sources,
        chain: Vec::new(),
        gain_db: 0.0,
        muted: false,
        sends: sends.iter().map(|s| send(s)).collect(),
        outputs: outputs.iter().map(|r| role(r)).collect(),
    }
}

/// The studio's buses: the mic and the music reach the stream through `program`; the game is captured but goes
/// nowhere yet; the bot's voice is a feed, not captured audio.
fn studio() -> GraphSpec {
    GraphSpec {
        buses: vec![
            bus("mic", Channels::Mono, vec![device("mic")], &["program"], &[]),
            bus("music", Channels::Stereo, vec![device("music_in")], &["program"], &[]),
            bus("game", Channels::Stereo, vec![device("game_in")], &[], &[]),
            bus("bot", Channels::Mono, vec![SourceRef::Feed { id: FeedId::new("tts").unwrap() }], &["program"], &[]),
            bus("program", Channels::Stereo, Vec::new(), &[], &["program_out", "monitor_out"]),
        ],
        ..GraphSpec::default()
    }
}

fn doc_with(graph: &GraphSpec) -> MemDoc {
    MemDoc::from_value(json!({ "audio": { "graph": graph } }))
}

fn person() -> Actor {
    Actor::Person { name: "tester".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "agent".into(), model: None }
}

/// Plans `op` as `actor` and applies its patch; returns the plan's output and the patch.
fn run(doc: &mut MemDoc, actor: &Actor, op: &impl Op) -> Result<(Value, Patch), OpError> {
    omuzaki_audio_engine::link();
    let services = ServicesBuilder::new().build();
    let clock = FakeClock::default();
    let plan = {
        let ctx = OpCtx::new(doc, actor, &clock).with_services(&services);
        op.plan(&ctx)?
    };
    doc.apply(&plan.patch).map_err(|e| OpError::internal(e.to_string()))?;
    Ok((plan.output.unwrap_or(Value::Null), plan.patch))
}

fn graph(doc: &MemDoc) -> GraphSpec {
    stored_graph(doc).unwrap().unwrap()
}

/// Every role these tests use, bound, with the mic live: the widest egress.
fn everything() -> Bindings {
    let mut bindings = Bindings { mic_mode: MicMode::Live, ..Bindings::default() };
    bindings.inputs = ["mic", "music_in", "game_in", "line_in"].into_iter().map(role).collect();
    bindings.outputs = ["program_out", "monitor_out", "phones"].into_iter().map(role).collect();
    bindings
}

fn pairs(graph: &GraphSpec) -> BTreeSet<(CapturedSource, Egress)> {
    egress_pairs(graph, &everything())
}

fn needs_person(result: &Result<(Value, Patch), OpError>) -> bool {
    matches!(result, Err(e) if e.code == ErrorCode::Denied)
}

/// One case: an op on the studio graph, and what it does to egress.
enum Effect {
    Grows,
    Shrinks,
    Keeps,
}

#[test]
#[allow(clippy::too_many_lines)] // one table
fn each_op_as_agent_and_person_grows_shrinks_or_keeps_egress() {
    type Case = (&'static str, Box<dyn Fn(&mut MemDoc, &Actor) -> Result<(Value, Patch), OpError>>, Effect);
    let cases: Vec<Case> = vec![
        (
            "create a bus that sends the game to program",
            Box::new(|doc: &mut MemDoc, actor: &Actor| {
                let op = BusCreate {
                    bus: id("game2"),
                    channels: Channels::Stereo,
                    sources: vec![device("game_in")],
                    sends: vec![send("program")],
                    outputs: Vec::new(),
                    gain_db: 0.0,
                };
                run(doc, actor, &op)
            }),
            Effect::Grows,
        ),
        (
            "create a bus fed by nothing",
            Box::new(|doc: &mut MemDoc, actor: &Actor| {
                let op = BusCreate {
                    bus: id("sfx"),
                    channels: Channels::Mono,
                    sources: Vec::new(),
                    sends: vec![send("program")],
                    outputs: Vec::new(),
                    gain_db: -3.0,
                };
                run(doc, actor, &op)
            }),
            Effect::Keeps,
        ),
        ("delete the music", Box::new(|doc, actor| run(doc, actor, &BusDelete { bus: id("music") })), Effect::Shrinks),
        ("delete the game", Box::new(|doc, actor| run(doc, actor, &BusDelete { bus: id("game") })), Effect::Keeps),
        (
            "lower the music",
            Box::new(|doc, actor| run(doc, actor, &BusSetGain { bus: id("music"), gain_db: -12.0, max_gain_db: None })),
            Effect::Keeps,
        ),
        (
            "mute the mic",
            Box::new(|doc, actor| run(doc, actor, &BusMute { bus: id("mic"), muted: true })),
            Effect::Shrinks,
        ),
        (
            "route the game to program",
            Box::new(|doc, actor| {
                run(
                    doc,
                    actor,
                    &BusRoute { bus: id("game"), sources: None, sends: Some(vec![send("program")]), outputs: None },
                )
            }),
            Effect::Grows,
        ),
        (
            "give the game its own output",
            Box::new(|doc, actor| {
                let op = BusRoute { bus: id("game"), sources: None, sends: None, outputs: Some(vec![role("phones")]) };
                run(doc, actor, &op)
            }),
            Effect::Grows,
        ),
        (
            "feed the bot bus from the line input",
            Box::new(|doc, actor| {
                let op =
                    BusRoute { bus: id("bot"), sources: Some(vec![device("line_in")]), sends: None, outputs: None };
                run(doc, actor, &op)
            }),
            Effect::Grows,
        ),
        (
            "take program off the monitor",
            Box::new(|doc, actor| {
                let op = BusRoute {
                    bus: id("program"),
                    sources: None,
                    sends: None,
                    outputs: Some(vec![role("program_out")]),
                };
                run(doc, actor, &op)
            }),
            Effect::Shrinks,
        ),
        (
            "duck the music under the mic",
            Box::new(|doc, actor| {
                let op = BusDuck {
                    target: id("music"),
                    trigger: DuckTrigger::Bus { id: id("mic") },
                    depth_db: Some(12.0),
                    threshold_db: None,
                    attack_ms: None,
                    hold_ms: None,
                    release_ms: None,
                    remove: false,
                };
                run(doc, actor, &op)
            }),
            Effect::Keeps,
        ),
    ];
    for (name, case, effect) in &cases {
        let before = pairs(&studio());
        let mut by_person = doc_with(&studio());
        case(&mut by_person, &person()).unwrap_or_else(|e| panic!("{name}: a person may: {e:?}"));
        let after = pairs(&graph(&by_person));
        match effect {
            Effect::Grows => assert!(after.is_superset(&before) && after != before, "{name}: grows"),
            Effect::Shrinks => assert!(after.is_subset(&before) && after != before, "{name}: shrinks"),
            Effect::Keeps => assert_eq!(after, before, "{name}: keeps"),
        }
        let mut by_agent = doc_with(&studio());
        let result = case(&mut by_agent, &agent());
        match effect {
            Effect::Grows => {
                assert!(needs_person(&result), "{name}: an agent needs a person, got {result:?}");
                let Err(e) = result else { unreachable!() };
                assert!(e.message.contains(" to "), "{name}: the refusal names the route: {}", e.message);
                assert_eq!(graph(&by_agent), studio(), "{name}: nothing changed");
            }
            _ => assert!(result.is_ok(), "{name}: an agent may: {result:?}"),
        }
    }
}

#[test]
fn a_person_unmutes_an_agent_cannot_and_undoing_an_agents_mute_restores_the_route() {
    let mut doc = doc_with(&studio());
    let (_, mute) = run(&mut doc, &agent(), &BusMute { bus: id("mic"), muted: true }).unwrap();
    assert!(!pairs(&graph(&doc)).iter().any(|(CapturedSource(r), _)| r.as_str() == "mic"));
    let unmute = run(&mut doc.clone(), &agent(), &BusMute { bus: id("mic"), muted: false });
    assert!(needs_person(&unmute), "an agent can't unmute the mic: {unmute:?}");
    // Undo of the agent's own mute (the executor applies the inverse patch) restores the person's state exactly.
    let mut undone = doc.clone();
    undone.apply(&mute.inverse()).unwrap();
    assert_eq!(graph(&undone), studio());
    // A person unmutes.
    run(&mut doc, &person(), &BusMute { bus: id("mic"), muted: false }).unwrap();
    assert_eq!(pairs(&graph(&doc)), pairs(&studio()));
}

#[test]
fn an_agents_gain_stays_under_the_ceiling_a_person_sets() {
    let mut doc = doc_with(&studio());
    let up = |db| BusSetGain { bus: id("music"), gain_db: db, max_gain_db: None };
    assert!(needs_person(&run(&mut doc, &agent(), &up(3.0))), "above the default 0 dB");
    run(&mut doc, &agent(), &up(0.0)).unwrap();
    let ceiling = BusSetGain { bus: id("music"), gain_db: 0.0, max_gain_db: Some(6.0) };
    assert!(needs_person(&run(&mut doc, &agent(), &ceiling)), "only a person sets the ceiling");
    run(&mut doc, &person(), &ceiling).unwrap();
    run(&mut doc, &agent(), &up(6.0)).unwrap();
    assert!(needs_person(&run(&mut doc, &agent(), &up(6.5))));
    let listed = run(&mut doc, &agent(), &BusList {}).unwrap().0;
    let music = listed["buses"].as_array().unwrap().iter().find(|b| b["bus"] == "music").unwrap();
    assert_eq!(music["gain_db"], json!(6.0));
    assert_eq!(music["ceiling_db"], json!(6.0));
}

#[test]
fn the_first_edit_creates_the_graph_and_undo_removes_it_whole() {
    let mut doc = MemDoc::new();
    let op = BusCreate {
        bus: id("program"),
        channels: Channels::Stereo,
        sources: Vec::new(),
        sends: Vec::new(),
        outputs: vec![role("program_out")],
        gain_db: 0.0,
    };
    let (_, patch) = run(&mut doc, &person(), &op).unwrap();
    assert_eq!(graph(&doc).buses.len(), 1);
    doc.apply(&patch.inverse()).unwrap();
    assert_eq!(doc.root(), MemDoc::new().root());
    assert!(doc.get(&graph_path()).is_none());
}

#[test]
fn bad_edits_are_refused_with_the_field_at_fault() {
    let mut doc = doc_with(&studio());
    let e = run(&mut doc, &person(), &BusMute { bus: id("drums"), muted: true }).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(e.hint.as_deref().unwrap_or_default().contains("program"));
    let cycle = BusRoute { bus: id("program"), sources: None, sends: Some(vec![send("mic")]), outputs: None };
    assert_eq!(run(&mut doc, &person(), &cycle).unwrap_err().code, ErrorCode::Invalid);
    let twice = BusCreate {
        bus: id("mic"),
        channels: Channels::Mono,
        sources: Vec::new(),
        sends: Vec::new(),
        outputs: Vec::new(),
        gain_db: 0.0,
    };
    assert_eq!(run(&mut doc, &person(), &twice).unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(graph(&doc), studio(), "nothing changed");
}

/// One random agent op from small pools of names, so ops keep hitting the same buses.
fn random_op(rng: &mut Rng, doc: &mut MemDoc) -> Result<(Value, Patch), OpError> {
    let buses = ["mic", "music", "game", "bot", "program", "extra"];
    let roles = ["mic", "music_in", "game_in", "line_in"];
    let outputs = ["program_out", "monitor_out", "phones"];
    let pick = |rng: &mut Rng, list: &[&'static str]| list[(rng.next_u64() % list.len() as u64) as usize];
    let some = |rng: &mut Rng, list: &[&'static str]| -> Vec<&'static str> {
        list.iter().copied().filter(|_| rng.next_f32() < 0.3).collect()
    };
    let agent = agent();
    match rng.next_u64() % 6 {
        0 => {
            let op = BusCreate {
                bus: id(pick(rng, &buses)),
                channels: if rng.next_f32() < 0.5 { Channels::Mono } else { Channels::Stereo },
                sources: some(rng, &roles).into_iter().map(device).collect(),
                sends: some(rng, &buses).into_iter().map(send).collect(),
                outputs: some(rng, &outputs).into_iter().map(role).collect(),
                gain_db: rng.range_f32(-12.0, 3.0),
            };
            run(doc, &agent, &op)
        }
        1 => run(doc, &agent, &BusDelete { bus: id(pick(rng, &buses)) }),
        2 => {
            let op = BusSetGain { bus: id(pick(rng, &buses)), gain_db: rng.range_f32(-24.0, 6.0), max_gain_db: None };
            run(doc, &agent, &op)
        }
        3 => run(doc, &agent, &BusMute { bus: id(pick(rng, &buses)), muted: rng.next_f32() < 0.5 }),
        4 => {
            let op = BusRoute {
                bus: id(pick(rng, &buses)),
                sources: (rng.next_f32() < 0.5).then(|| some(rng, &roles).into_iter().map(device).collect()),
                sends: (rng.next_f32() < 0.5).then(|| some(rng, &buses).into_iter().map(send).collect()),
                outputs: (rng.next_f32() < 0.5).then(|| some(rng, &outputs).into_iter().map(role).collect()),
            };
            run(doc, &agent, &op)
        }
        _ => {
            let op = BusDuck {
                target: id(pick(rng, &buses)),
                trigger: DuckTrigger::Bus { id: id(pick(rng, &buses)) },
                depth_db: Some(rng.range_f32(1.0, 24.0)),
                threshold_db: None,
                attack_ms: None,
                hold_ms: None,
                release_ms: None,
                remove: rng.next_f32() < 0.3,
            };
            run(doc, &agent, &op)
        }
    }
}

#[test]
fn no_sequence_of_agent_ops_sends_captured_audio_anywhere_new() {
    let mut rng = Rng::new(1157);
    let (mut applied, mut refused) = (0u32, 0u32);
    for sequence in 0..1_000 {
        let mut doc = doc_with(&studio());
        let mut reach = pairs(&studio());
        for step in 0..20 {
            match random_op(&mut rng, &mut doc) {
                Ok(_) => applied += 1,
                Err(_) => refused += 1,
            }
            let now = pairs(&graph(&doc));
            assert!(now.is_subset(&reach), "sequence {sequence}, step {step}: grew to {:?}", now.difference(&reach));
            reach = now;
        }
    }
    println!("egress property: 1000 agent sequences of 20 ops, {applied} applied, {refused} refused, none grew egress");
    assert!(applied > 5_000, "the generator mostly makes ops that apply: {applied}");
}
