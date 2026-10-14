//! `part.duplicate`: copy a part as a disabled variant, to try versions side by side (CB-91).

use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::source;
use crate::ops::character::access;

/// Copies a part under a new id, as a disabled variant (hair A / B / C side by side).
///
/// The copy has the same kind, parent, layer, attachment, parameters and weight brushes, and is **disabled**, so
/// duplicating changes nothing about how the character looks. Children are not copied. The copy has the same source
/// row (where the part came from and its rights, `licence.set_source`), since it is the same asset, so a character stays
/// as licensable as it was. Edit the copy with the part ops, then enable it with `part.set_enabled` (and disable the
/// original) to compare them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartDuplicate {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part to copy.
    pub part: PartId,
    /// The copy's id: lowercase letters, digits, `-` and `_`. Must not be used by another part of this character.
    pub new_id: PartId,
}

impl Op for PartDuplicate {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.duplicate",
            summary: "Copy a part under a new id, disabled, to keep versions (hair A / B / C) side by side.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        access::part(&def, &self.character, &self.part, "part")?;
        if let Some(existing) = def.parts.get(&self.new_id) {
            return Err(OpError::invalid(
                "new_id",
                format!("`{}` is already a part of `{}` (kind `{}`)", self.new_id, self.character, existing.kind),
            )
            .hint("choose an id no part uses yet, for example by adding a suffix (`-b`)"));
        }

        // Copy what the document stores rather than the typed part, so nothing is normalised away or invented.
        let source = paths::part(&self.character, &self.part);
        let Some(Value::Object(stored)) = ctx.doc.get(&source) else {
            return Err(OpError::internal(format!("part `{}` is in the character but not in the document", self.part)));
        };
        let mut copy = stored.clone();
        copy.insert("enabled".to_owned(), Value::Bool(false));
        let mut changes = access::create(ctx, paths::part(&self.character, &self.new_id), Value::Object(copy));
        if let Some(brushes) = ctx.doc.get(&paths::weight_brushes(&self.character).join(self.part.as_str())) {
            let path = paths::weight_brushes(&self.character).join(self.new_id.as_str());
            changes.extend(access::create(ctx, path, brushes.clone()));
        }
        let row = source::carry(ctx, &self.character, &self.part, &self.new_id);
        let copied_row = row.as_ref().is_some_and(|change| change.after.is_some());
        changes.extend(row);
        let summary = json!({
            "character": self.character,
            "part": self.new_id,
            "copied_from": self.part,
            "enabled": false,
            "source_row_copied": copied_row.then_some(true),
            "note": format!(
                "`{}` is disabled: enable it with part.set_enabled (and disable `{}`) to compare them",
                self.new_id, self.part
            ),
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PartDuplicate);
