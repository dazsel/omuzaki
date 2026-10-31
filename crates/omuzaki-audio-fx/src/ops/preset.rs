//! `audio.fx.kinds` and the preset ops: list, get, save, set a parameter, rename and delete a character's presets.

use omuzaki_audio_api::FxPreset;
use omuzaki_audio_api::graph::NodeSpec;
use omuzaki_audio_api::node::{node_kind, node_kinds};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Source, check_chain, find, load, store, summary};
use crate::presets::builtin;

/// List the voice-effect kinds this build has, with every parameter's unit, range and default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Kinds {}

impl Op for Kinds {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.fx.kinds",
            summary: "List the voice-effect kinds (EQ, gate, compressor, pitch, robot, radio, limiter, noise \
                      suppression, ...) with each parameter's unit, range and default, and each kind's latency.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let kinds: Vec<_> = node_kinds().into_iter().map(omuzaki_audio_api::NodeKind::info).collect();
        Ok(Plan::answer(json!({ "kinds": kinds })))
    }
}

/// List a character's voice presets and the built-in ones.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "character": "kit" }], "x-omuzaki-fixture" = "character"))]
pub struct PresetList {
    /// The character whose presets to list. Without one, only the built-in presets.
    #[serde(default)]
    pub character: Option<String>,
}

impl Op for PresetList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.fx.preset.list",
            summary: "List a character's voice-effect presets and the built-in ones (Clean, Deeper, Higher, Giant, \
                      Tiny, Robot, Radio, Telephone), with their effects and latency.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let own = match &self.character {
            Some(character) => load(ctx.doc, character)?.presets,
            None => Vec::new(),
        };
        Ok(Plan::answer(json!({
            "presets": own.iter().map(|p| summary(p, Source::Character)).collect::<Vec<_>>(),
            "builtin": builtin().iter().map(|p| summary(p, Source::Builtin)).collect::<Vec<_>>(),
        })))
    }
}

/// Get one preset whole: every node with its settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "preset": "Deeper" }], "x-omuzaki-fixture" = "character"))]
pub struct PresetGet {
    /// The character whose preset it is. Without one, a built-in preset.
    #[serde(default)]
    pub character: Option<String>,
    /// The preset's name. The character's own comes first; otherwise a built-in one of that name.
    pub preset: String,
}

impl Op for PresetGet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.fx.preset.get",
            summary: "Get one voice-effect preset whole: every effect in order with its parameters.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (preset, source) = find(ctx.doc, self.character.as_deref(), &self.preset)?;
        Ok(Plan::answer(json!({ "preset": preset, "source": source })))
    }
}

/// Save a voice preset to a character: a new chain, or a copy of another preset (a built-in one, say) to tune.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "character": "kit", "preset": "Kit low", "copy": "Deeper" },
    { "character": "kit", "preset": "Bright", "chain": [
        { "id": "gate", "kind": "fx.gate" },
        { "id": "pitch", "kind": "fx.pitch", "params": { "semitones": 3.0, "formant": 1.1 } }
    ] }
], "x-omuzaki-fixture" = "character"))]
pub struct PresetSave {
    /// The character to save it to.
    pub character: String,
    /// Its name; an existing preset of that name is replaced.
    pub preset: String,
    /// The effects in order. Give this or `copy`.
    #[serde(default)]
    pub chain: Option<Vec<NodeSpec>>,
    /// The name of a preset to copy (the character's own or a built-in one). Give this or `chain`.
    #[serde(default)]
    pub copy: Option<String>,
}

impl Op for PresetSave {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.fx.preset.save",
            summary: "Save a voice-effect preset to a character (its voice.toml): a chain of effects, or a copy of \
                      another preset to tune. Replaces a preset of the same name.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let name = self.preset.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(OpError::invalid("preset", "a preset's name is 1 to 64 characters"));
        }
        let mut file = load(ctx.doc, &self.character)?;
        let chain = match (&self.chain, &self.copy) {
            (Some(chain), None) => chain.clone(),
            (None, Some(copy)) => find(ctx.doc, Some(&self.character), copy)?.0.chain,
            _ => {
                return Err(OpError::invalid("chain", "give either `chain` or `copy`")
                    .hint("`copy` takes a preset's name; audio.fx.preset.list lists them"));
            }
        };
        check_chain(&chain)?;
        let preset = FxPreset { name: name.to_owned(), chain };
        let replaced = if let Some(existing) = file.presets.iter_mut().find(|p| p.name == name) {
            *existing = preset.clone();
            true
        } else {
            file.presets.push(preset.clone());
            false
        };
        let patch = store(ctx.doc, &self.character, &file)?;
        Ok(Plan {
            output: Some(json!({ "saved": summary(&preset, Source::Character), "replaced": replaced })),
            ..Plan::edit(patch)
        })
    }
}

