//! `part.reorder`: change where a part builds, by its layer or its parent.

use omuzaki_char_api::{CharId, CharacterDef, PartDef, PartId, paths};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ops::character::access;
use crate::ops::character::tree::{self, Problem};

/// Changes where a part builds: its `layer`, its `parent`, or both.
///
/// Layers order building from the inside out (body and head 0, shirt 20, vest 30, outer garments 40 and up,
/// accessories 50 and up), and a part builds after its parent, after every part with a lower layer and after the kinds
/// it requires. Give at least one of `layer`, `parent` and `detach`. Refused, naming the parts, when the result would
/// contradict itself: a part can't be its own ancestor, and the build order can't loop.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartReorder {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part to move.
    pub part: PartId,
    /// The new layer, a whole number (negative allowed). Left out, the layer stays as it is.
    #[serde(default)]
    pub layer: Option<i32>,
    /// The new parent, another part of this character (not the part itself or anything below it). Left out, the
    /// parent stays as it is; to remove the parent use `detach`.
    #[serde(default)]
    pub parent: Option<PartId>,
    /// `true` to make the part top-level by removing its parent. Cannot be combined with `parent`.
    #[serde(default)]
    pub detach: bool,
}

/// The chain from `new_parent` up to `part` when `new_parent` is one of `part`'s descendants, which would make the
/// part its own ancestor.
fn chain_back_to(def: &CharacterDef, part: &PartId, new_parent: &PartId) -> Option<Vec<PartId>> {
    let mut chain = vec![new_parent.clone()];
    let mut cursor = new_parent;
    // A loop already in the data ends the walk after at most one lap.
    for _ in 0..=def.parts.len() {
        let next = def.parts.get(cursor)?.parent.as_ref()?;
        chain.push(next.clone());
        if next == part {
            return Some(chain);
        }
        cursor = next;
    }
    None
}

impl PartReorder {
    /// Rejects an unusable combination of arguments before the character is read.
    fn check_arguments(&self) -> Result<(), OpError> {
        if self.detach && self.parent.is_some() {
            return Err(OpError::invalid("detach", "`parent` and `detach` contradict each other")
                .hint("give `parent` to move the part under another one, or `detach` to make it top-level, not both"));
        }
        if self.layer.is_none() && self.parent.is_none() && !self.detach {
            return Err(OpError::invalid("layer", "there is nothing to change")
                .hint("give a `layer`, a `parent`, or `detach: true`"));
        }
        Ok(())
    }

    /// Checks the new parent against the character.
    fn check_parent(&self, def: &CharacterDef, parent: &PartId) -> Result<(), OpError> {
        access::part(def, &self.character, parent, "parent")?;
        if *parent == self.part {
            return Err(OpError::invalid("parent", format!("`{}` can't be its own parent", self.part))
                .hint("pick another part, or `detach` to make it top-level"));
        }
        if let Some(chain) = chain_back_to(def, &self.part, parent) {
            let route = chain.iter().map(ToString::to_string).collect::<Vec<_>>().join(" -> ");
            return Err(OpError::invalid(
                "parent",
                format!("`{parent}` is below `{}` ({route}), so it can't become its parent", self.part),
            )
            .hint("pick a part that isn't below this one, or move that part first"));
        }
        Ok(())
    }
}

impl PartReorder {
    /// The argument that changes the parent: `detach` when the part is made top-level, otherwise `parent`.
    fn parent_argument(&self) -> &'static str {
        if self.detach { "detach" } else { "parent" }
    }

    /// The argument to blame when the moved part cannot be put in build order: the layer or the parent change,
    /// whichever causes it alone (the layer if only the two together do, and it was given).
    fn loop_culprit(&self, def: &CharacterDef, current: &PartDef, layer: i32, parent: Option<&PartId>) -> &'static str {
        let moved = |layer: i32, parent: Option<PartId>| {
            let mut after = def.clone();
            if let Some(part) = after.parts.get_mut(&self.part) {
                part.layer = layer;
                part.parent = parent;
            }
            after
        };
        let mut candidates = Vec::new();
        if self.layer.is_some() {
            candidates.push(("layer", moved(layer, current.parent.clone())));
        }
        if self.detach || self.parent.is_some() {
            candidates.push((self.parent_argument(), moved(current.layer, parent.cloned())));
        }
        let fallback = if self.layer.is_some() { "layer" } else { self.parent_argument() };
        tree::culprit(def, &candidates, fallback)
    }
}

impl Op for PartReorder {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.reorder",
            summary: "Change a part's layer (build order, inner to outer) or its parent.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.check_arguments()?;
        let def = access::load(ctx, &self.character)?;
        let current = access::part(&def, &self.character, &self.part, "part")?;
        if let Some(parent) = &self.parent {
            self.check_parent(&def, parent)?;
        }

        let layer = self.layer.unwrap_or(current.layer);
        let parent = if self.detach { None } else { self.parent.clone().or_else(|| current.parent.clone()) };
        let mut after = def.clone();
        if let Some(part) = after.parts.get_mut(&self.part) {
            part.layer = layer;
            part.parent.clone_from(&parent);
        }
        let order = tree::check_edit(&def, &after, &format!("moving `{}`", self.part), |problem| match problem {
            Problem::Blocked { .. } => self.loop_culprit(&def, current, layer, parent.as_ref()),
            _ => self.parent_argument(),
        })?;

        let base = paths::part(&self.character, &self.part);
        let mut changes = Vec::new();
        if layer != current.layer {
            changes.push(access::change(ctx, base.join("layer"), Some(json!(layer))));
        }
        if parent != current.parent {
            changes.push(access::change(ctx, base.join("parent"), parent.as_ref().map(|p| json!(p))));
        }
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "layer": layer,
            "parent": parent,
            "changed": !changes.is_empty(),
            "build_order": order,
            "note": tree::idle_reason(&after, &self.part),
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PartReorder);
