//! Stage presets (ST-01, design D9, #1273): the set a scene stands in (its backdrop, its ground and the props that make
//! the room) as one item, applied with `stage.set_stage_preset` and listed by `stage.stage_presets`.
//!
//! Each preset is one TOML file under `library/stages/`. Applying one replaces the backdrop, the ground and every prop
//! an earlier preset placed (ids starting `set-`) in one undo step; the lighting, the cast, the cameras and the props
//! people added themselves stay. A preset names the light rig it pairs with, and leaves applying it to the caller.

use std::collections::BTreeMap;

use omuzaki_core::text::nearest;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::{Backdrop, Ground, MAX_PROPS, PropDef, PropId, SceneId, find_prop_kind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::edit::SceneEdit;

/// The prefix of the ids of the props a preset places: what the next preset replaces.
pub const SET_PREFIX: &str = "set-";

/// The presets shipped with the crate, by name. Each file is parsed on use, so a broken file fails the test that loads
/// it.
static LIBRARY: [(&str, &str); 6] = [
    ("study", include_str!("../../../library/stages/study.toml")),
    ("bedroom", include_str!("../../../library/stages/bedroom.toml")),
    ("cafe", include_str!("../../../library/stages/cafe.toml")),
    ("night-library", include_str!("../../../library/stages/night-library.toml")),
    ("desk", include_str!("../../../library/stages/desk.toml")),
    ("green-screen", include_str!("../../../library/stages/green-screen.toml")),
];

/// One stage preset as its file holds it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagePreset {
    /// The name people read (`Study`).
    pub title: String,
    /// What it looks like and which rig it pairs with.
    pub summary: String,
    /// The light rig it is designed under, for `stage.set_light_rig`.
    #[serde(default)]
    pub light_rig: Option<String>,
    /// Words to find it by.
    #[serde(default)]
    pub tags: Vec<String>,
    /// What shows behind the room.
    pub backdrop: Backdrop,
    /// The ground: none for a room with its own floor.
    pub ground: Ground,
    /// The props, by name; each is placed as `set-<name>`.
    #[serde(default)]
    pub props: BTreeMap<String, PropDef>,
}

impl StagePreset {
    /// The names of the shipped presets, in the library's order.
    pub fn names() -> impl Iterator<Item = &'static str> {
        LIBRARY.iter().map(|(name, _)| *name)
    }

    /// The preset `name`.
    ///
    /// # Errors
    /// A message for a name the library lacks or a file that doesn't read.
    pub fn load(name: &str) -> Result<Self, String> {
        let (_, text) =
            LIBRARY.iter().find(|(n, _)| *n == name).ok_or_else(|| format!("there is no stage preset '{name}'"))?;
        toml::from_str(text).map_err(|e| format!("the stage preset '{name}' doesn't read: {e}"))
    }

    /// The props as the scene stores them: each one's id with the `set-` prefix, its params checked by its kind with
    /// every default filled in.
    ///
    /// # Errors
    /// A message naming a prop whose kind isn't registered or whose params its kind refuses, or an id that isn't one.
    pub fn placed(&self) -> Result<Vec<(PropId, PropDef)>, String> {
        self.props
            .iter()
            .map(|(name, prop)| {
                let id = PropId::new(format!("{SET_PREFIX}{name}")).map_err(|e| format!("prop '{name}': {e}"))?;
                let kind = find_prop_kind(prop.kind.as_str())
                    .ok_or_else(|| format!("prop '{name}': no prop kind '{}'", prop.kind))?;
                let mut prop = prop.clone();
                prop.params = kind.check(&prop.params).map_err(|e| format!("prop '{name}': {e}"))?;
                Ok((id, prop))
            })
            .collect()
    }
}

/// Puts a scene in a stage preset: its backdrop, ground and props.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"preset": "study"}, {"scene": "night_stream", "preset": "night-library"}, {"preset": "green-screen"}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetStagePreset {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The preset: `study`, `bedroom`, `cafe`, `night-library`, `desk` or `green-screen` (`stage.stage_presets`).
    pub preset: String,
}

impl Op for SetStagePreset {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_stage_preset",
            summary: "Put a scene in a stage preset (a furnished study, bedroom, café, night library, streamer desk, or a \
                      green screen): its backdrop, ground and props, replacing the last preset's. Lighting, cast, \
                      cameras and props you added stay; the output names the light rig it pairs with.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let names: Vec<&str> = StagePreset::names().collect();
        if !names.contains(&self.preset.as_str()) {
            let error = OpError::invalid("preset", format!("there is no stage preset '{}'", self.preset));
            return Err(match nearest(&self.preset, names.iter().copied()) {
                Some(near) => error.hint(format!("did you mean '{near}'? the presets: {}", names.join(", "))),
                None => error.hint(format!("the presets: {}", names.join(", "))),
            });
        }
        let internal = |e: String| OpError::internal(format!("the library stage preset '{}': {e}", self.preset));
        let preset = StagePreset::load(&self.preset).map_err(internal)?;
        let placed = preset.placed().map_err(internal)?;
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let before: Vec<String> =
            edit.def.props.keys().filter(|id| id.as_str().starts_with(SET_PREFIX)).map(ToString::to_string).collect();
        edit.def.props.retain(|id, _| !id.as_str().starts_with(SET_PREFIX));
        if edit.def.props.len() + placed.len() > MAX_PROPS {
            return Err(OpError::invalid(
                "preset",
                format!(
                    "the scene's {} own props and the preset's {} are more than the {MAX_PROPS} a scene can have",
                    edit.def.props.len(),
                    placed.len()
                ),
            )
            .hint("remove some with stage.remove_prop"));
        }
        let added: Vec<&str> = placed.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>();
        let output = json!({
            "scene": edit.id.as_str(),
            "preset": self.preset,
            "title": preset.title,
            "light_rig": preset.light_rig,
            "props": added,
            "replaced": before,
        });
        edit.def.scene.backdrop = preset.backdrop;
        edit.def.scene.ground = preset.ground;
        edit.def.props.extend(placed);
        edit.finish(ctx, output)
    }
}

register_op!(SetStagePreset);

/// Lists the stage presets.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct StagePresets {}

impl Op for StagePresets {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.stage_presets",
            summary: "List the stage presets a scene can be put in (stage.set_stage_preset): name, title, look, tags, \
                      the light rig each pairs with, and its props.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut presets = Vec::new();
        for name in StagePreset::names() {
            let preset = StagePreset::load(name).map_err(OpError::internal)?;
            let props: Vec<serde_json::Value> =
                preset.props.iter().map(|(n, p)| json!({ "name": n, "kind": p.kind.as_str() })).collect();
            presets.push(json!({
                "name": name,
                "title": preset.title,
                "summary": preset.summary,
                "tags": preset.tags,
                "light_rig": preset.light_rig,
                "props": props,
            }));
        }
        Ok(Plan::answer(json!({ "presets": presets })))
    }
}

register_op!(StagePresets);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_reads_and_its_props_check() {
        crate::link();
        for name in StagePreset::names() {
            let preset = StagePreset::load(name).unwrap_or_else(|e| panic!("{e}"));
            let placed = preset.placed().unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(placed.len() <= MAX_PROPS, "{name}");
            if let Some(rig) = &preset.light_rig {
                assert!(crate::recipe::LightRig::names().any(|r| r == rig), "{name}: no light rig '{rig}'");
            }
        }
    }
}
