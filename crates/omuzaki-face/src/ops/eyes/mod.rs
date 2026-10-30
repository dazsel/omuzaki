//! The eye ops (#2469): `face.eyes.list_presets` lists the eye presets, `face.eyes.preset` gives a character one, and
//! `face.eyes.randomise` gives it a seeded random eye that always reads, keeping whatever groups of the design the caller
//! locks. Each writes the whole `face/eyes` section in one undoable edit, so nothing of an earlier design is left
//! behind; the presets and the priors are in [`crate::eye_presets`].

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::eye_presets::{DEFAULT_STYLE, EyeGroup, preset_eyes, presets, randomise};
use crate::ops_shared::check::check_edit;
use crate::ops_shared::edit::{changed_output, diff, effective, stored_face, to_json, working_copy};
use crate::ops_shared::path::template;

/// Lists the eye presets: each one's `id` and a line on what it looks like.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct ListPresets {}

impl Op for ListPresets {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.eyes.list_presets",
            summary: "List the eye presets (kit, sparkle, tareme, tsurime, sleepy, realistic, cat, doll, ...) with a line \
                      on each",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        let list: Vec<Value> = presets()
            .map_err(OpError::internal)?
            .into_iter()
            .map(|p| json!({ "id": p.id, "summary": p.summary }))
            .collect();
        Ok(Plan::answer(json!({ "presets": list })))
    }
}

register_op!(ListPresets);

/// Gives a character's eyes a preset (`face.eyes.list_presets` lists them): the whole eye design (opening, iris and its
/// detail, catchlights, eye white, lash line and strand lashes) becomes the preset's. By default the character keeps its
/// own iris colours (both eyes') and lash colour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "character": "kit", "preset": "sparkle" }, { "character": "kit", "preset": "cat", "keep_colours": false }],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetPreset {
    /// The character whose eyes change.
    pub character: CharId,
    /// The preset's id.
    pub preset: String,
    /// Keep the character's own iris and lash colours (the default); `false` takes the preset's colours too.
    #[serde(default = "yes")]
    pub keep_colours: bool,
}

fn yes() -> bool {
    true
}

impl Op for SetPreset {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.eyes.preset",
            summary: "Give a character's eyes a preset design (opening, iris detail, catchlights, eye white, lashes), \
                      keeping its own eye colours unless told not to, and return the changed paths",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut eyes = preset_eyes(&self.preset).map_err(|e| {
            OpError::invalid("preset", e).hint("`face.eyes.list_presets` lists the presets")
        })?;
        let stored = stored_face(ctx, &self.character)?;
        if self.keep_colours {
            let current = to_json(&effective(stored.as_ref())?.eyes)?;
            keep_colours(&mut eyes, &current);
        }
        let mut plan = write_eyes(&self.character, stored.as_ref(), eyes)?;
        if let Some(Value::Object(output)) = &mut plan.output {
            output.insert("preset".to_owned(), json!(self.preset));
        }
        Ok(plan)
    }
}

register_op!(SetPreset);

/// Gives a character a seeded random eye that always reads: it starts from a preset (`kit` unless `style` names
/// another) and draws every group of the design not in `keep` (`shape`, `colours`, `highlights`, `detail`, `lashes`,
/// `sclera`) from ranges that make a good eye. The same seed always gives the same eye; kept groups stay as the
/// character has them now, so "keep the colours, re-roll the rest" is one call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "character": "kit", "seed": 7 }, { "character": "kit", "seed": 12, "keep": ["colours", "shape"] }],
    "x-omuzaki-fixture" = "character"
))]
pub struct Randomise {
    /// The character whose eyes change.
    pub character: CharId,
    /// The seed: a whole number from 0 to 4294967295.
    pub seed: u32,
    /// The preset the groups that are re-rolled start from; `kit` when left out. Kept groups come from the character.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// The groups of the design to keep as they are.
    #[serde(default)]
    pub keep: Vec<EyeGroup>,
}

