//! `part.add`: put a new part of any registered kind into a character.

use omuzaki_char_api::{
    CharId, CharacterDef, PartDef, PartId, PartKindId, PartKindRegistration, find_kind, paths,
};
use omuzaki_doc_api::Value;
use omuzaki_face_api::ParamErrors;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{list_kinds, source};
use crate::ops::character::access::{self, list};
use crate::ops::character::tree::{self, Problem};

/// Adds a part to a character: hair, a garment, an accessory, a body or a head.
///
/// `kind` must be a registered part kind and `id` a name not yet used in the character. The part starts enabled with
/// the kind's default parameters; `params` changes only the ones it names, and a wrong value is refused with its
/// path. Hair and garments are built over other parts, so they need an enabled part of the kind they require (a shirt
/// needs a body, hair needs a head): add those first. `layer` orders building from the inside out.
///
/// A new part has no source row (`licence.set_source`), so it counts as the most restrictive part possible until one is
/// recorded; a part of a project-owned kind needs none. A row left under the id by a part that no longer exists is
/// removed, so the new part never inherits another part's rights.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartAdd {
    /// The character to add the part to.
    pub character: CharId,
    /// The part kind, such as `hair` or `head`: it selects the generator and the parameters. Must be registered; the
    /// error for an unknown kind lists the ones that are.
    pub kind: PartKindId,
    /// The new part's id: lowercase letters, digits, `-` and `_`. Must not be used by another part of this character.
    /// Other ops call the part by it, and it appears in saved projects and variants, so choose a lasting name
    /// (`hair-long`, `shirt`).
    pub id: PartId,
    /// The part this one belongs to (a hood to its robe). A disabled parent disables its children. Must exist.
    /// Leave out for a top-level part.
    #[serde(default)]
    pub parent: Option<PartId>,
    /// Build order from inner to outer: body and head 0, shirt 20, vest 30, outer garments 40 and up, accessories 50
    /// and up. A part builds after its parent, after every part with a lower layer and after the kinds it requires.
    /// Left out, the kind's usual layer is used.
    #[serde(default)]
    pub layer: Option<i32>,
    /// The kind's parameters as an object. Only the keys you give change; the rest keep the kind's defaults. See
    /// `part.describe` for a kind's parameters and ranges.
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, Value>>")]
    pub params: Option<Value>,
}

/// The starting parameters: the kind's defaults, or `given` laid over them, normalised through the kind.
fn starting_params(kind: &PartKindRegistration, given: Option<&Value>) -> Result<Value, OpError> {
    if let Some(given) = given
        && !given.is_object()
    {
        return Err(OpError::invalid(
            "params",
            format!("`params` must be an object of parameter names and values, not {}", json_type(given)),
        )
        .hint("give an object such as {\"ease\": 0.01}, or leave `params` out for the kind's defaults"));
    }
    let raw = given.cloned().unwrap_or_else(|| (kind.defaults)());
    (kind.check)(&raw).map_err(|errors| params_error(&errors))
}

/// A JSON value's type, as a person would say it.
fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// The first rejected parameter names the field (`params/ease`); the message lists every one.
fn params_error(errors: &ParamErrors) -> OpError {
    let first = errors.0.first().map_or("params", |e| e.path.as_str());
    let field = if first == "params" { "params".to_owned() } else { format!("params/{first}") };
    OpError::invalid(field, errors.to_string())
        .hint("fix the parameters listed; any you leave out keep the kind's defaults (part.describe shows the ranges)")
}

impl PartAdd {
    /// The argument to blame when the new part cannot be put in build order: the `layer` or the `parent` given,
    /// whichever causes it alone (`layer` if only the two together do), and the `kind` when neither was given,
    /// because then it is the kind's default layer against the kinds it requires.
    fn loop_culprit(&self, def: &CharacterDef, part: &PartDef, default_layer: i32) -> &'static str {
        let with = |layer: i32, parent: Option<PartId>| {
            let mut after = def.clone();
            after.parts.insert(self.id.clone(), PartDef { layer, parent, ..part.clone() });
            after
        };
        let mut candidates = Vec::new();
        if let Some(layer) = self.layer {
            candidates.push(("layer", with(layer, None)));
        }
        if let Some(parent) = &self.parent {
            candidates.push(("parent", with(default_layer, Some(parent.clone()))));
        }
        let fallback = match (self.layer, &self.parent) {
            (Some(_), _) => "layer",
            (None, Some(_)) => "parent",
            (None, None) => "kind",
        };
        tree::culprit(def, &candidates, fallback)
    }
}

impl Op for PartAdd {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.add",
            summary: "Add a part (hair, garment, accessory, body or head) of a registered kind to a character.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let kind = find_kind(self.kind.as_str()).ok_or_else(|| {
            OpError::invalid("kind", format!("there is no part kind `{}`", self.kind))
                .hint(format!("the registered kinds are: {}", list(list_kinds::registered().iter().map(|m| m.id))))
        })?;
        if let Some(existing) = def.parts.get(&self.id) {
            return Err(OpError::invalid(
                "id",
                format!("`{}` is already a part of `{}` (kind `{}`)", self.id, self.character, existing.kind),
            )
            .hint("choose another id, or copy the existing part with part.duplicate"));
        }
        if let Some(parent) = &self.parent {
            access::part(&def, &self.character, parent, "parent")?;
        }
        let default_layer = tree::default_layer((kind.meta)().category);
        let part = PartDef {
            kind: self.kind.clone(),
            parent: self.parent.clone(),
            enabled: true,
            layer: self.layer.unwrap_or(default_layer),
            attach: None,
            params: starting_params(kind, self.params.as_ref())?,
        };
        let mut after = def.clone();
        after.parts.insert(self.id.clone(), part.clone());
        let order = tree::check_edit(&def, &after, &format!("adding `{}`", self.id), |problem| match problem {
            Problem::Unmet { .. } => "kind",
            Problem::MissingParent { .. } => "parent",
            Problem::Blocked { .. } => self.loop_culprit(&def, &part, default_layer),
        })?;

        let stored = serde_json::to_value(&part)
            .map_err(|e| OpError::internal(format!("serialising the new part failed: {e}")))?;
        let mut changes = access::create(ctx, paths::part(&self.character, &self.id), stored);
        let stale_row = source::discard(ctx, &self.character, &self.id);
        let summary = json!({
            "character": self.character,
            "part": self.id,
            "kind": part.kind,
            "layer": part.layer,
            "parent": part.parent,
            "build_order": order,
            "note": tree::idle_reason(&after, &self.id),
            "stale_source_row_removed": stale_row.is_some().then_some(true),
        });
        changes.extend(stale_row);
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PartAdd);
