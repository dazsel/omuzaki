//! The `audio.fx.*` ops (#1265): each op as an agent and as a person, the preset file's round trip through TOML, undo,
//! applying presets to the microphone (with the SE-05 refusal of a voice model for agents), and the render jobs:
//! `compare` of dry against +5 semitones reports an F0 ratio of 1.335 within 1%.

#![allow(clippy::unwrap_used, clippy::panic)]

use omuzaki_audio_api::graph::{Channels, GraphSpec, NodeSpec};
use omuzaki_audio_api::ids::{BusId, NodeId, NodeKindId};
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo, ParamIndex,
};
use omuzaki_audio_api::{VoiceFile, register_node_kind};
use omuzaki_audio_fx::ops::apply::Apply;
use omuzaki_audio_fx::ops::preset::{
    Kinds, PresetDelete, PresetGet, PresetList, PresetRename, PresetSave, PresetSetParam,
};
use omuzaki_audio_fx::ops::render::{Clip, Compare, Render};
use omuzaki_audio_fx::ops::voice_path;
use omuzaki_audio_fx::presets::builtin;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, CancelFlag, ErrorCode, ErrorDetail, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, ServicesBuilder,
};
use serde_json::{Value, json};

/// A stand-in for the voice-model node (ML-14): only its kind id matters here.
struct ModelStream;
register_node_kind!(ModelStream);