impl Op for Randomise {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.eyes.randomise",
            summary: "Give a character a seeded random eye that always reads, keeping the groups of the design asked for \
                      (shape, colours, highlights, detail, lashes, sclera), and return the changed paths",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let style = self.style.as_deref().unwrap_or(DEFAULT_STYLE);
        let base = preset_eyes(style)
            .map_err(|e| OpError::invalid("style", e).hint("`face.eyes.list_presets` lists the presets"))?;
        let stored = stored_face(ctx, &self.character)?;
        // Kept groups come from the character: re-roll the others over its own eyes, then lay the style's
        // values of the re-rolled groups under them (the draw sets every field it owns).
        let current = to_json(&effective(stored.as_ref())?.eyes)?;
        let start = if self.keep.is_empty() { base } else { blend_kept(&base, &current, &self.keep) };
        let eyes = randomise(&start, self.seed, &self.keep);
        let mut plan = write_eyes(&self.character, stored.as_ref(), eyes)?;
        if let Some(Value::Object(output)) = &mut plan.output {
            output.insert("seed".to_owned(), json!(self.seed));
        }
        Ok(plan)
    }
}

register_op!(Randomise);

/// The colour fields a preset leaves as the character has them.
const COLOUR_PATHS: [&[&str]; 6] =
    [&["iris", "top"], &["iris", "mid"], &["iris", "low"], &["iris", "ring"], &["iris", "pupil", "colour"], &["lash", "colour"]];

/// Copies the colour fields (and the right eye's own iris, or its absence) from `current` into `eyes`.
fn keep_colours(eyes: &mut Value, current: &Value) {
    for path in COLOUR_PATHS {
        copy(eyes, current, path);
    }
    copy_key(eyes, current, "right_iris");
}

/// The fields of each kept group, as paths under `eyes`.
fn group_paths(group: EyeGroup) -> &'static [&'static [&'static str]] {
    match group {
        EyeGroup::Shape => &[&["scale"], &["aperture"], &["iris", "width"], &["iris", "height"], &["iris", "drop"], &["iris", "inset"]],
        EyeGroup::Colours => &COLOUR_PATHS,
        EyeGroup::Highlights => &[&["highlights"]],
        EyeGroup::Detail => &[&["iris", "detail"], &["iris", "pupil", "shape"], &["iris", "pupil", "size"], &["seed"]],
        EyeGroup::Lashes => &[&["lash", "thickness"], &["lash", "flick"], &["lash", "lower"], &["lash", "inner_tick"], &["lash", "strands"]],
        EyeGroup::Sclera => &[&["sclera"], &["lid_shadow"]],
    }
}

/// `base` with the kept groups' fields taken from `current`.
fn blend_kept(base: &Value, current: &Value, keep: &[EyeGroup]) -> Value {
    let mut eyes = base.clone();
    for group in keep {
        for path in group_paths(*group) {
            copy(&mut eyes, current, path);
        }
        if *group == EyeGroup::Colours {
            copy_key(&mut eyes, current, "right_iris");
        }
    }
    eyes
}

/// Sets `path` in `to` to its value in `from`, when `from` has one.
fn copy(to: &mut Value, from: &Value, path: &[&str]) {
    let Some(value) = path.iter().try_fold(from, |node, key| node.get(*key)) else { return };
    let Some((last, parents)) = path.split_last() else { return };
    let mut node = to;
    for key in parents {
        let Some(next) = node.get_mut(*key) else { return };
        node = next;
    }
    if let Value::Object(map) = node {
        map.insert((*last).to_owned(), value.clone());
    }
}

/// Makes `to`'s top-level `key` what `from` has: the same value, or absent.
fn copy_key(to: &mut Value, from: &Value, key: &str) {
    if let Value::Object(map) = to {
        match from.get(key) {
            Some(value) => {
                map.insert(key.to_owned(), value.clone());
            }
            None => {
                map.remove(key);
            }
        }
    }
}

/// Replaces the character's whole eye section with `eyes`, checked like any face edit.
fn write_eyes(character: &CharId, stored: Option<&Value>, eyes: Value) -> Result<Plan, OpError> {
    let defaults = template()?;
    let mut working = working_copy(stored);
    if let Value::Object(face) = &mut working {
        face.insert("eyes".to_owned(), eyes);
    }
    check_edit(stored, &working, &defaults)?;
    let mut patch = Patch::new();
    diff(&paths::face(character), stored, Some(&working), &mut patch);
    let mut plan = Plan::edit(patch);
    plan.output = Some(changed_output(&plan.patch, None));
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_sets_a_nested_field_and_copy_key_removes_what_the_source_lacks() {
        let mut to = json!({ "iris": { "mid": "#000000", "width": 0.5 }, "right_iris": { "mid": "#ffffff" } });
        let from = json!({ "iris": { "mid": "#123456" } });
        copy(&mut to, &from, &["iris", "mid"]);
        copy_key(&mut to, &from, "right_iris");
        assert_eq!(to, json!({ "iris": { "mid": "#123456", "width": 0.5 } }));
    }
}
