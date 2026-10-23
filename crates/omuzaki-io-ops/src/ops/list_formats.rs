//! `io.list_formats`: which model formats this build can read and write, and which motion formats it can write.
//!
//! The answer is whatever importers and exporters the linker collected (`omuzaki-io-api` registers them by format), so a
//! build with the VRM crate lists `vrm1` and one without it doesn't. Agents call this before `io.import` and
//! `io.export` to learn the `format` values they may pass, and before `io.export_animation` for its own (`animations`).

use omuzaki_io_api::{FormatInfo, FormatSupport};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Lists the registered formats. Takes no arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListFormats {}

/// One format and what this build can do with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FormatEntry {
    /// The id to pass as `format`: `glb`, `vrm1`.
    pub id: String,
    /// The human name, "VRM 1.0".
    pub name: String,
    /// File extensions without the dot, most common first.
    pub extensions: Vec<String>,
    /// The media type.
    pub media_type: String,
    /// An importer is registered: `io.import` and `io.inspect` can read it.
    pub import: bool,
    /// An exporter is registered: `io.export` can write it.
    pub export: bool,
}

impl From<FormatSupport> for FormatEntry {
    fn from(support: FormatSupport) -> Self {
        let FormatSupport { info, import, export } = support;
        Self {
            id: info.id.to_owned(),
            name: info.name.to_owned(),
            extensions: info.extensions.iter().map(|extension| (*extension).to_owned()).collect(),
            media_type: info.media_type.to_owned(),
            import,
            export,
        }
    }
}

/// One motion format `io.export_animation` can write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AnimationFormatEntry {
    /// The id to pass as `format`: `vrma`.
    pub id: String,
    /// The human name, "VRM Animation".
    pub name: String,
    /// File extensions without the dot, most common first.
    pub extensions: Vec<String>,
    /// The media type.
    pub media_type: String,
}

impl From<FormatInfo> for AnimationFormatEntry {
    fn from(info: FormatInfo) -> Self {
        Self {
            id: info.id.to_owned(),
            name: info.name.to_owned(),
            extensions: info.extensions.iter().map(|extension| (*extension).to_owned()).collect(),
            media_type: info.media_type.to_owned(),
        }
    }
}

/// What `io.list_formats` returns.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Formats {
    /// Every model format with an importer, an exporter or both, sorted by id.
    pub formats: Vec<FormatEntry>,
    /// Every motion format with an exporter (`io.export_animation`), sorted by id.
    #[serde(default)]
    pub animations: Vec<AnimationFormatEntry>,
}

impl Formats {
    /// The formats registered in this build.
    pub fn registered() -> Self {
        Self {
            formats: omuzaki_io_api::formats().into_iter().map(FormatEntry::from).collect(),
            animations: omuzaki_io_api::animation_formats().into_iter().map(AnimationFormatEntry::from).collect(),
        }
    }
}

impl Op for ListFormats {
    fn meta() -> OpMeta {
        OpMeta {
            name: "io.list_formats",
            summary: "List the model formats this build can import and export, and the motion formats it can export.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let formats = serde_json::to_value(Formats::registered())
            .map_err(|e| OpError::internal(format!("serialising the format list failed: {e}")))?;
        Ok(Plan::answer(formats))
    }
}

register_op!(ListFormats);
