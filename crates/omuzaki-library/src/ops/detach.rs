//! `library.detach`: drop the record of an application whose output is gone.

use omuzaki_char_api::{CharId, paths};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::apply::{JoinPath, Work};
use crate::records;

/// Removes an application record once nothing it wrote remains (design D8, D9): every value it set is gone or
/// changed. While any remains, the item's rights stay with the character, so the record stays and the refusal lists
/// the paths; remove or replace what the item made first.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("x-omuzaki-fixture" = "character-library", "examples" = [{ "character": "kit", "application": "colourway" }]))]
pub struct LibraryDetach {
    /// The character.
    pub character: CharId,
    /// The application key, as `library.lock` lists it.
    pub application: String,
}

impl Op for LibraryDetach {
    fn meta() -> OpMeta {
        OpMeta {
            name: "library.detach",
            summary: "Remove the record of a library item whose output is gone from a character.",
            kind: OpKind::Edit,
            scope: Scope::EditLibrary,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let record = records::one(ctx.doc, &self.character, &self.application).ok_or_else(|| {
            OpError::not_found(format!("{} has no application `{}`", self.character, self.application))
                .hint("list them with library.lock")
        })?;
        let node = paths::character(&self.character);
        let remaining: Vec<&String> = record
            .wrote
            .iter()
            .filter(|(path, value)| ctx.doc.get(&node.join_path(path)) == Some(*value))
            .map(|(path, _)| path)
            .collect();
        if !remaining.is_empty() {
            return Err(OpError::conflict(format!(
                "{}@{} still shapes {}: {}",
                record.item,
                record.version,
                self.character,
                remaining.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ")
            ))
            .hint("remove or replace what the item made first; its rights stay while any of it remains"));
        }
        let mut work = Work::of(ctx.doc);
        work.set(omuzaki_library_api::record_path(&self.character, &self.application), None)?;
        let output = json!({ "character": self.character, "application": self.application, "item": record.item });
        Ok(Plan { patch: work.combined, output: Some(output), ..Plan::default() })
    }
}

register_op!(LibraryDetach);
