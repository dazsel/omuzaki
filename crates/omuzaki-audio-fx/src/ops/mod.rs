//! The `audio.fx.*` ops (design section 6.2 and 7.1, #1265): the effect kinds, a character's voice presets, applying
//! one to the microphone, and rendering one offline so an agent can hear what it does in numbers.
//!
//! | Module | Ops |
//! |---|---|
//! | [`preset`] | `audio.fx.kinds`, `audio.fx.preset.list`, `.get`, `.save`, `.set_param`, `.rename`, `.delete` |
//! | [`apply`] | `audio.fx.apply`: a preset (or nothing) on the `mic` bus |
//! | [`render`] | `audio.fx.render` and `audio.fx.compare`: jobs that put a clip through presets and measure it |
//!
//! A character's presets live in its `voice.toml` (`characters/<id>/voice`, an [`omuzaki_audio_api::VoiceFile`]), so
//! a voice travels with its character. A name that is not one of the character's is looked up among the built-in
//! presets ([`crate::presets::builtin`]), which every op can read and none can change.

pub mod apply;
pub mod preset;
pub mod render;

use omuzaki_audio_api::graph::NodeSpec;
use omuzaki_audio_api::{FxPreset, VoiceFile};
use omuzaki_core::id::Slug;
use omuzaki_doc_api::{Change, DocPath, DocRead, Patch, Value};
use omuzaki_ops_api::OpError;

use crate::chain::{ChainError, check};
use crate::presets::builtin;

/// `characters/<id>`.
fn character_path(character: &str) -> DocPath {
    DocPath::root().join("characters").join(character)
}

/// `characters/<id>/voice`: the character's `voice.toml`.
#[must_use]
pub fn voice_path(character: &str) -> DocPath {
    character_path(character).join("voice")
}

/// Refuses a character id that is not a slug or names no character in the document.
fn require_character(doc: &dyn DocRead, character: &str) -> Result<(), OpError> {
    let invalid = || {
        OpError::invalid("character", format!("`{character}` is not a character id"))
            .hint("character ids are lowercase slugs such as `mira`; list them with project.get")
    };
    Slug::new(character).map_err(|_| invalid())?;
    if doc.get(&character_path(character)).is_none() {
        let mut error = OpError::not_found(format!("there is no character `{character}`"))
            .hint("create one with character.create, or check the id with project.get");
        error.field = Some("character".to_owned());
        return Err(error);
    }
    Ok(())
}

/// The character's voice file; empty when it has none yet.
///
/// # Errors
/// `Invalid` when the stored file is not in the voice-file format.
fn load(doc: &dyn DocRead, character: &str) -> Result<VoiceFile, OpError> {
    require_character(doc, character)?;
    doc.get(&voice_path(character)).map_or_else(
        || Ok(VoiceFile::default()),
        |value| {
            serde_json::from_value(value.clone()).map_err(|e| {
                OpError::invalid("character", format!("`{character}`'s voice.toml is not a voice file: {e}"))
                    .hint("repair or delete characters/<id>/voice.toml, or undo the edit that broke it")
            })
        },
    )
}

/// The patch that writes `file` as the character's voice file.
fn store(doc: &dyn DocRead, character: &str, file: &VoiceFile) -> Result<Patch, OpError> {
    let path = voice_path(character);
    let after = serde_json::to_value(file).map_err(|e| OpError::internal(format!("the voice file: {e}")))?;
    Ok(Patch { changes: vec![Change { before: doc.get(&path).cloned(), path, after: Some(after) }] })
}

/// Where a preset was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The character's `voice.toml`.
    Character,
    /// The built-in presets.
    Builtin,
}

/// The preset `name`: the character's own if it has one by that name, else the built-in one.
fn find(doc: &dyn DocRead, character: Option<&str>, name: &str) -> Result<(FxPreset, Source), OpError> {
    if let Some(character) = character
        && let Some(own) = load(doc, character)?.get(name)
    {
        return Ok((own.clone(), Source::Character));
    }
    if let Some(builtin) = builtin().into_iter().find(|p| p.name == name) {
        return Ok((builtin, Source::Builtin));
    }
    let mut known: Vec<String> = builtin().into_iter().map(|p| p.name).collect();
    if let Some(character) = character {
        known.extend(load(doc, character)?.presets.into_iter().map(|p| p.name));
    }
    let mut error =
        OpError::not_found(format!("there is no preset `{name}`")).hint(format!("presets: {}", known.join(", ")));
    error.field = Some("preset".to_owned());
    Err(error)
}

/// Refuses a chain the registered kinds can't build.
fn check_chain(chain: &[NodeSpec]) -> Result<(), OpError> {
    check(chain).map_err(|e| {
        let hint = match e {
            ChainError::UnknownKind { .. } => "list the effect kinds with audio.fx.kinds",
            _ => "audio.fx.kinds lists each kind's parameters and their ranges",
        };
        OpError::invalid("chain", e.to_string()).hint(hint)
    })
}

/// A preset as a list shows it: its name, where it is, its nodes and its latency.
fn summary(preset: &FxPreset, source: Source) -> Value {
    let latency: u32 = preset
        .chain
        .iter()
        .filter_map(|n| omuzaki_audio_api::node::node_kind(&n.kind).map(|k| k.info().latency.max))
        .sum();
    serde_json::json!({
        "name": preset.name,
        "source": source,
        "nodes": preset.chain.iter().map(|n| serde_json::json!({ "id": n.id, "kind": n.kind, "enabled": n.enabled })).collect::<Vec<_>>(),
        "latency_ms": f64::from(latency) / 48.0,
    })
}
