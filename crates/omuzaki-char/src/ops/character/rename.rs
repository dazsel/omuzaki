//! `character.rename`: change a character's display name.

use omuzaki_char_api::{CharId, paths};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access;

/// Changes a character's display name. The id (its folder in the project, and what every other op calls it by) stays
/// the same.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CharacterRename {
    /// The character to rename.
    pub character: CharId,
    /// The new display name: any non-empty text.
    #[schemars(length(min = 1))]
    pub name: String,
}

impl Op for CharacterRename {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.rename",
            summary: "Change a character's display name; its id stays the same.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let name = self.name.trim();
        if name.is_empty() {
            return Err(OpError::invalid("name", "the name is empty")
                .hint("give the character a display name, for example \"Mira\""));
        }
        let changes = if name == def.name {
            Vec::new()
        } else {
            vec![access::change(ctx, paths::character(&self.character).join("name"), Some(json!(name)))]
        };
        let summary = json!({ "character": self.character, "name": name, "changed": !changes.is_empty() });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(CharacterRename);
