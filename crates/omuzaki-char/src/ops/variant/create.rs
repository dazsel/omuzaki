//! `variant.create`: a new variant of a character, with no overrides yet.

use std::collections::BTreeMap;

use omuzaki_char_api::{CharId, ShotRange, VariantDef, VariantId, paths, resolve_variant};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access;
use crate::ops::character::access as character;

/// Creates a variant of a character. It starts with no overrides, so it looks like the base character until
/// `variant.set_override` changes it.
///
/// `extends` names another variant whose overrides the new one starts from, so a close-up can build on the game face
/// and change only what differs. `shot` says which head coverage the variant is for (0.1 for a full-body shot, 0.5 for
/// a portrait, 1 and above for an extreme close-up); a variant without one is only chosen by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VariantCreate {
    /// The character the variant belongs to.
    pub character: CharId,
    /// The new variant's id: a slug such as `close-up`. Must not be in use. It names the variant's file, so it never
    /// changes.
    pub id: VariantId,
    /// Another variant of the character to start from: its overrides apply first, and this variant's own override
    /// win over them.
    pub extends: Option<VariantId>,
    /// The head coverage the variant is for: `min` (inclusive) to `max` (exclusive), as fractions of the frame height.
    pub shot: Option<ShotRange>,
}

/// Refuses a shot range that covers no head coverage: the start below zero, or an end that isn't above the start.
fn check_shot(shot: &ShotRange) -> Result<(), OpError> {
    if !shot.min.is_finite() || shot.min < 0.0 {
        return Err(OpError::invalid("shot.min", format!("the shot starts at {}, not a head coverage", shot.min))
            .hint("give a coverage from 0 (a far shot) upwards, as a fraction of the frame height, for example 0.35"));
    }
    if !shot.max.is_finite() || shot.max <= shot.min {
        return Err(OpError::invalid(
            "shot.max",
            format!("the shot ends at {}, not above its start {}", shot.max, shot.min),
        )
        .hint("make max larger than min, for example min 0.35 and max 1.0"));
    }
    Ok(())
}

impl Op for VariantCreate {
    fn meta() -> OpMeta {
        OpMeta {
            name: "variant.create",
            summary: "Create a variant of a character with no overrides yet, optionally extending another and covering a range of shots.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = character::load(ctx, &self.character)?;
        let path = paths::variant(&self.character, &self.id);
        if ctx.doc.get(&path).is_some() {
            return Err(OpError::invalid(
                "id",
                format!("a variant `{}` already exists on `{}`", self.id, self.character),
            )
            .hint("choose another id, or change the existing variant with variant.set_override"));
        }
        if let Some(parent) = &self.extends
            && ctx.doc.get(&paths::variant(&self.character, parent)).is_none()
        {
            return Err(access::missing(ctx, &self.character, parent, "extends"));
        }
        if let Some(shot) = &self.shot {
            check_shot(shot)?;
        }

        let variant = VariantDef {
            description: String::new(),
            extends: self.extends.clone(),
            overrides: BTreeMap::new(),
            shot: self.shot,
        };
        // Resolve the new variant before writing it, so a broken variant it extends is named now.
        let set = access::with_ancestors(ctx, &self.character, &self.id, variant.clone())?;
        resolve_variant(&def, &set, &self.id).map_err(|e| access::refusal(&e, "variant"))?;

        // `extends` and `shot` are left out of the stored node when absent (VariantDef skips them), so it holds no null.
        let stored = serde_json::to_value(&variant)
            .map_err(|e| OpError::internal(format!("serialising variant `{}` failed: {e}", self.id)))?;
        let summary = json!({
            "character": self.character,
            "variant": self.id,
            "extends": self.extends,
            "shot": self.shot,
        });
        let changes = character::create(ctx, path, stored);
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(VariantCreate);
