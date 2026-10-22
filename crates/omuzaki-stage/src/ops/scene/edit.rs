//! What every scene edit shares (design D8): find the scene (the one named, else the one the project opens in), read
//! it, change the definition, check it with the one validator, and write back only the leaves that changed, so a
//! one-field edit is a one-path patch with an exact inverse.

use std::collections::BTreeSet;

use omuzaki_char_api::paths as char_paths;
use omuzaki_core::text::nearest;
use omuzaki_doc_api::{Change, DocPath, DocRead, Patch, Value};
use omuzaki_ops_api::{OpCtx, OpError, Plan};
use omuzaki_stage_api::{SceneDef, SceneError, SceneId, paths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::scene::default::opening_scene;

/// A scene being edited: its id, the node as stored, and the definition the op changes.
pub(crate) struct SceneEdit {
    /// The scene.
    pub id: SceneId,
    /// `scenes/<id>` as stored.
    before: Value,
    /// The definition, to change.
    pub def: SceneDef,
}

/// The scene `scene` names, else the one the project opens in.
///
/// # Errors
/// `NotFound` for a scene the project lacks (with the nearest id), or for no scene at all when none is named.
pub(crate) fn scene_id(doc: &dyn DocRead, scene: Option<&SceneId>) -> Result<SceneId, OpError> {
    let Some(id) = scene.cloned().or_else(|| opening_scene(doc)) else {
        return Err(OpError::not_found("the project has no scene yet").hint("stage.create_scene makes one"));
    };
    if doc.get(&paths::scene(&id)).is_none() {
        let ids = scene_ids(doc);
        let mut error = OpError::not_found(format!("there is no scene '{id}'"));
        error = match nearest(id.as_str(), ids.iter().map(String::as_str)) {
            Some(near) => error.hint(format!("did you mean '{near}'? stage.list_scenes lists the project's scenes")),
            None => error.hint("stage.list_scenes lists the project's scenes"),
        };
        return Err(error);
    }
    Ok(id)
}

/// The ids of the project's scenes, in order.
pub(crate) fn scene_ids(doc: &dyn DocRead) -> Vec<String> {
    doc.get(&DocPath::root().join("scenes")).and_then(Value::as_object).map(|t| t.keys().cloned().collect()).unwrap_or_default()
}

impl SceneEdit {
    /// Opens the scene `scene` names (else the project's opening scene) for editing.
    ///
    /// # Errors
    /// What [`scene_id`] refuses; `Invalid` for a scene that doesn't read.
    pub(crate) fn open(doc: &dyn DocRead, scene: Option<&SceneId>) -> Result<Self, OpError> {
        let id = scene_id(doc, scene)?;
        let before = doc.get(&paths::scene(&id)).cloned().unwrap_or(Value::Null);
        let def = SceneDef::from_node(&before)
            .map_err(|e| OpError::invalid("scene", format!("scene '{id}' doesn't read: {e}")))?;
        Ok(Self { id, before, def })
    }

    /// Checks the changed definition and returns the plan that writes it: only the leaves that differ, with `output`.
    /// A definition equal to the stored one is an empty patch.
    ///
    /// # Errors
    /// `Invalid` naming the field the validator refuses.
    pub(crate) fn finish(self, ctx: &OpCtx<'_>, output: Value) -> Result<Plan, OpError> {
        let doc = ctx.doc;
        let has_character = |id: &omuzaki_char_api::CharId| doc.get(&char_paths::character(id)).is_some();
        self.def.check_with(&has_character).map_err(refused)?;
        let mut after = self.before.clone();
        self.def.to_node(&mut after).map_err(|e| OpError::internal(format!("writing scene '{}': {e}", self.id)))?;
        let mut patch = Patch::new();
        diff(&paths::scene(&self.id), Some(&self.before), Some(&after), &mut patch);
        Ok(Plan { patch, output: Some(output), ..Plan::default() })
    }
}

/// The validator's refusal as an op error naming the field.
pub(crate) fn refused(error: SceneError) -> OpError {
    let message = error.to_string();
    OpError::invalid(error.path, message)
}

/// The changes that turn `before` into `after` at `path`, down to the leaves. Objects are compared key by key and lists
/// of equal length index by index; anything else, including a key or a list that is new, is one change carrying the
/// whole value.
pub(crate) fn diff(path: &DocPath, before: Option<&Value>, after: Option<&Value>, into: &mut Patch) {
    if before == after {
        return;
    }
    match (before, after) {
        (Some(Value::Object(old)), Some(Value::Object(new))) => {
            let keys: BTreeSet<&str> = old.keys().chain(new.keys()).map(String::as_str).collect();
            for key in keys {
                diff(&path.join(key), old.get(key), new.get(key), into);
            }
        }
        (Some(Value::Array(old)), Some(Value::Array(new))) if old.len() == new.len() => {
            for (i, (was, now)) in old.iter().zip(new).enumerate() {
                diff(&path.join(i.to_string()), Some(was), Some(now), into);
            }
        }
        _ => into.changes.push(Change { path: path.clone(), before: before.cloned(), after: after.cloned() }),
    }
}

/// The paths a patch changes, for an op's output.
pub(crate) fn changed(patch: &Patch) -> Vec<String> {
    patch.changes.iter().map(|c| c.path.to_string()).collect()
}

/// `off` in an argument that either sets something or removes it (`"skin_floor": "off"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Off {
    /// Remove it.
    Off,
}

/// A value, or `off` to remove it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OrOff<T> {
    /// Remove it.
    Off(Off),
    /// Set it.
    Set(T),
}

impl<T> OrOff<T> {
    /// The value to store: `None` for `off`.
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Off(Off::Off) => None,
            Self::Set(value) => Some(value),
        }
    }
}

/// The error for an id the scene lacks among `known`, with the nearest one.
pub(crate) fn unknown(field: &str, what: &str, id: &str, known: impl IntoIterator<Item = String>) -> OpError {
    let known: Vec<String> = known.into_iter().collect();
    let mut error = OpError::not_found(format!("the scene has no {what} '{id}'"));
    error.field = Some(field.to_owned());
    match nearest(id, known.iter().map(String::as_str)) {
        Some(near) => error.hint(format!("did you mean '{near}'? the scene has: {}", known.join(", "))),
        None => error.hint(format!("the scene has no {what}s yet")),
    }
}

/// The first id free among `base`, `base-2`, `base-3`...
pub(crate) fn free_id(base: &str, taken: &dyn Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_owned();
    }
    // A scene holds at most a few dozen of anything, so the search ends long before the range does.
    (2..=u32::MAX).map(|n| format!("{base}-{n}")).find(|id| !taken(id)).unwrap_or_else(|| base.to_owned())
}
