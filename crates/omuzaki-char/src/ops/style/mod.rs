//! Style ops: `character.set_style` applies a character style (`crate::style`) to a character in one undoable patch,
//! and `character.list_styles` lists the styles.

use omuzaki_char_api::{CharId, paths};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ops::character::access::{self, list};
use crate::style::{self, StyleError};

/// The op error for a style that could not be found or applied.
pub(crate) fn style_error(e: StyleError) -> OpError {
    let names = || style::styles().map(|all| list(all.iter().map(|s| s.id.as_str()))).unwrap_or_default();
    match e {
        StyleError::Unknown(id) => access::not_found("style", format!("there is no style `{id}`"))
            .hint(format!("the styles are: {}; character.list_styles lists them", names())),
        StyleError::Template { .. } => OpError::invalid("style", e.to_string())
            .hint("move the character to one of those templates with body.set_template first"),
        StyleError::Missing(_) => {
            OpError::invalid("character", e.to_string()).hint("add the part with part.add, then apply the style")
        }
        StyleError::File { .. } | StyleError::Invalid(_) => OpError::internal(e.to_string()),
    }
}

/// Applies a character style: its face design (eye shape, lashes, mouth, nose, brows, face shape) and its head and
/// body numbers (neck, proportions), together, in one undoable step. `mature` turns the kit's design into an older
/// anime character on the `standard` and `tall` templates. Parts other than the head and body are left as they are.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"character": "kit", "style": "mature"}],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetStyle {
    /// The character to restyle.
    pub character: CharId,
    /// The style's id (`character.list_styles` lists them).
    pub style: String,
}

impl Op for SetStyle {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.set_style",
            summary: "Apply a character style (mature, ...): its face design, face shape, neck and proportions together.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let style = style::find(&self.style).map_err(style_error)?;
        let before = access::load(ctx, &self.character)?;
        let mut after = before.clone();
        style.apply(&mut after).map_err(style_error)?;
        let mut changes = Vec::new();
        let mut touched = Vec::new();
        if after.face != before.face {
            let value = serde_json::to_value(&after.face)
                .map_err(|e| OpError::internal(format!("a face did not serialise: {e}")))?;
            changes.push(access::change(ctx, paths::face(&self.character), Some(value)));
            touched.push("face".to_owned());
        }
        for (id, part) in &after.parts {
            if before.parts.get(id).is_some_and(|b| b.params != part.params) {
                let path = paths::part_params(&self.character, id);
                changes.push(access::change(ctx, path, Some(part.params.clone())));
                touched.push(format!("parts/{id}"));
            }
        }
        Ok(access::edit_plan(changes, json!({ "character": self.character, "style": style.id, "changed": touched })))
    }
}

register_op!(SetStyle);

/// Lists the character styles with what each looks like and the templates it is tuned for.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct ListStyles {}

impl Op for ListStyles {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.list_styles",
            summary: "List the character styles (mature, ...) with a summary and the templates each is tuned for.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let all = style::styles().map_err(style_error)?;
        let styles: Vec<_> = all
            .iter()
            .map(|s| json!({ "id": s.id, "name": s.name, "summary": s.summary, "templates": s.templates }))
            .collect();
        Ok(Plan::answer(json!({ "styles": styles })))
    }
}

register_op!(ListStyles);