struct Silent;
impl Node for Silent {
    fn latency(&self) -> u32 {
        0
    }
    fn set_param(&mut self, _: ParamIndex, _: f32) {}
    fn process(&mut self, _: &mut BlockMut<'_>) {}
    fn reset(&mut self) {}
}

impl NodeKind for ModelStream {
    fn info(&self) -> NodeKindInfo {
        NodeKindInfo {
            id: "model.stream",
            summary: "a voice model (test stand-in)",
            channels: ChannelSupport::Any,
            params: &[],
            latency: LatencyRange { min: 0, max: 0 },
            sidechain: false,
        }
    }
    fn create(&self, _: &NodeConfig, _: Channels) -> Result<Box<dyn Node>, NodeError> {
        Ok(Box::new(Silent))
    }
}

fn person() -> Actor {
    Actor::Person { name: "tester".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "agent".into(), model: None }
}

fn studio() -> MemDoc {
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira" } } }))
}

/// Plans `op` as `actor` and applies its patch; returns the plan's output and the patch.
fn run(doc: &mut MemDoc, actor: &Actor, op: &impl Op) -> Result<(Value, Patch), OpError> {
    omuzaki_audio_fx::link();
    let services = ServicesBuilder::new().build();
    let clock = FakeClock::default();
    let plan = {
        let ctx = OpCtx::new(doc, actor, &clock).with_services(&services);
        op.plan(&ctx)?
    };
    doc.apply(&plan.patch).map_err(|e| OpError::internal(e.to_string()))?;
    Ok((plan.output.unwrap_or(Value::Null), plan.patch))
}

fn voice(doc: &MemDoc) -> VoiceFile {
    serde_json::from_value(doc.get(&voice_path("mira")).cloned().unwrap_or(json!({}))).unwrap()
}

fn mic_chain(doc: &MemDoc) -> Vec<NodeSpec> {
    let path = DocPath::root().join("audio").join("graph");
    let graph: GraphSpec = serde_json::from_value(doc.get(&path).cloned().unwrap()).unwrap();
    graph.bus(&BusId::new("mic").unwrap()).unwrap().chain.clone()
}

fn save(preset: &str, copy: &str) -> PresetSave {
    PresetSave { character: "mira".into(), preset: preset.into(), chain: None, copy: Some(copy.into()) }
}

#[test]
fn kinds_lists_every_effect_with_its_parameters() {
    let (out, _) = run(&mut studio(), &agent(), &Kinds {}).unwrap();
    let ids: Vec<&str> = out["kinds"].as_array().unwrap().iter().map(|k| k["id"].as_str().unwrap()).collect();
    for id in ["fx.compressor", "fx.eq", "fx.gate", "fx.limiter", "fx.pitch", "fx.radio", "fx.robot"] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    let pitch = out["kinds"].as_array().unwrap().iter().find(|k| k["id"] == "fx.pitch").unwrap();
    assert_eq!(pitch["params"][0]["name"], "semitones");
    assert_eq!(pitch["latency"]["max"], 912);
}

#[test]
fn list_and_get_show_the_builtins_and_the_characters_own() {
    let mut doc = studio();
    let (out, _) = run(&mut doc, &agent(), &PresetList { character: None }).unwrap();
    assert_eq!(out["builtin"].as_array().unwrap().len(), builtin().len());
    assert_eq!(out["presets"], json!([]));
    let missing = run(&mut doc, &agent(), &PresetList { character: Some("nobody".into()) }).unwrap_err();
    assert_eq!((missing.code, missing.field.as_deref()), (ErrorCode::NotFound, Some("character")));

    run(&mut doc, &agent(), &save("Mira low", "Deeper")).unwrap();
    let (out, _) = run(&mut doc, &agent(), &PresetList { character: Some("mira".into()) }).unwrap();
    assert_eq!(out["presets"][0]["name"], "Mira low");
    assert_eq!(out["presets"][0]["source"], "character");
    let (got, _) =
        run(&mut doc, &person(), &PresetGet { character: Some("mira".into()), preset: "Mira low".into() }).unwrap();
    assert_eq!(got["preset"]["chain"][2]["kind"], "fx.pitch");
    let (got, _) = run(&mut doc, &person(), &PresetGet { character: None, preset: "Robot".into() }).unwrap();
    assert_eq!(got["source"], "builtin");
    let unknown = run(&mut doc, &agent(), &PresetGet { character: None, preset: "Nope".into() }).unwrap_err();
    assert!(unknown.hint.as_deref().unwrap_or_default().contains("Telephone"), "{unknown:?}");
}

#[test]
fn an_agent_and_a_person_edit_presets_and_undo_restores_each_edit() {
    for actor in [agent(), person()] {
        let mut doc = studio();
        let (_, saved) = run(&mut doc, &actor, &save("Mira low", "Deeper")).unwrap();
        assert_eq!(voice(&doc).presets.len(), 1);

        let set = |value: f32| PresetSetParam {
            character: "mira".into(),
            preset: "Mira low".into(),
            node: "pitch".into(),
            param: "semitones".into(),
            value,
        };
        let (_, changed) = run(&mut doc, &actor, &set(-6.0)).unwrap();
        let pitch =
            |doc: &MemDoc| voice(doc).presets[0].chain.iter().find(|n| n.id.as_str() == "pitch").unwrap().clone();
        assert_eq!(pitch(&doc).params["semitones"], -6.0);
        // Back to the default leaves the parameter out of the file.
        run(&mut doc, &actor, &set(0.0)).unwrap();
        assert!(!pitch(&doc).params.contains_key("semitones"));
        let range = run(&mut doc, &actor, &set(30.0)).unwrap_err();
        assert_eq!((range.code, range.field.as_deref()), (ErrorCode::Invalid, Some("param")));
        let mut bad = set(1.0);
        bad.param = "octaves".into();
        assert!(run(&mut doc, &actor, &bad).unwrap_err().message.contains("semitones"));

        run(&mut doc, &actor, &save("Second", "Robot")).unwrap();
        let taken = PresetRename { character: "mira".into(), preset: "Second".into(), to: "Mira low".into() };
        assert_eq!(run(&mut doc, &actor, &taken).unwrap_err().code, ErrorCode::Conflict);
        let rename = PresetRename { character: "mira".into(), preset: "Second".into(), to: "Mira robot".into() };
        let (_, renamed) = run(&mut doc, &actor, &rename).unwrap();
        assert!(voice(&doc).get("Mira robot").is_some());

        let delete = PresetDelete { character: "mira".into(), preset: "Mira robot".into() };
        let (_, deleted) = run(&mut doc, &actor, &delete).unwrap();
        assert!(voice(&doc).get("Mira robot").is_none());
        doc.apply(&deleted.inverse()).unwrap();
        doc.apply(&renamed.inverse()).unwrap();
        assert!(voice(&doc).get("Second").is_some(), "undo of delete and rename");
        let _ = changed;
        // Undo back to before the first save: the character has no voice file again.
        let all = voice(&doc);
        assert_eq!(all.presets.len(), 2);
        let mut undo_all = doc.clone();
        undo_all
            .apply(&Patch {
                changes: vec![omuzaki_doc_api::Change {
                    path: voice_path("mira"),
                    before: undo_all.get(&voice_path("mira")).cloned(),
                    after: saved.changes[0].before.clone(),
                }],
            })
            .unwrap();
        assert!(undo_all.get(&voice_path("mira")).is_none());
    }
}

#[test]
fn save_refuses_an_unknown_kind_and_a_missing_character() {
    let mut doc = studio();
    let chain = vec![NodeSpec {
        id: NodeId::new("x").unwrap(),
        kind: NodeKindId::new("fx.nothing").unwrap(),
        config: NodeConfig::default(),
        params: Default::default(),
        enabled: true,
        sidechain: None,
    }];
    let op = PresetSave { character: "mira".into(), preset: "Bad".into(), chain: Some(chain), copy: None };
    let error = run(&mut doc, &agent(), &op).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.hint.unwrap().contains("audio.fx.kinds"));
    let op = PresetSave { character: "nobody".into(), preset: "A".into(), chain: None, copy: Some("Clean".into()) };
    assert_eq!(run(&mut doc, &agent(), &op).unwrap_err().code, ErrorCode::NotFound);
}

