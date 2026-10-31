//! `face.expression.add_from_library`: brings expressions of the built-in library into a character's face that it lacks,
//! so a character made before the library grew (a `grin`, say) can use the new ones. Expressions the face already has
//! are never touched, whatever their tuning.

use omuzaki_char_api::CharId;
use omuzaki_doc_api::Patch;
use omuzaki_face_api::{ExpressionId, ExpressionSet};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::store::{edited_face, entry, library, patch_to};
use crate::ops_shared::edit::{changed, stored_face, to_json};

/// Adds the built-in library's expressions that a character's face lacks: the ones named, or every one it lacks when
/// none are named. An expression the face already has is kept as it is (named or not), so this never undoes a tuning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"character": "kit", "ids": ["grin"]}], "x-omuzaki-fixture" = "character"))]
pub struct AddFromLibrary {
    /// The character whose face gains the expressions.
    pub character: CharId,
    /// The library's expressions to add. Left out, every library expression the face lacks is added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<ExpressionId>>,
}

impl Op for AddFromLibrary {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.expression.add_from_library",
            summary: "Add the built-in library's expressions a character's face lacks (named ones, or all it lacks), \
                      keeping every expression it already has",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let built_in = ExpressionSet::library().map_err(|e| OpError::internal(format!("the expression library: {e}")))?;
        if let Some(ids) = &self.ids
            && let Some(unknown) = ids.iter().find(|id| !built_in.0.contains_key(*id))
        {
            let names: Vec<&str> = built_in.0.keys().map(ExpressionId::as_str).collect();
            return Err(OpError::invalid("ids", format!("the library has no expression '{unknown}'"))
                .hint(format!("its expressions are {}", names.join(", "))));
        }
        let face = stored_face(ctx, &self.character)?;
        let stored = library(face.as_ref())?;
        let wanted = |id: &ExpressionId| self.ids.as_ref().is_none_or(|ids| ids.contains(id));
        let mut added = Vec::new();
        let mut kept = Vec::new();
        let mut new = Vec::new();
        for (id, expression) in &built_in.0 {
            if !wanted(id) {
                continue;
            }
            if entry(stored, id).is_some() {
                kept.push(id.as_str().to_owned());
            } else {
                added.push(id.as_str().to_owned());
                new.push((id.as_str().to_owned(), to_json(expression)?));
            }
        }
        let working = edited_face(face.as_ref(), |entries| entries.extend(new))?;
        let patch = if added.is_empty() { Patch::new() } else { patch_to(&self.character, face.as_ref(), &working) };
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "added": added, "kept": kept, "changed": changed(&plan.patch) }));
        Ok(plan)
    }
}

register_op!(AddFromLibrary);
