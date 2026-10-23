//! `library.install`: add an item to the person's own library.

use std::io::Read as _;

use omuzaki_library_api::ItemManifest;
use omuzaki_ops_api::{Effect, FsMode, Op, OpCall, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::common::{failed, host};

/// Adds an item to your library, across projects (design D9, D11): from a manifest file (`from`) or its text
/// (`manifest`). The manifest is checked, its publisher verified, and it is refused when the same publisher already
/// holds that version with other content. Installing is a person's decision: an agent's call asks the person first.
/// Undo uninstalls it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "from": "downloads/ribbon-bow.toml" }]))]
pub struct LibraryInstall {
    /// A manifest file (`.toml`), absolute or relative to the project.
    #[serde(default)]
    pub from: Option<String>,
    /// The manifest's text, instead of a file.
    #[serde(default)]
    pub manifest: Option<String>,
}

pub(crate) fn call(op: &str, args: serde_json::Value) -> OpCall {
    OpCall { op: op.to_owned(), args }
}

impl Op for LibraryInstall {
    fn meta() -> OpMeta {
        OpMeta {
            name: "library.install",
            summary: "Install a library item into your own library.",
            kind: OpKind::Edit,
            scope: Scope::EditLibrary,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let text = match (&self.from, &self.manifest) {
            (Some(path), None) => {
                let mut text = String::new();
                ctx.fs_access(path, FsMode::Read)?
                    .open_read()?
                    .read_to_string(&mut text)
                    .map_err(|e| OpError::invalid("from", e.to_string()))?;
                text
            }
            (None, Some(text)) => text.clone(),
            _ => return Err(OpError::invalid("from", "give a manifest file in `from` or its text in `manifest`")),
        };
        let manifest = ItemManifest::parse(&text).map_err(|e| OpError::invalid("manifest", e.to_string()))?;
        let id = manifest.item_id().map_err(|e| OpError::invalid("manifest", e.to_string()))?;
        let version = manifest.item_version().map_err(|e| OpError::invalid("manifest", e.to_string()))?;
        let problems = manifest.check();
        if !problems.is_empty() {
            let all: Vec<String> = problems.iter().map(ToString::to_string).collect();
            return Err(OpError::invalid("manifest", all.join("; ")));
        }
        ctx.require_person(format!("install {id}@{version} ({}) into your library", manifest.rights.licence))?;
        let library = host(ctx)?;
        let new = if ctx.dry_run { true } else { library.install(&manifest).map_err(|e| failed(&e))? };
        let output = json!({
            "item": id,
            "version": version.to_string(),
            "hash": manifest.hash().to_string(),
            "licence": manifest.rights.licence,
            "new": new,
        });
        let effect = Effect {
            summary: format!("installed {id}@{version} in your library"),
            undo: call("library.uninstall", json!({ "item": id, "version": version.to_string() })),
            redo: call("library.install", json!({ "manifest": text })),
        };
        Ok(Plan { output: Some(output), effect: new.then_some(effect), ..Plan::default() })
    }
}

register_op!(LibraryInstall);