#[test]
fn the_voice_file_round_trips_through_toml() {
    let file = VoiceFile { presets: builtin() };
    let text = toml::to_string(&file).unwrap();
    let back: VoiceFile = toml::from_str(&text).unwrap();
    assert_eq!(back, file);
    assert!(text.contains("[[preset]]"), "{text}");
}

#[test]
fn apply_puts_a_preset_on_the_mic_and_a_matching_one_changes_only_parameters() {
    let mut doc = studio();
    let (out, applied) = run(&mut doc, &agent(), &Apply { preset: Some("Deeper".into()), ..Apply::default() }).unwrap();
    assert_eq!(out["parameters_only"], false);
    assert_eq!(mic_chain(&doc).len(), 4);
    // The same shape with other values: a parameter change.
    run(&mut doc, &agent(), &save("Mira low", "Deeper")).unwrap();
    let set = PresetSetParam {
        character: "mira".into(),
        preset: "Mira low".into(),
        node: "pitch".into(),
        param: "semitones".into(),
        value: -7.0,
    };
    run(&mut doc, &agent(), &set).unwrap();
    let (out, _) =
        run(&mut doc, &agent(), &Apply { character: Some("mira".into()), preset: Some("Mira low".into()), dry: false })
            .unwrap();
    assert_eq!(out["parameters_only"], true);
    assert_eq!(mic_chain(&doc)[2].params["semitones"], -7.0);
    let (out, _) = run(&mut doc, &person(), &Apply { dry: true, ..Apply::default() }).unwrap();
    assert_eq!(out["applied"], "dry");
    assert!(mic_chain(&doc).is_empty());
    let neither = run(&mut doc, &agent(), &Apply::default()).unwrap_err();
    assert_eq!(neither.code, ErrorCode::Invalid);
    // Undo of the first apply removes the graph it created, and the `audio` table with it.
    let mut fresh = studio();
    fresh.apply(&applied).unwrap();
    fresh.apply(&applied.inverse()).unwrap();
    assert!(fresh.get(&DocPath::root().join("audio")).is_none());
}

