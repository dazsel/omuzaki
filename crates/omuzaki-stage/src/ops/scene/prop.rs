//! A scene's props (design D7, D8, #705): `stage.add_prop`, `stage.set_prop`, `stage.remove_prop`, and
//! `stage.prop_kinds` to find what can be added.
//!
//! A prop's `params` are checked by its kind's own schema and stored as the kind returns them, every default filled
//! in, so the document says what is drawn and a one-param edit is a one-path patch.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_core::text::nearest;
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::{
    Anchor, MAX_PROPS, PropDef, PropError, PropId, PropKindId, SceneId, find_prop_kind, prop_kinds,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::edit::{OrOff, SceneEdit, unknown};

/// The registered kind `kind`, or `NotFound` with the nearest one and how to list them.
fn kind_of(kind: &PropKindId) -> Result<&'static omuzaki_stage_api::props::PropKindRegistration, OpError> {
    find_prop_kind(kind.as_str()).ok_or_else(|| {
        let ids: Vec<&str> = prop_kinds().iter().map(|k| k.id).collect();
        let mut error = OpError::not_found(format!("there is no prop kind '{kind}'"));
        error.field = Some("kind".to_owned());
        match nearest(kind.as_str(), ids.iter().copied()) {
            Some(near) => error.hint(format!("did you mean '{near}'? stage.prop_kinds lists them: {}", ids.join(", "))),
            None => error.hint(format!("stage.prop_kinds lists them: {}", ids.join(", "))),
        }
    })
}

/// `params` checked by `kind`, every default filled in; a refusal names `params.<field>`.
fn checked(kind: &omuzaki_stage_api::props::PropKindRegistration, params: &Value) -> Result<Value, OpError> {
    kind.check(params).map_err(|error| {
        // A kind's rule starts with the field it is about (`width is 0.6 to 2.4, got 9`).
        let field = match &error {
            PropError::Params { reason, .. } => reason
                .split_once(" is ")
                .map(|(field, _)| field)
                .filter(|f| !f.is_empty() && !f.contains(' '))
                .map_or_else(|| "params".to_owned(), |f| format!("params.{f}")),
            _ => "params".to_owned(),
        };
        OpError::invalid(field, error.to_string()).hint(format!("stage.prop_kinds shows what a {} takes", kind.id))
    })
}

/// `over` laid onto `base` key by key (one level: a kind's params are flat), for an edit that names some params.
fn merged(base: &Value, over: &Value) -> Value {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            let mut out = base.clone();
            for (key, value) in over {
                out.insert(key.clone(), value.clone());
            }
            Value::Object(out)
        }
        _ => over.clone(),
    }
}

/// Adds a prop to a scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"kind": "desk", "position": [0.0, 0.0, 0.45]},
        {"scene": "night_stream", "id": "reading-lamp", "kind": "lamp", "params": {"style": "floor", "intensity": 3.0}, "position": [-1.1, 0.0, -0.6]},
        {"kind": "cup", "anchor": "subject", "attach": "rightHand", "position": [0.0, -0.04, 0.05], "rotation": [0.0, 0.0, 90.0]}
    ],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct AddProp {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// Its id; the kind's name (then `-2`, `-3`...) when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<PropId>,
    /// The kind: `room`, `desk`, `chair`, `shelf`, `lamp`, `window`, `cup`... (`stage.prop_kinds` lists them).
    pub kind: PropKindId,
    /// The kind's parameters; what is left out takes the kind's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// What `position` is measured from: `stage` (the default), `subject` or `member:<id>`, in metres either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// A humanoid bone of the anchor member to follow (`rightHand`, `head`); `position` and `rotation` are then in
    /// that bone's frame. Needs a `subject` or `member:<id>` anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<HumanoidBone>,
    /// Metres from the anchor (x right, y up, z toward the front camera); the origin when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// Yaw, pitch and roll in degrees (about +Y, then +X, then +Z).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<[f32; 3]>,
    /// A uniform scale above 0; 1 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// Whether it is drawn; true when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Whether it shadows the cast and the other props; true when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub casts_shadow: Option<bool>,
}

