//! Adding and removing the groups of a hair part (#2452): a braid, a tail or a bun joins a hairstyle as a group of the
//! same part, so it shares the part's shading and physics instead of being a second hair part to keep in step.
//!
//! | Op | Does |
//! |---|---|
//! | `hair.group.add` | a new group of any generator type, with that generator's defaults or the fields given |
//! | `hair.group.remove` | removes a group, refusing the last one |
//!
//! Both write the part's `groups` back as one change (`part::changes`), so undo restores it exactly.

use omuzaki_char_api::{CharId, GroupId, HairGroup, PartId};
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::part;
use crate::ops::character::access as character;
use crate::ops::character::access::{list, not_found};

/// The generator types a group can be, for hints.
const TYPES: &str = "cap, part_sweep, framing, braid, curtain, bangs, tail, bun, guides";

/// Adds a group to a hair part. `generator` names the group's type (`{"type": "braid"}`) and any of its fields to
/// change from that type's defaults; `part.describe` lists them once the group is there. Groups build in id order and
/// later groups draw over earlier ones, so a prefix such as `e-` places it. `simulate` turns its physics on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "e-braid", "generator": {"type": "braid", "strands": 4}},
        {"character": "kit", "part": "hair", "group": "e-tail", "generator": {"type": "tail"}, "simulate": true}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairGroupAdd {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The new group's id: not used by another group of the part.
    pub group: GroupId,
    /// The generator: an object with its `type` (cap, `part_sweep`, framing, braid, curtain, bangs, tail, bun or guides)
    /// and any of its fields.
    pub generator: Value,
    /// Simulate the group with the part's hair physics (rigid to the head when left out).
    #[serde(default)]
    pub simulate: Option<bool>,
    /// Also build its mirror image across the midline (for a one-sided generator).
    #[serde(default)]
    pub mirror: Option<bool>,
}

impl Op for HairGroupAdd {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.group.add",
            summary: "Add a group (braid, tail, bun, bangs, locks or guides) to a hair part, with its generator's \
                      defaults or the fields given",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        if found.hair.groups.contains_key(&self.group) {
            return Err(OpError::invalid("group", format!("`{}` already has a group `{}`", self.part, self.group))
                .hint(format!("its groups: {}; choose a free id", list(found.hair.groups.keys()))));
        }
        if self.generator.get("type").and_then(Value::as_str).is_none() {
            return Err(OpError::invalid("generator", "the generator needs its `type`")
                .hint(format!("for example {{\"type\": \"braid\"}}; the types are {TYPES}")));
        }
        let mut given = json!({ "generator": self.generator });
        if let Some(mirror) = self.mirror {
            given["mirror"] = json!(mirror);
        }
        let mut group: HairGroup = serde_json::from_value(given).map_err(|e| {
            OpError::invalid("generator", format!("the generator isn't one a group can hold: {e}"))
                .hint(format!("the types are {TYPES}; part.describe lists a type's fields once a group has it"))
        })?;
        if let Some(simulate) = self.simulate {
            group.dynamics.simulate = simulate;
        }
        let mut after = found.hair.clone();
        after.groups.insert(self.group.clone(), group);
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "group": self.group,
            "groups": after.groups.keys().collect::<Vec<_>>(),
        });
        Ok(character::edit_plan(part::changes(ctx, &found, &after, &["groups"], "generator")?, summary))
    }
}

register_op!(HairGroupAdd);

/// Removes a group from a hair part. The last group can't go (remove the part with `part.remove` instead); to keep a
/// group but stop building it, set its `enabled` to false with `part.set_param`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"character": "kit", "part": "hair", "group": "d-framing"}],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairGroupRemove {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The group to remove.
    pub group: GroupId,
}

impl Op for HairGroupRemove {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.group.remove",
            summary: "Remove a group from a hair part (not its last one)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        if !found.hair.groups.contains_key(&self.group) {
            return Err(not_found("group", format!("`{}` has no group `{}`", self.part, self.group))
                .hint(format!("its groups: {}", list(found.hair.groups.keys()))));
        }
        if found.hair.groups.len() == 1 {
            return Err(OpError::invalid("group", format!("`{}` is the only group of `{}`", self.group, self.part))
                .hint("remove the whole part with part.remove, or turn the group off with part.set_param enabled"));
        }
        let mut after = found.hair.clone();
        after.groups.remove(&self.group);
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "group": self.group,
            "groups": after.groups.keys().collect::<Vec<_>>(),
        });
        Ok(character::edit_plan(part::changes(ctx, &found, &after, &["groups"], "group")?, summary))
    }
}

register_op!(HairGroupRemove);
