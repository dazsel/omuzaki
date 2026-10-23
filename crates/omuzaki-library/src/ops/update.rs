//! `library.update`: move an application to another version, or new parameters, keeping the person's edits.

use std::collections::BTreeMap;

use omuzaki_char_api::CharId;
use omuzaki_library_api::{CompatStatus, LibraryError, ResolvedItem, VersionReq};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::apply::{UpdateRequest, Work, library_error, update};
use crate::common::{facts, library, version};
use crate::records;

/// Whose value wins where both the person and the new version changed something.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Prefer {
    /// The person's edit stays (the default), and the path is reported as a conflict.
    #[default]
    Mine,
    /// The item's new value replaces the person's edit.
    Item,
}

/// Updates an application on a character to another version of its item, or re-applies it with new `params` (design
/// D10), as one undoable step. It is a three-way merge: what the old version wrote is the base, so a value only the
/// new version changes takes the new value, a value only you changed keeps yours, and one both changed keeps yours
/// and is listed as a conflict (`prefer: "item"` takes the item's). A part you removed stays removed. Without
/// `to_version` it takes the newest version in the same major. A dry-run is the diff preview: each changed path with
/// before, after and why.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "character": "mia", "application": "hairstyle" },
    { "character": "mia", "application": "hairstyle", "params": { "tail_length": 3.0 } }
]))]
pub struct LibraryUpdate {
    /// The character.
    pub character: CharId,
    /// The application key, as `library.lock` lists it.
    pub application: String,
    /// The version to move to.
    #[serde(default)]
    pub to_version: Option<String>,
    /// New parameter values; the others keep the values the application used.
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
    /// Whose value wins where both changed it.
    #[serde(default)]
    pub prefer: Prefer,
}

impl Op for LibraryUpdate {
    fn meta() -> OpMeta {
        OpMeta {
            name: "library.update",
            summary: "Update a library item on a character, keeping your own edits.",
            kind: OpKind::Edit,
            scope: Scope::EditLibrary,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let old = records::one(ctx.doc, &self.character, &self.application).ok_or_else(|| {
            OpError::not_found(format!("{} has no application `{}`", self.character, self.application))
                .hint("list them with library.lock")
        })?;
        let library = library(ctx)?;
        let req = match &self.to_version {
            Some(v) => VersionReq::Exact(version("to_version", v)?),
            None => VersionReq::parse(&format!("^{}", old.version)).unwrap_or(VersionReq::Exact(old.version)),
        };
        let item = library.resolve(&old.item, &req).map_err(|e| library_error("to_version", &e))?;
        let mut warnings = Vec::new();
        match facts(ctx, &self.character)?.status(&item.manifest.compat) {
            CompatStatus::Incompatible { reason } => {
                return Err(OpError::invalid(
                    "to_version",
                    format!("{}@{} doesn't fit {}: {reason}", old.item, item.manifest.version, self.character),
                )
                .hint("keep the version you have, or pin one that fits with `to_version`"));
            }
            CompatStatus::StyleMismatch { reason } => warnings.push(reason),
            CompatStatus::Compatible => {}
        }
        let resolve = |id: &_, req: &_| -> Result<ResolvedItem, LibraryError> { library.resolve(id, req) };
        let mut work = Work::of(ctx.doc);
        let request = UpdateRequest {
            character: &self.character,
            app: &self.application,
            old: &old,
            item: &item,
            params: self.params.clone(),
            prefer_item: self.prefer == Prefer::Item,
        };
        let updated = update(ctx, &resolve, &mut work, &request)?;
        let output = json!({
            "character": self.character,
            "application": self.application,
            "item": updated.record.item,
            "from_version": old.version.to_string(),
            "version": updated.record.version.to_string(),
            "params": updated.record.params,
            "changes": updated.changes,
            "conflicts": updated.conflicts,
            "dropped_params": updated.dropped,
            "licence": updated.record.rights.licence,
            "licence_changed": updated.record.rights.licence != old.rights.licence,
            "warnings": warnings,
        });
        Ok(Plan { patch: work.combined, output: Some(output), ..Plan::default() })
    }
}

register_op!(LibraryUpdate);
