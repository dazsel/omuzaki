//! `variant.preview`: the variant resolved and compared with the base character. A query; writes nothing.

use std::collections::BTreeSet;

use omuzaki_char_api::{CharId, VariantId, resolve_variant};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};

use super::access;
use crate::ops::character::access as character;

/// Resolves a variant (its `extends` chain and its overrides) and lists every field it changes against the base
/// character: the path, and the value before and after. A field the base doesn't have has only an `after`, and one the
/// variant removes has only a `before`. Builds nothing and writes nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "variant": "close-up"}
    ],
    "x-omuzaki-fixture" = "character-variant"
))]
pub struct VariantPreview {
    /// The character the variant belongs to.
    pub character: CharId,
    /// The variant to resolve.
    pub variant: VariantId,
}

/// Records, under `path`, what differs between `before` and `after` (either may be absent). Objects are compared key
/// by key, so the entries name the fields; anything else (a number, a list, a string) is compared whole.
fn walk(path: &DocPath, before: Option<&Value>, after: Option<&Value>, out: &mut Vec<Value>) {
    // The character format writes no null: an absent option is no key. A null here, from a file written before that,
    // is absent too, so it is not reported as a change to `null`.
    let before = before.filter(|v| !v.is_null());
    let after = after.filter(|v| !v.is_null());
    if let (Some(Value::Object(b)), Some(Value::Object(a))) = (before, after) {
        let keys: BTreeSet<&str> = b.keys().chain(a.keys()).map(String::as_str).collect();
        for key in keys {
            walk(&path.join(key), b.get(key), a.get(key), out);
        }
        return;
    }
    if before == after {
        return;
    }
    let mut entry = Map::new();
    entry.insert("path".to_owned(), json!(path.to_string()));
    if let Some(before) = before {
        entry.insert("before".to_owned(), before.clone());
    }
    if let Some(after) = after {
        entry.insert("after".to_owned(), after.clone());
    }
    out.push(Value::Object(entry));
}

impl Op for VariantPreview {
    fn meta() -> OpMeta {
        OpMeta {
            name: "variant.preview",
            summary: "Resolve a variant and list each field it changes against the base character, with before and after. Writes nothing.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = character::load(ctx, &self.character)?;
        let variant = access::load(ctx, &self.character, &self.variant)?;
        let set = access::with_ancestors(ctx, &self.character, &self.variant, variant)?;
        let resolved = resolve_variant(&def, &set, &self.variant).map_err(|e| access::refusal(&e, "variant"))?;

        let base = serde_json::to_value(&def)
            .map_err(|e| OpError::internal(format!("serialising the base character failed: {e}")))?;
        let variant_tree = serde_json::to_value(&resolved)
            .map_err(|e| OpError::internal(format!("serialising the resolved variant failed: {e}")))?;
        let mut changes = Vec::new();
        walk(&DocPath::root(), Some(&base), Some(&variant_tree), &mut changes);

        let output = json!({
            "character": self.character,
            "variant": self.variant,
            "count": changes.len(),
            "changes": changes,
        });
        Ok(Plan::answer(output))
    }
}

register_op!(VariantPreview);