impl Op for AddProp {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.add_prop",
            summary: "Add a prop to a scene: furniture, a room, a lamp or window with its light, or a thing held in a \
                      hand (anchor it to a member and attach it to a bone). Params are the kind's own.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let kind = kind_of(&self.kind)?;
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let props = &mut edit.def.props;
        if props.len() >= MAX_PROPS {
            return Err(OpError::invalid(
                "kind",
                format!("the scene has {} props, the most a scene can have", props.len()),
            )
            .hint("remove one with stage.remove_prop"));
        }
        let id = match &self.id {
            Some(id) if props.contains_key(id) => {
                return Err(OpError::conflict(format!("the scene already has a prop '{id}'"))
                    .hint("pick another id, or change that prop with stage.set_prop"));
            }
            Some(id) => id.clone(),
            None => {
                let free = super::edit::free_id(kind.id, &|s| props.keys().any(|k| k.as_str() == s));
                PropId::new(&free).map_err(|e| OpError::internal(format!("prop id '{free}': {e}")))?
            }
        };
        let mut prop = PropDef::new(self.kind.clone());
        prop.params = checked(kind, self.params.as_ref().unwrap_or(&json!({})))?;
        if let Some(anchor) = &self.anchor {
            prop.anchor = anchor.clone();
        }
        prop.attach = self.attach;
        prop.position = self.position.unwrap_or_default();
        prop.rotation = self.rotation.unwrap_or_default();
        prop.scale = self.scale.unwrap_or(1.0);
        prop.visible = self.visible.unwrap_or(true);
        prop.casts_shadow = self.casts_shadow.unwrap_or(true);
        props.insert(id.clone(), prop);
        let output = json!({ "scene": edit.id.as_str(), "prop": id.as_str(), "kind": kind.id });
        edit.finish(ctx, output)
    }
}

register_op!(AddProp);

/// Changes a prop of a scene: only the fields given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"prop": "desk", "position": [0.2, 0.0, 0.5]},
        {"prop": "desk", "params": {"width": 1.6, "colour": "#6b4a32"}},
        {"prop": "cup", "attach": "leftHand"},
        {"prop": "cup", "visible": false}
    ],
    "x-omuzaki-fixture" = "scenes-props"
))]
pub struct SetProp {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The prop.
    pub prop: PropId,
    /// Params to change; the others keep their values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// What `position` is measured from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// The bone to follow, or `off` to stand free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<OrOff<HumanoidBone>>,
    /// Metres from the anchor (or the bone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// Yaw, pitch and roll in degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<[f32; 3]>,
    /// A uniform scale above 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// Whether it is drawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Whether it shadows the cast and the other props.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub casts_shadow: Option<bool>,
}

impl Op for SetProp {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_prop",
            summary: "Change a prop of a scene: its params (only the ones given), where it stands or which bone it \
                      follows, its turn and scale, whether it shows or casts a shadow.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let known: Vec<String> = edit.def.props.keys().map(ToString::to_string).collect();
        let prop = edit
            .def
            .props
            .get_mut(&self.prop)
            .ok_or_else(|| unknown("prop", "prop", self.prop.as_str(), known))?;
        if let Some(params) = &self.params {
            let kind = kind_of(&prop.kind)?;
            prop.params = checked(kind, &merged(&prop.params, params))?;
        }
        if let Some(anchor) = &self.anchor {
            prop.anchor = anchor.clone();
        }
        if let Some(attach) = &self.attach {
            prop.attach = attach.clone().into_option();
        }
        if let Some(position) = self.position {
            prop.position = position;
        }
        if let Some(rotation) = self.rotation {
            prop.rotation = rotation;
        }
        if let Some(scale) = self.scale {
            prop.scale = scale;
        }
        if let Some(visible) = self.visible {
            prop.visible = visible;
        }
        if let Some(casts_shadow) = self.casts_shadow {
            prop.casts_shadow = casts_shadow;
        }
        let output = json!({ "scene": edit.id.as_str(), "prop": self.prop.as_str() });
        edit.finish(ctx, output)
    }
}

register_op!(SetProp);

/// Removes a prop from a scene.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"prop": "cup"}], "x-omuzaki-fixture" = "scenes-props"))]
pub struct RemoveProp {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The prop.
    pub prop: PropId,
}

impl Op for RemoveProp {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.remove_prop",
            summary: "Remove a prop from a scene.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let known: Vec<String> = edit.def.props.keys().map(ToString::to_string).collect();
        if edit.def.props.remove(&self.prop).is_none() {
            return Err(unknown("prop", "prop", self.prop.as_str(), known));
        }
        let output = json!({ "scene": edit.id.as_str(), "removed": self.prop.as_str() });
        edit.finish(ctx, output)
    }
}

register_op!(RemoveProp);

/// Lists the prop kinds a scene can hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, {"kind": "lamp"}]))]
pub struct PropKinds {
    /// One kind, with its parameter schema and defaults; every kind with its summary when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<PropKindId>,
}

impl Op for PropKinds {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.prop_kinds",
            summary: "List the prop kinds stage.add_prop takes, or one kind's parameters (their schema and defaults).",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if let Some(kind) = &self.kind {
            let kind = kind_of(kind)?;
            return Ok(Plan::answer(json!({
                "kind": kind.id,
                "summary": kind.summary,
                "params": kind.schema(),
                "defaults": kind.defaults(),
            })));
        }
        let kinds: Vec<Value> = prop_kinds().iter().map(|k| json!({ "kind": k.id, "summary": k.summary })).collect();
        Ok(Plan::answer(json!({ "kinds": kinds })))
    }
}

register_op!(PropKinds);
