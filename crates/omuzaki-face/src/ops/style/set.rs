//! `face.set_style`: writes one face field and returns the smallest patch that makes it.

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::ops_shared::check::check_edit;
use crate::ops_shared::edit::{changed, diff, first_null, marks_len, put, stored_face};
use crate::ops_shared::path::{canonical, chain, steps, template};

/// Sets one field of a character's face: a key under `face/eyes`, `brows`, `mouth`, `nose` or `cheeks`, or a field of one
/// mark (`face/marks/<i>/...`). An object value sets the fields it names and leaves the others. The marks list itself is
/// changed with `face.add_mark` and `face.remove_mark`, never here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetStyle {
    /// The character whose face changes.
    pub character: CharId,
    /// Where in the face, for example `face/eyes/iris/width`. The leading `face/` may be left out.
    pub path: String,
    /// The new value, of the type the field holds: a number, a colour (`#rrggbb` or `@slot`), a name from the schema, or
    /// an object for several fields at once.
    pub value: Value,
}

impl Op for SetStyle {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.set_style",
            summary: "Set one face field (eyes, brows, mouth, nose, cheeks or a mark's field), validate the whole face, \
                      and return the changed paths",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let keys = steps(&self.path)?;
        let field = canonical(&keys);
        if matches!(keys.as_slice(), [only] if only == "marks") {
            return Err(OpError::invalid(field, "the marks list can't be replaced whole").hint(
                "add a mark with `face.add_mark`, remove the last one with `face.remove_mark`, and set a mark's own \
                       fields as face/marks/<i>/<field>",
            ));
        }
        let stored = stored_face(ctx, &self.character)?;
        let defaults = template()?;
        let chain = chain(&defaults, &keys, marks_len(stored.as_ref()))?;
        if let Some(at) = first_null(&self.value, &field) {
            return Err(OpError::invalid(at.clone(), format!("{at} is null, and the face holds no null"))
                .hint("give the field a value of its type; a null can't be written"));
        }

        let mut working = stored.clone().unwrap_or_else(|| Value::Object(Map::new()));
        put(&mut working, "face", &keys, &chain, self.value.clone())?;
        check_edit(stored.as_ref(), &working, &defaults)?;

        let mut patch = Patch::new();
        diff(&paths::face(&self.character), stored.as_ref(), Some(&working), &mut patch);
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "changed": changed(&plan.patch) }));
        Ok(plan)
    }
}

register_op!(SetStyle);