#[test]
fn a_voice_model_on_the_mic_needs_a_person() {
    let mut doc = studio();
    let chain = vec![NodeSpec {
        id: NodeId::new("vc").unwrap(),
        kind: NodeKindId::new("model.stream").unwrap(),
        config: NodeConfig::default(),
        params: Default::default(),
        enabled: true,
        sidechain: None,
    }];
    let op = PresetSave { character: "mira".into(), preset: "Clone".into(), chain: Some(chain), copy: None };
    run(&mut doc, &agent(), &op).unwrap();
    let apply = Apply { character: Some("mira".into()), preset: Some("Clone".into()), dry: false };
    let refused = run(&mut doc, &agent(), &apply).unwrap_err();
    assert!(matches!(refused.detail.as_deref(), Some(ErrorDetail::NeedsPerson { .. })), "{refused:?}");
    run(&mut doc, &person(), &apply).unwrap();
    assert_eq!(mic_chain(&doc)[0].kind.as_str(), "model.stream");
}

#[derive(Default)]
struct Signal(CancelFlag);

impl JobSignal for Signal {
    fn progress(&self, _: &str, _: f32) {}
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancel_flag(&self) -> CancelFlag {
        self.0.clone()
    }
    fn spend(&self, _: u64) -> Result<(), OpError> {
        Ok(())
    }
}

struct Doc(MemDoc);

impl JobDoc for Doc {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(&self.0);
    }
}

/// Plans and works a job as `actor` on `doc`.
fn job(doc: &MemDoc, actor: &Actor, op: &impl Op) -> Result<Value, OpError> {
    omuzaki_audio_fx::link();
    let services = ServicesBuilder::new().build();
    let clock = FakeClock::default();
    op.plan(&OpCtx::new(doc, actor, &clock).with_services(&services))?;
    let (doc, signal) = (Doc(doc.clone()), Signal::default());
    op.work(&JobCtx::new(JobId(1), actor, &doc, &signal).with_services(&services))
}

#[test]
fn compare_of_dry_against_plus_5_semitones_reports_an_f0_ratio_of_1_335() {
    let mut doc = studio();
    let chain = vec![NodeSpec {
        id: NodeId::new("pitch").unwrap(),
        kind: NodeKindId::new("fx.pitch").unwrap(),
        config: NodeConfig::default(),
        params: [("semitones".to_owned(), 5.0)].into_iter().collect(),
        enabled: true,
        sidechain: None,
    }];
    run(
        &mut doc,
        &agent(),
        &PresetSave { character: "mira".into(), preset: "Up 5".into(), chain: Some(chain), copy: None },
    )
    .unwrap();
    let out = job(
        &doc,
        &agent(),
        &Compare { character: Some("mira".into()), a: "dry".into(), b: "Up 5".into(), clip: Clip::default() },
    )
    .unwrap();
    let ratio = out["b_against_a"]["f0_ratio"].as_f64().unwrap();
    let expected = 2f64.powf(5.0 / 12.0);
    eprintln!(
        "compare dry / +5 st: F0 ratio {ratio:.4} (expected {expected:.4}), formant ratios {}, loudness {} dB, latency {} ms",
        out["b_against_a"]["formant_ratios"], out["b_against_a"]["loudness_db"], out["b_against_a"]["latency_ms"]
    );
    assert!((ratio / expected - 1.0).abs() < 0.01, "{ratio}");
    assert!((out["a"]["output"]["median_f0_hz"].as_f64().unwrap() - 120.0).abs() < 1.0);
    assert_eq!(out["b_against_a"]["latency_ms"], 19.0);
}

#[test]
fn render_measures_a_preset_and_a_recording_is_person_only() {
    let doc = studio();
    let out = job(
        &doc,
        &agent(),
        &Render { character: None, preset: Some("Giant".into()), chain: None, clip: Clip::default() },
    )
    .unwrap();
    let f0 = out["rendered"]["output"]["median_f0_hz"].as_f64().unwrap();
    let st = 12.0 * (f0 / 120.0).log2();
    eprintln!("render Giant: F0 {f0:.1} Hz ({st:+.2} st), output {}", out["rendered"]["output"]);
    assert!((st + 9.0).abs() < 0.2, "{st}");
    let file = Render { character: None, preset: Some("Clean".into()), chain: None, clip: Clip::File("me.wav".into()) };
    let refused = job(&doc, &agent(), &file).unwrap_err();
    assert!(matches!(refused.detail.as_deref(), Some(ErrorDetail::PersonOnly { .. })), "{refused:?}");
}
