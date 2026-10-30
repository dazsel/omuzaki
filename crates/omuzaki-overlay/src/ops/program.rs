//! `program.*`: what goes out (spec v3 ST-04). `program.switch_scene` puts another scene on the program with a
//! transition, from a hotkey, a Stream Deck key, a chat command through an automation, or an agent; the program's
//! scene is live state, so switching changes no document and has no undo (switch back instead).
//! `program.set_transition` sets the default transition, and `program.set_adaptive` how the frame holds its budget;
//! both are saved with the project and undoable.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::{SceneId, paths as scene_paths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{merge, put, service};
use crate::model::{AdaptiveSpec, paths};
use crate::transition::{Direction, TransitionKind, TransitionSpec};

/// Put another scene on the program, with a transition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "scene": "brb" },
    { "scene": "main", "transition": { "kind": "iris", "ms": 600 } },
    { "scene": "night_stream", "transition": { "kind": "dip", "ms": 800, "colour": "#ffffff" } }
]))]
pub struct SwitchScene {
    /// The scene to put on.
    pub scene: SceneId,
    /// How to get there; default the project's transition (`program.set_transition`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<TransitionSpec>,
}

/// Set the transition `program.switch_scene` uses when it names none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "kind": "wipe", "ms": 500, "direction": "right" }, { "kind": "cut" }]))]
pub struct SetTransition {
    /// What it looks like: `cut`, `fade`, `dip`, `wipe`, `slide` or `iris`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<TransitionKind>,
    /// How long it takes, milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u32>,
    /// The colour a dip passes through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    /// Which way a wipe or slide moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
}

/// Turn adaptive quality on or off, and say whether it may change the look while a take records.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "enabled": true }, { "while_recording": true }, { "enabled": false }]))]
pub struct SetAdaptive {
    /// Lower quality to hold the frame budget when the GPU is busy (a game beside the show), and raise it again when
    /// there is room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Whether quality may also change while a take records (default no: a recording keeps one look).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub while_recording: Option<bool>,
}

/// List the kinds of scene transition.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Transitions {}

/// Report the program: its scene, a transition under way, the overlay and what composing costs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

impl Op for SwitchScene {
    fn meta() -> OpMeta {
        OpMeta {
            name: "program.switch_scene",
            summary: "Put another scene on the program (what viewers see), with a transition.",
            kind: OpKind::Edit,
            scope: Scope::Broadcast,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if ctx.doc.get(&scene_paths::scene(&self.scene)).is_none() {
            return Err(OpError::not_found(format!("there is no scene '{}'", self.scene))
                .hint("stage.list_scenes lists the project's scenes"));
        }
        if let Some(spec) = &self.transition {
            spec.check().map_err(|(field, why)| OpError::invalid(format!("transition.{field}"), why))?;
        }
        let service = service(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "scene": self.scene.as_str() })));
        }
        service.request_switch(self.scene.as_str().to_owned(), self.transition.clone());
        Ok(Plan::answer(json!({ "switching_to": self.scene.as_str() })))
    }
}

impl Op for SetTransition {
    fn meta() -> OpMeta {
        OpMeta {
            name: "program.set_transition",
            summary: "Set the transition scene switches use when they name none.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = paths::settings().join("transition");
        let mut value = ctx
            .doc
            .get(&at)
            .cloned()
            .unwrap_or_else(|| serde_json::to_value(TransitionSpec::default()).unwrap_or(Value::Null));
        let changes = serde_json::to_value(self).map_err(|e| OpError::internal(e.to_string()))?;
        merge(&mut value, &changes);
        let spec: TransitionSpec =
            serde_json::from_value(value).map_err(|e| OpError::invalid("transition", e.to_string()))?;
        spec.check().map_err(|(field, why)| OpError::invalid(field, why))?;
        let value = serde_json::to_value(&spec).map_err(|e| OpError::internal(e.to_string()))?;
        let patch = if ctx.doc.get(&at) == Some(&value) {
            omuzaki_doc_api::Patch::new()
        } else {
            put(ctx.doc, &at, value.clone())
        };
        Ok(Plan { patch, output: Some(json!({ "transition": value })), ..Plan::default() })
    }
}

impl Op for SetAdaptive {
    fn meta() -> OpMeta {
        OpMeta {
            name: "program.set_adaptive",
            summary: "Turn adaptive quality on or off: lower quality to hold the frame budget beside a game.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = paths::settings().join("adaptive");
        let mut spec: AdaptiveSpec = match ctx.doc.get(&at) {
            Some(value) => serde_json::from_value(value.clone()).unwrap_or_default(),
            None => AdaptiveSpec::default(),
        };
        if let Some(enabled) = self.enabled {
            spec.enabled = Some(enabled);
        }
        if let Some(while_recording) = self.while_recording {
            spec.while_recording = while_recording;
        }
        let value = serde_json::to_value(&spec).map_err(|e| OpError::internal(e.to_string()))?;
        let patch = if ctx.doc.get(&at) == Some(&value) {
            omuzaki_doc_api::Patch::new()
        } else {
            put(ctx.doc, &at, value.clone())
        };
        Ok(Plan { patch, output: Some(json!({ "adaptive": value })), ..Plan::default() })
    }
}

impl Op for Transitions {
    fn meta() -> OpMeta {
        OpMeta {
            name: "program.transitions",
            summary: "List the kinds of scene transition.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let kinds: Vec<Value> =
            TransitionKind::ALL.iter().map(|k| json!({ "kind": k.name(), "summary": k.summary() })).collect();
        Ok(Plan::answer(json!({ "transitions": kinds, "default": TransitionSpec::default() })))
    }
}

impl Op for Status {
    fn meta() -> OpMeta {
        OpMeta {
            name: "program.status",
            summary: "Report the program: its scene, a transition under way, the overlay and what composing costs.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(service(ctx)?.status()))
    }
}

register_op!(SwitchScene);
register_op!(SetTransition);
register_op!(SetAdaptive);
register_op!(Transitions);
register_op!(Status);
