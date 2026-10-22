//! `audio.fx.apply`: puts a preset's chain (or none) on the `mic` bus of the project's audio graph, which the live
//! engine follows. A preset with the same nodes as the bus has now only changes parameters; the engine then keeps the
//! nodes running and moves their parameters (design section 3.3), with no gap in the voice.

use omuzaki_audio_api::FxPreset;
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::{BusSpec, Channels, GRAPH_DOC_PATH, GraphSpec, MIC_BUS, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole};
use omuzaki_doc_api::{Change, DocPath, DocRead, Patch, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{check_chain, find};

/// The kind of node that runs a voice model, which only a person may put on the microphone (ADR 0016, SE-05).
pub const MODEL_KIND: &str = "model.stream";

fn graph_path() -> DocPath {
    GRAPH_DOC_PATH.iter().fold(DocPath::root(), |path, segment| path.join(*segment))
}

/// The graph the edit starts from: the document's, else the engine's, else an empty one.
fn current(ctx: &OpCtx<'_>) -> Result<GraphSpec, OpError> {
    if let Some(value) = ctx.doc.get(&graph_path()) {
        return serde_json::from_value(value.clone()).map_err(|e| {
            OpError::invalid("graph", format!("the project's audio graph is not in the graph format: {e}"))
                .hint("repair `audio/graph` in the project file, or undo the edit that broke it")
        });
    }
    Ok(ctx.service::<dyn AudioEngine>().map(AudioEngine::graph).unwrap_or_default())
}

/// The changes that set `path` to `value`, creating the `audio` table first if the document has none.
fn set(doc: &dyn DocRead, path: DocPath, value: Value) -> Vec<Change> {
    let mut changes = Vec::new();
    if let Some((table, _)) = path.split_last()
        && table != DocPath::root()
        && doc.get(&table).is_none()
    {
        changes.push(Change { path: table, before: None, after: Some(Value::Object(serde_json::Map::new())) });
    }
    changes.push(Change { before: doc.get(&path).cloned(), path, after: Some(value) });
    changes
}

/// Put a voice-effect preset on the microphone, or take every effect off.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "preset": "Robot" }, { "dry": true }], "x-omuzaki-fixture" = "audio"))]
pub struct Apply {
    /// The character whose preset it is. Without one, a built-in preset.
    #[serde(default)]
    pub character: Option<String>,
    /// The preset to apply. Give this or `dry`.
    #[serde(default)]
    pub preset: Option<String>,
    /// `true` takes every effect off the microphone.
    #[serde(default)]
    pub dry: bool,
}

impl Op for Apply {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.fx.apply",
            summary: "Put a voice-effect preset on the microphone (a character's own or a built-in one), or take every \
                      effect off with `dry`. The live voice changes at once; undo puts the last one back.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let preset = match (&self.preset, self.dry) {
            (Some(name), false) => find(ctx.doc, self.character.as_deref(), name)?.0,
            (None, true) => FxPreset { name: "dry".to_owned(), chain: Vec::new() },
            _ => {
                return Err(OpError::invalid("preset", "give either `preset` or `dry: true`")
                    .hint("audio.fx.preset.list lists the presets"));
            }
        };
        check_chain(&preset.chain)?;
        if let Some(model) = preset.chain.iter().find(|n| n.kind.as_str() == MODEL_KIND) {
            // A voice model may be unacknowledged or carry a real person's voice; neither is an agent's call.
            ctx.require_person(format!(
                "the preset `{}` runs a voice model (node `{}`) on your microphone",
                preset.name, model.id
            ))?;
        }
        let mut graph = current(ctx)?;
        let mic = BusId::new(MIC_BUS).map_err(|e| OpError::internal(e.to_string()))?;
        if graph.bus(&mic).is_none() {
            // The bus the engine adds for a bound microphone (`audio.mic`), so the chain has a place to live.
            let role = DeviceRole::new("mic").map_err(|e| OpError::internal(e.to_string()))?;
            graph.buses.push(BusSpec {
                id: mic.clone(),
                channels: Channels::Mono,
                sources: vec![SourceRef::Device { role }],
                chain: Vec::new(),
                gain_db: 0.0,
                muted: false,
                sends: Vec::new(),
                outputs: Vec::new(),
            });
        }
        let Some(bus) = graph.buses.iter_mut().find(|b| b.id == mic) else {
            return Err(OpError::internal("the mic bus went missing"));
        };
        let parameters_only = preset.keeps_shape_of(&bus.chain);
        bus.chain.clone_from(&preset.chain);
        graph.validate_registered().map_err(|e| {
            OpError::invalid("preset", format!("the mixer would be invalid with this preset: {e}"))
                .hint("every effect on the mic bus must run on a mono bus; audio.fx.kinds lists which do")
        })?;
        let latency: u32 = preset
            .chain
            .iter()
            .filter_map(|n| omuzaki_audio_api::node::node_kind(&n.kind).map(|k| k.info().latency.max))
            .sum();
        let value = serde_json::to_value(&graph).map_err(|e| OpError::internal(e.to_string()))?;
        let patch = Patch { changes: set(ctx.doc, graph_path(), value) };
        Ok(Plan {
            output: Some(json!({
                "applied": preset.name,
                "effects": preset.chain.iter().map(|n| n.kind.as_str()).collect::<Vec<_>>(),
                "parameters_only": parameters_only,
                "latency_ms": f64::from(latency) / 48.0,
            })),
            ..Plan::edit(patch)
        })
    }
}

register_op!(Apply);
