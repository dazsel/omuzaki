//! `variant.set_override`: sets one override of a variant, checked by resolving the variant before anything is written.

use omuzaki_char_api::{CharId, CharacterDef, PartId, VariantId, find_kind, resolve_variant};
use omuzaki_core::text;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access;
use crate::ops::character::access as character;
use crate::ops::character::access::list;

/// Sets one override of a variant: the value at `path` in the variant, which replaces what the base character (and any
/// variant it extends) has there. An override already at `path` is replaced.
///
/// `path` is a document path from the character root with `/` between names, such as `parts/head/params/outline/chin`
/// or `face/layout/mouth_y`. The variant is resolved on a copy with the override in place, so a path that names no
/// part, a field the character doesn't have, or a value of the wrong type is refused with the variant and the path
/// named, and nothing is written.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "variant": "close-up", "path": "face/layout/mouth_y", "value": -0.12}
    ],
    "x-omuzaki-fixture" = "character-variant"
))]
pub struct VariantSetOverride {
    /// The character the variant belongs to.
    pub character: CharId,
    /// The variant to change.
    pub variant: VariantId,
    /// The document path to override, from the character root, with `/` between names.
    pub path: String,
    /// The new value, in the shape the field stores it (a number, a table, a list or a string). Never `null`: a
    /// project file can't hold one, so drop an override with `variant.clear_override` instead.
    pub value: Value,
}

/// The most edits a misspelt name can be from the one it means and still get a "did you mean".
const MAX_TYPO: usize = 2;

/// The name in `candidates` nearest to `name`, when it is within [`MAX_TYPO`] edits. Ties keep the first candidate,
/// so the same typo always gets the same suggestion.
fn nearest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let best = text::nearest(name, candidates)?;
    (text::edit_distance(name, best) <= MAX_TYPO).then_some(best)
}

/// Puts a "did you mean" in front of an error's hint, when there is a suggestion.
fn with_suggestion(mut error: OpError, suggestion: Option<String>) -> OpError {
    if let Some(suggestion) = suggestion {
        let rest = error.hint.take().unwrap_or_default();
        error.hint = Some(format!("did you mean `{suggestion}`? {rest}").trim_end().to_owned());
    }
    error
}

/// The override path with its first misspelt name corrected, when the base character has a name close to it where the
/// path has none. Only the names the character's own definition has are used, so a sparse key that is new on purpose is
/// never "corrected" when nothing is close.
fn field_suggestion(def: &CharacterDef, path: &DocPath) -> Option<String> {
    let base = serde_json::to_value(def).ok()?;
    let mut node = &base;
    for (depth, segment) in path.segments().iter().enumerate() {
        let Value::Object(fields) = node else { return None };
        if let Some(child) = fields.get(segment) {
            node = child;
            continue;
        }
        let fixed = nearest(segment, fields.keys().map(String::as_str))?;
        let mut corrected = path.segments().to_vec();
        corrected[depth].clone_from(&fixed.to_owned());
        return Some(corrected.join("/"));
    }
    None
}

/// Refuses an override under `parts/<id>` when the character has no part `<id>`. The typed check alone would call that a
/// value of the wrong shape, not a missing part, so the part is named here, with the nearest part id when it is a typo.
fn check_part(def: &CharacterDef, owner: &CharId, path: &DocPath) -> Result<(), OpError> {
    let segments = path.segments();
    if segments.first().map(String::as_str) != Some("parts") {
        return Ok(());
    }
    let Some(part) = segments.get(1) else { return Ok(()) };
    if def.parts.keys().any(|id| id.as_str() == part) {
        return Ok(());
    }
    let suggestion = nearest(part, def.parts.keys().map(PartId::as_str)).map(str::to_owned);
    let error = character::not_found("path", format!("the override `{path}` names no part `{part}` on `{owner}`"))
        .hint(format!("the parts of `{owner}` are: {}", list(def.parts.keys())));
    Err(with_suggestion(error, suggestion))
}

/// Checks an override of a part's parameters with the part kind's own check, which the builder runs anyway. Nothing
/// else looks inside the parameters, so a misspelt parameter would otherwise be a sparse key that fails only when the
/// part builds.
fn check_params(
    def: &CharacterDef,
    resolved: &CharacterDef,
    variant: &VariantId,
    path: &DocPath,
) -> Result<(), OpError> {
    let segments = path.segments();
    if segments.len() < 3 || segments[0] != "parts" || segments[2] != "params" {
        return Ok(());
    }
    let Some(part) = resolved.parts.iter().find(|(id, _)| id.as_str() == segments[1]).map(|(_, part)| part) else {
        return Ok(());
    };
    let Some(kind) = find_kind(part.kind.as_str()) else { return Ok(()) };
    (kind.check)(&part.params).map_err(|e| {
        let error = OpError::invalid(
            "path",
            format!("the override `{path}` of variant `{variant}` doesn't fit the `{}` parameters: {e}", part.kind),
        )
        .hint("name a parameter the part kind has, with a value of its type; the kind's schema lists them");
        with_suggestion(error, field_suggestion(def, path))
    })?;
    Ok(())
}

impl Op for VariantSetOverride {
    fn meta() -> OpMeta {
        OpMeta {
            name: "variant.set_override",
            summary: "Set one override of a variant (a document path and its new value); refused, naming the variant and the path, when it doesn't resolve.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = character::load(ctx, &self.character)?;
        let mut variant = access::load(ctx, &self.character, &self.variant)?;
        let path = access::parse_path(&self.path)?;
        if access::holds_null(&self.value) {
            return Err(OpError::invalid(
                "value",
                format!("the value of `{path}` holds a null, which a project file can't store"),
            )
            .hint("give a number, text, a list or a table; to remove the override, use variant.clear_override"));
        }
        check_part(&def, &self.character, &path)?;

        variant.overrides.insert(path.clone(), self.value.clone());
        let set = access::with_ancestors(ctx, &self.character, &self.variant, variant.clone())?;
        let resolved = resolve_variant(&def, &set, &self.variant)
            .map_err(|e| with_suggestion(access::refusal(&e, "path"), field_suggestion(&def, &path)))?;
        check_params(&def, &resolved, &self.variant, &path)?;

        let stored = serde_json::to_value(&variant.overrides)
            .map_err(|e| OpError::internal(format!("serialising the overrides of `{}` failed: {e}", self.variant)))?;
        let change = character::change(ctx, access::overrides_path(&self.character, &self.variant), Some(stored));
        let summary = json!({
            "character": self.character,
            "variant": self.variant,
            "path": path.to_string(),
            "value": self.value,
        });
        Ok(character::edit_plan(vec![change], summary))
    }
}

register_op!(VariantSetOverride);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_takes_the_closest_name_within_reach_and_none_otherwise() {
        assert_eq!(nearest("hed", ["hair", "head", "body"]), Some("head"));
        assert_eq!(nearest("zzzz", ["hair", "head", "body"]), None);
        assert_eq!(nearest("ab", ["ba", "xy"]), Some("ba"), "a transposition is two edits");
        assert_eq!(nearest("ab", ["xyz", "zzzz"]), None);
    }
}
