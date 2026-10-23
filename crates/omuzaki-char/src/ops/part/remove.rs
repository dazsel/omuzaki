//! `part.remove`: delete a part, refusing while something still depends on it.

use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::source;
use crate::ops::character::access::{self, list};
use crate::ops::character::tree;

/// Removes a part from a character.
///
/// Refused while something depends on the part, and the error names what: its children (parts whose `parent` it is,
/// enabled or not), parts fixed to its anchors, other parts that need its kind built first (garments over a body)
/// when no other enabled part of that kind would remain, and variants that override its parameters. To keep a part
/// without building it, use `part.set_enabled` instead. The part's weight brushes and its source row (where it came
/// from and its rights, `licence.set_source`) go with it, and undo brings them back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"character": "kit", "part": "hair"}],
    "x-omuzaki-fixture" = "character"
))]
pub struct PartRemove {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part to remove.
    pub part: PartId,
}

/// The overrides of the character's variants that point into `part`, as `variant: path` pairs.
fn variant_overrides(ctx: &OpCtx<'_>, character: &CharId, part: &PartId) -> Vec<String> {
    let inside = DocPath::root().join("parts").join(part.as_str());
    let Some(Value::Object(variants)) = ctx.doc.get(&paths::variants(character)) else { return Vec::new() };
    let mut found = Vec::new();
    for (name, variant) in variants {
        let Some(Value::Object(overrides)) = variant.get("overrides") else { continue };
        for path in overrides.keys() {
            if DocPath::parse(path).is_ok_and(|p| p.starts_with(&inside)) {
                found.push(format!("{name}: {path}"));
            }
        }
    }
    found
}

/// Refuses to remove a part something is attached to, with a message that names what and how to proceed.
fn blocked(part: &PartId, what: &str, hint: &str) -> OpError {
    OpError::invalid("part", format!("can't remove `{part}`: {what}")).hint(hint)
}

impl Op for PartRemove {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.remove",
            summary: "Remove a part from a character; refused, naming them, while other parts or variants depend on it.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let removed = access::part(&def, &self.character, &self.part, "part")?;
        let children = tree::children(&def, &self.part);
        if !children.is_empty() {
            return Err(blocked(
                &self.part,
                &format!("it is the parent of {}", list(&children)),
                &format!("remove {} first, or move them to another parent with part.reorder", list(&children)),
            ));
        }
        let anchored = tree::anchored_to(&def, &self.part);
        if !anchored.is_empty() {
            return Err(blocked(
                &self.part,
                &format!("{} are fixed to its anchors", list(&anchored)),
                &format!("remove {} first, since they would have nothing to attach to", list(&anchored)),
            ));
        }
        let overrides = variant_overrides(ctx, &self.character, &self.part);
        if !overrides.is_empty() {
            return Err(blocked(
                &self.part,
                &format!("variants override its settings ({})", overrides.join(", ")),
                "remove those overrides from their variants first (variant.clear_override)",
            ));
        }

        let mut after = def.clone();
        after.parts.remove(&self.part);
        let brushes = after.weight_brushes.remove(&self.part);
        let order = tree::check_edit(&def, &after, &format!("removing `{}`", self.part), |_| "part")?;

        let part_path = paths::part(&self.character, &self.part);
        let mut changes = vec![access::change(ctx, part_path, None)];
        if brushes.is_some() {
            changes.push(access::change(ctx, paths::weight_brushes(&self.character).join(self.part.as_str()), None));
        }
        let row = source::discard(ctx, &self.character, &self.part);
        let removed_row = row.is_some();
        changes.extend(row);
        let summary = json!({
            "character": self.character,
            "removed": self.part,
            "kind": removed.kind,
            "source_row_removed": removed_row.then_some(true),
            "build_order": order,
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PartRemove);