/// Set one parameter of one effect in a character's preset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "character": "kit", "preset": "Kit low", "node": "pitch", "param": "semitones", "value": -3.0 }
], "x-omuzaki-fixture" = "character-voice"))]
pub struct PresetSetParam {
    /// The character.
    pub character: String,
    /// The preset (one of the character's own; copy a built-in one first with audio.fx.preset.save).
    pub preset: String,
    /// The effect's id in the chain.
    pub node: String,
    /// The parameter's name (audio.fx.kinds lists them).
    pub param: String,
    /// The value, within the parameter's range.
    pub value: f32,
}

impl Op for PresetSetParam {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.fx.preset.set_param",
            summary: "Set one parameter of one effect in a character's voice-effect preset.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut file = load(ctx.doc, &self.character)?;
        let Some(preset) = file.presets.iter_mut().find(|p| p.name == self.preset) else {
            let mut error = OpError::not_found(format!("`{}` has no preset `{}`", self.character, self.preset))
                .hint("a built-in preset is changed by saving a copy first: audio.fx.preset.save with `copy`");
            error.field = Some("preset".to_owned());
            return Err(error);
        };
        let ids = preset.chain.iter().map(|n| n.id.to_string()).collect::<Vec<_>>().join(", ");
        let Some(node) = preset.chain.iter_mut().find(|n| n.id.as_str() == self.node) else {
            let mut error = OpError::not_found(format!("the preset has no effect `{}`", self.node))
                .hint(format!("its effects: {ids}"));
            error.field = Some("node".to_owned());
            return Err(error);
        };
        let mut probe = node.clone();
        probe.params.insert(self.param.clone(), self.value);
        check_chain(std::slice::from_ref(&probe)).map_err(|mut e| {
            e.field = Some("param".to_owned());
            e
        })?;
        // A value back at the default leaves the file as small as a person would write it.
        let default = node_kind(&node.kind)
            .and_then(|k| k.info().params.iter().find(|p| p.name == self.param).map(|p| p.default));
        if default == Some(self.value) {
            node.params.remove(&self.param);
        } else {
            node.params.insert(self.param.clone(), self.value);
        }
        let after = summary(preset, Source::Character);
        let patch = store(ctx.doc, &self.character, &file)?;
        Ok(Plan { output: Some(json!({ "preset": after })), ..Plan::edit(patch) })
    }
}

/// Rename a character's preset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "character": "kit", "preset": "Kit low", "to": "Kit deep" }], "x-omuzaki-fixture" = "character-voice"))]
pub struct PresetRename {
    /// The character.
    pub character: String,
    /// The preset's current name.
    pub preset: String,
    /// Its new name, not taken by another of the character's presets.
    pub to: String,
}

impl Op for PresetRename {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.fx.preset.rename",
            summary: "Rename one of a character's voice-effect presets.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let to = self.to.trim();
        if to.is_empty() || to.len() > 64 {
            return Err(OpError::invalid("to", "a preset's name is 1 to 64 characters"));
        }
        let mut file = load(ctx.doc, &self.character)?;
        if to != self.preset && file.get(to).is_some() {
            let mut error = OpError::conflict(format!("`{}` already has a preset `{to}`", self.character))
                .hint("pick another name, or delete that preset first");
            error.field = Some("to".to_owned());
            return Err(error);
        }
        let Some(preset) = file.presets.iter_mut().find(|p| p.name == self.preset) else {
            let mut error = OpError::not_found(format!("`{}` has no preset `{}`", self.character, self.preset));
            error.field = Some("preset".to_owned());
            return Err(error);
        };
        to.clone_into(&mut preset.name);
        let patch = store(ctx.doc, &self.character, &file)?;
        Ok(Plan { output: Some(json!({ "renamed": self.preset, "to": to })), ..Plan::edit(patch) })
    }
}

/// Delete one of a character's presets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "character": "kit", "preset": "Kit low" }], "x-omuzaki-fixture" = "character-voice"))]
pub struct PresetDelete {
    /// The character.
    pub character: String,
    /// The preset to delete.
    pub preset: String,
}

impl Op for PresetDelete {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.fx.preset.delete",
            summary: "Delete one of a character's voice-effect presets (undo brings it back).",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut file = load(ctx.doc, &self.character)?;
        let before = file.presets.len();
        file.presets.retain(|p| p.name != self.preset);
        if file.presets.len() == before {
            let mut error = OpError::not_found(format!("`{}` has no preset `{}`", self.character, self.preset));
            error.field = Some("preset".to_owned());
            return Err(error);
        }
        let patch = store(ctx.doc, &self.character, &file)?;
        Ok(Plan { output: Some(json!({ "deleted": self.preset })), ..Plan::edit(patch) })
    }
}

register_op!(Kinds);
register_op!(PresetList);
register_op!(PresetGet);
register_op!(PresetSave);
register_op!(PresetSetParam);
register_op!(PresetRename);
register_op!(PresetDelete);
