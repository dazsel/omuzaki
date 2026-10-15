//! `face.add_mark` and `face.remove_mark`: the marks list changes only at its end, so each op writes one entry and the
//! undo is exact.

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_face_api::FaceMark;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::ops_shared::check::check_edit;
use crate::ops_shared::edit::{changed, marks_len, stored_face, to_json};
use crate::ops_shared::path::template;

/// Adds a mark (freckles, a beauty mark, face paint or a scar) at the end of a character's marks list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddMark {
    /// The character whose face gets the mark.
    pub character: CharId,
    /// The mark. A field it leaves out takes its default.
    #[serde(default)]
    pub mark: FaceMark,
}

impl Op for AddMark {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.add_mark",
            summary: "Add a face mark (freckles, beauty mark, paint or scar) at the end of a character's marks list",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let stored = stored_face(ctx, &self.character)?;
        let defaults = template()?;
        let index = marks_len(stored.as_ref());
        let mark = to_json(&self.mark)?;

        let mut working = stored.clone().unwrap_or_else(|| Value::Object(Map::new()));
        push_mark(&mut working, mark.clone())?;
        check_edit(stored.as_ref(), &working, &defaults)?;

        let patch = append_patch(&paths::face(&self.character), stored.as_ref(), working, index, mark);
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "index": index, "changed": changed(&plan.patch) }));
        Ok(plan)
    }
}

register_op!(AddMark);

/// Removes the last mark of a character's marks list. Marks come off the end only, so `index` must name the last one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveMark {
    /// The character whose face loses the mark.
    pub character: CharId,
    /// The position of the mark to remove: the last one, which is the number of marks minus one.
    pub index: usize,
}

impl Op for RemoveMark {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.remove_mark",
            summary: "Remove the last mark from a character's marks list (marks come off the end only)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let stored = stored_face(ctx, &self.character)?;
        let len = marks_len(stored.as_ref());
        let Some(last) = len.checked_sub(1) else {
            return Err(OpError::invalid("face/marks", format!("{} has no marks to remove", self.character))
                .hint("`face.add_mark` adds one"));
        };
        let field = format!("face/marks/{}", self.index);
        if self.index > last {
            return Err(OpError::invalid(field, format!("there is no mark {} (the character has {len})", self.index))
                .hint(format!("the marks are 0 to {last}")));
        }
        if self.index < last {
            return Err(OpError::invalid(
                field,
                format!("mark {} can't be removed: marks come off the end, and the last is {last}", self.index),
            )
            .hint(format!("remove mark {last} first, then mark {}", self.index)));
        }

        let mark = stored
            .as_ref()
            .and_then(|face| face.get("marks"))
            .and_then(|marks| marks.get(last))
            .cloned()
            .ok_or_else(|| OpError::internal("the last mark was counted but is missing"))?;
        let at = paths::face(&self.character).join("marks").join(last.to_string());
        let mut plan = Plan::edit(Patch::new().with(at, Some(mark), None));
        plan.output = Some(json!({ "index": last, "changed": changed(&plan.patch) }));
        Ok(plan)
    }
}

register_op!(RemoveMark);

/// Appends `mark` to the face's marks list in a working copy, creating the list when the face has none.
fn push_mark(face: &mut Value, mark: Value) -> Result<(), OpError> {
    let Value::Object(fields) = face else {
        return Err(OpError::internal("a face is an object"));
    };
    match fields.get_mut("marks") {
        Some(Value::Array(list)) => list.push(mark),
        Some(_) => {
            return Err(OpError::conflict("face/marks is not a list, so a mark can't be added to it")
                .hint("fix the character file"));
        }
        None => {
            fields.insert("marks".to_owned(), Value::Array(vec![mark]));
        }
    }
    Ok(())
}

/// The patch that adds the mark. It is one change at the new position when the list exists, so the list only grows at
/// its end; when the file has no list (or no face) the new list, or the whole face, is created in one change.
fn append_patch(face: &DocPath, stored: Option<&Value>, working: Value, index: usize, mark: Value) -> Patch {
    match stored {
        None => Patch::new().with(face.clone(), None, Some(working)),
        Some(stored) if stored.get("marks").is_some() => {
            Patch::new().with(face.join("marks").join(index.to_string()), None, Some(mark))
        }
        Some(_) => Patch::new().with(face.join("marks"), None, Some(Value::Array(vec![mark]))),
    }
}
