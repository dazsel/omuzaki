//! The guide-curve ops (CB-32, #180): editing a hair group made of explicit guide curves, point by point, and freezing
//! any other group into one.
//!
//! | Op | Does |
//! |---|---|
//! | `hair.guide.move_point` | moves one control point (the viewport drag), anchoring it to the head's surface when it lands near it |
//! | `hair.guide.add_point` | inserts a control point in a clump, at the end by default |
//! | `hair.guide.remove_point` | removes a control point, keeping at least two |
//! | `hair.guide.add_clump` | adds a clump of guide points to a group, making the group when it is new; answers its index |
//! | `hair.guide.remove_clump` | removes a clump |
//! | `hair.guide.mirror` | reflects every clump of a group left to right |
//! | `hair.guide.jitter` | moves every point but the roots by up to `amount`, the same for the same seed |
//! | `hair.group.to_guides` | freezes any group's clumps as explicit guides, so a procedural style can be hand-edited |
//!
//! Each writes one change at the deepest node it touches (a clump's `points`, a group's `clumps`, a group), so undo
//! restores exactly what the file held. A point is `{"surface": [theta_deg, phi_deg, offset]}` (on the head; theta from
//! the top, phi from the front toward the character's left), `{"head": [x, y, z]}` (metres from the head centre, in the
//! head's frame) or `{"body": [x, y, z]}` (bind space).

use omuzaki_char_api::{
    BuildServices, CharId, Dynamics, Generator, GroupId, GuideClump, GuidePoint, GuidesGen, HairGroup, HairParams,
    PartId,
};
use omuzaki_doc_api::Change;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::part::{self, HairPart};
use crate::hair::compile::is_bilateral;
use crate::hair::edit::{self, NEAR};
use crate::hair::r#gen::guides;
use crate::ops::character::access as character;
use crate::ops::character::access::{list, not_found};

/// The most clumps a guides group may hold.
pub const MAX_CLUMPS: usize = omuzaki_char_api::hair::MAX_GUIDE_CLUMPS;
/// The most control points a clump may hold.
pub const MAX_POINTS: usize = omuzaki_char_api::hair::MAX_GUIDE_POINTS;
/// The largest jitter, metres.
pub const MAX_JITTER: f64 = 0.05;

/// The guides of group `group`, or a refusal naming what the group is.
fn guides_of<'a>(hair: &'a mut HairParams, part: &PartId, group: &GroupId) -> Result<&'a mut GuidesGen, OpError> {
    let names: Vec<String> = hair.groups.keys().map(ToString::to_string).collect();
    let Some(found) = hair.groups.get_mut(group) else {
        return Err(not_found("group", format!("`{part}` has no group `{group}`"))
            .hint(format!("its groups: {}", list(names))));
    };
    match &mut found.generator {
        Generator::Guides(g) => Ok(g),
        other => {
            let kind = serde_json::to_value(&*other).ok().and_then(|v| v.get("type").cloned()).unwrap_or_default();
            Err(OpError::invalid("group", format!("the group `{group}` is made by the {kind} generator, not guides"))
                .hint("freeze it into editable guides with hair.group.to_guides first"))
        }
    }
}

/// Clump `index` of `g`.
fn clump_at<'a>(g: &'a mut GuidesGen, index: usize, group: &GroupId) -> Result<&'a mut GuideClump, OpError> {
    let count = g.clumps.len();
    g.clumps.get_mut(index).ok_or_else(|| {
        not_found("clump", format!("the group `{group}` has {count} clumps, numbered from 0"))
            .hint("add one with hair.guide.add_clump")
    })
}

/// The changes that store `after` for the edit at `steps` below the group (`["generator", "clumps", "2", "points"]`).
fn write(
    ctx: &OpCtx<'_>,
    found: &HairPart,
    after: &HairParams,
    group: &GroupId,
    steps: &[&str],
    field: &str,
) -> Result<Vec<Change>, OpError> {
    let mut path = vec!["groups", group.as_str()];
    path.extend_from_slice(steps);
    part::changes(ctx, found, after, &path, field)
}

/// `point` as stored: anchored to the head's surface when `anchor` and it lies within 3 cm of it.
fn placed(found: &HairPart, character: &CharId, part: &PartId, point: GuidePoint, anchor: bool) -> Result<GuidePoint, OpError> {
    if !anchor || matches!(point, GuidePoint::Surface(_)) {
        return Ok(point);
    }
    let services = BuildServices::default();
    let head = edit::head_of(character, &found.def, part, &services).map_err(|e| {
        OpError::invalid("character", format!("the head to anchor the point to doesn't build: {e}"))
            .hint("pass \"anchor\": false to store the point as given")
    })?;
    Ok(guides::anchored(head.as_ref(), point.resolve(head.as_ref()), NEAR))
}

/// Refuses a point the contract would: non-finite, or a surface point out of range.
fn check_point(point: &GuidePoint, field: &str) -> Result<(), OpError> {
    omuzaki_face_api::Validate::validate(point).map_err(|e| {
        OpError::invalid(field, format!("not a guide point: {e}"))
            .hint("surface: theta 0 to 180 degrees, offset -0.05 to 0.3 m; head and body: finite metres")
    })
}

fn yes() -> bool {
    true
}

/// Moves control point `index` of clump `clump` in a guides group to `point`: the viewport's drag. With `anchor` (the
/// default), a point that lands within 3 cm of the head's surface is stored on the surface, so it moves when the head
/// is reshaped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "e-guides", "clump": 0, "index": 1, "point": {"surface": [60.0, 20.0, 0.02]}}
    ],
    "x-omuzaki-fixture" = "hair-guides"
))]
pub struct HairGuideMovePoint {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The guides group.
    pub group: GroupId,
    /// Which clump, from 0.
    pub clump: usize,
    /// Which control point, from 0 at the root.
    pub index: usize,
    /// Where it goes.
    pub point: GuidePoint,
    /// Anchor it to the head's surface when it lands near it.
    #[serde(default = "yes")]
    pub anchor: bool,
}

impl Op for HairGuideMovePoint {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.guide.move_point",
            summary: "Move one control point of a hair guide clump, anchoring it to the head's surface when it lands \
                      near it",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_point(&self.point, "point")?;
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let clump = clump_at(guides_of(&mut after, &self.part, &self.group)?, self.clump, &self.group)?;
        let count = clump.points.len();
        if self.index >= count {
            return Err(not_found("index", format!("clump {} has {count} points, numbered from 0", self.clump))
                .hint("add one with hair.guide.add_point"));
        }
        let before = clump.points[self.index];
        let point = placed(&found, &self.character, &self.part, self.point, self.anchor)?;
        clump_at(guides_of(&mut after, &self.part, &self.group)?, self.clump, &self.group)?.points[self.index] = point;
        let clump = self.clump.to_string();
        let changes = write(ctx, &found, &after, &self.group, &["generator", "clumps", &clump, "points"], "point")?;
        let summary = json!({
            "character": self.character, "part": self.part, "group": self.group, "clump": self.clump,
            "index": self.index, "before": before, "after": point,
        });
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(HairGuideMovePoint);

/// Inserts a control point into clump `clump` of a guides group, before point `index` (at the end when left out). With
/// `anchor` (the default) a point within 3 cm of the head's surface is stored on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "e-guides", "clump": 0, "point": {"head": [0.02, -0.2, -0.1]}}
    ],
    "x-omuzaki-fixture" = "hair-guides"
))]
pub struct HairGuideAddPoint {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The guides group.
    pub group: GroupId,
    /// Which clump, from 0.
    pub clump: usize,
    /// Insert before this point; the end when left out.
    #[serde(default)]
    pub index: Option<usize>,
    /// The new point.
    pub point: GuidePoint,
    /// Anchor it to the head's surface when it is near it.
    #[serde(default = "yes")]
    pub anchor: bool,
}

impl Op for HairGuideAddPoint {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.guide.add_point",
            summary: "Insert a control point into a hair guide clump (at the end by default)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_point(&self.point, "point")?;
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let count = clump_at(guides_of(&mut after, &self.part, &self.group)?, self.clump, &self.group)?.points.len();
        if count >= MAX_POINTS {
            return Err(OpError::invalid("clump", format!("clump {} already has {MAX_POINTS} points", self.clump))
                .hint("remove one with hair.guide.remove_point"));
        }
        let index = self.index.unwrap_or(count);
        if index > count {
            return Err(not_found("index", format!("clump {} has {count} points; insert at 0 to {count}", self.clump))
                .hint("leave index out to add at the end"));
        }
        let point = placed(&found, &self.character, &self.part, self.point, self.anchor)?;
        clump_at(guides_of(&mut after, &self.part, &self.group)?, self.clump, &self.group)?.points.insert(index, point);
        let clump = self.clump.to_string();
        let changes = write(ctx, &found, &after, &self.group, &["generator", "clumps", &clump, "points"], "point")?;
        let summary = json!({
            "character": self.character, "part": self.part, "group": self.group, "clump": self.clump,
            "index": index, "point": point,
        });
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(HairGuideAddPoint);

/// Removes control point `index` from clump `clump` of a guides group. A clump keeps at least two points.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "e-guides", "clump": 0, "index": 1}
    ],
    "x-omuzaki-fixture" = "hair-guides"
))]
pub struct HairGuideRemovePoint {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The guides group.
    pub group: GroupId,
    /// Which clump, from 0.
    pub clump: usize,
    /// Which point, from 0 at the root.
    pub index: usize,
}

impl Op for HairGuideRemovePoint {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.guide.remove_point",
            summary: "Remove a control point from a hair guide clump, keeping at least two",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let clump = clump_at(guides_of(&mut after, &self.part, &self.group)?, self.clump, &self.group)?;
        let count = clump.points.len();
        if self.index >= count {
            return Err(not_found("index", format!("clump {} has {count} points, numbered from 0", self.clump))
                .hint("part.describe lists them"));
        }
        if count <= 2 {
            return Err(OpError::invalid("index", format!("clump {} has only two points", self.clump))
                .hint("a clump needs two; remove the whole clump with hair.guide.remove_clump"));
        }
        let removed = clump.points.remove(self.index);
        let clump = self.clump.to_string();
        let changes = write(ctx, &found, &after, &self.group, &["generator", "clumps", &clump, "points"], "index")?;
        let summary = json!({
            "character": self.character, "part": self.part, "group": self.group, "clump": self.clump,
            "index": self.index, "removed": removed,
        });
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(HairGuideRemovePoint);

/// Adds a clump to a guides group: `points` (at least two, root first; see the module docs for the three kinds of
/// point), and optionally `width`, `thickness` and `shade`. A group id that doesn't exist yet makes a new guides group,
/// rigid to the head (`hair.group.set_dynamics` makes it swing). Answers the clump's index.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "e-guides", "clump": {"points": [{"surface": [70.0, 30.0, 0.0]}, {"head": [0.07, -0.12, 0.04]}]}}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairGuideAddClump {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The guides group; a new id makes a new group.
    pub group: GroupId,
    /// The clump.
    pub clump: GuideClump,
}

impl Op for HairGuideAddClump {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.guide.add_clump",
            summary: "Add a clump of guide points to a hair group (a new group id makes a guides group)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.clump.points.len() < 2 {
            return Err(OpError::invalid("clump", "a clump needs at least two points, root first")
                .hint("for example {\"points\": [{\"surface\": [70, 30, 0]}, {\"head\": [0.07, -0.12, 0.04]}]}"));
        }
        if self.clump.points.len() > MAX_POINTS {
            return Err(OpError::invalid("clump", format!("a clump holds at most {MAX_POINTS} points"))
                .hint("the curve is smoothed through them; a few points make a lock"));
        }
        for (k, p) in self.clump.points.iter().enumerate() {
            check_point(p, &format!("clump/points/{k}"))?;
        }
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let created = !after.groups.contains_key(&self.group);
        if created {
            let group = HairGroup {
                enabled: true,
                mirror: false,
                dynamics: Dynamics::default(),
                generator: Generator::Guides(GuidesGen::default()),
                lock: omuzaki_char_api::LockShape::default(),
            };
            after.groups.insert(self.group.clone(), group);
        }
        let g = guides_of(&mut after, &self.part, &self.group)?;
        if g.clumps.len() >= MAX_CLUMPS {
            return Err(OpError::invalid("group", format!("the group `{}` already has {MAX_CLUMPS} clumps", self.group))
                .hint("start another group"));
        }
        g.clumps.push(self.clump.clone());
        let index = g.clumps.len() - 1;
        let steps: &[&str] = if created { &[] } else { &["generator", "clumps"] };
        let changes = write(ctx, &found, &after, &self.group, steps, "clump")?;
        let summary = json!({
            "character": self.character, "part": self.part, "group": self.group, "index": index,
            "created_group": created,
        });
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(HairGuideAddClump);

/// Removes clump `clump` from a guides group; the clumps after it move down by one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "e-guides", "clump": 0}
    ],
    "x-omuzaki-fixture" = "hair-guides"
))]
pub struct HairGuideRemoveClump {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The guides group.
    pub group: GroupId,
    /// Which clump, from 0.
    pub clump: usize,
}

impl Op for HairGuideRemoveClump {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.guide.remove_clump",
            summary: "Remove a clump from a hair guides group",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let g = guides_of(&mut after, &self.part, &self.group)?;
        clump_at(g, self.clump, &self.group)?;
        let removed = g.clumps.remove(self.clump);
        let changes = write(ctx, &found, &after, &self.group, &["generator", "clumps"], "clump")?;
        let summary = json!({
            "character": self.character, "part": self.part, "group": self.group, "clump": self.clump,
            "removed": removed,
        });
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(HairGuideRemoveClump);

/// Reflects every clump of a guides group left to right (a surface point's `phi` and a free point's `x` change sign);
/// twice is the identity. To get both sides, turn on the group's `mirror` instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "e-guides"}
    ],
    "x-omuzaki-fixture" = "hair-guides"
))]
pub struct HairGuideMirror {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The guides group.
    pub group: GroupId,
}

impl Op for HairGuideMirror {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.guide.mirror",
            summary: "Reflect every clump of a hair guides group left to right",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let g = guides_of(&mut after, &self.part, &self.group)?;
        *g = edit::mirrored(g);
        let clumps = g.clumps.len();
        let changes = write(ctx, &found, &after, &self.group, &["generator", "clumps"], "group")?;
        let summary = json!({ "character": self.character, "part": self.part, "group": self.group, "clumps": clumps });
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(HairGuideMirror);

/// Moves every control point of a guides group but each clump's root by up to `amount` metres (at most 0.05), drawn
/// from `seed`: the same seed gives the same hair. Roughens a hand-drawn style.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "e-guides", "amount": 0.005, "seed": 3}
    ],
    "x-omuzaki-fixture" = "hair-guides"
))]
pub struct HairGuideJitter {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The guides group.
    pub group: GroupId,
    /// The largest move, metres.
    pub amount: f64,
    /// Where the noise comes from.
    #[serde(default)]
    pub seed: u64,
}

impl Op for HairGuideJitter {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.guide.jitter",
            summary: "Move every point of a hair guides group but the roots by up to an amount, the same for a seed",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(self.amount > 0.0 && self.amount <= MAX_JITTER) {
            return Err(OpError::invalid("amount", format!("{} is out of range", self.amount))
                .hint(format!("use more than 0 and at most {MAX_JITTER} metres")));
        }
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let g = guides_of(&mut after, &self.part, &self.group)?;
        *g = edit::jittered(g, self.amount, self.seed);
        let changes = write(ctx, &found, &after, &self.group, &["generator", "clumps"], "amount")?;
        let summary = json!({
            "character": self.character, "part": self.part, "group": self.group, "amount": self.amount,
            "seed": self.seed,
        });
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(HairGuideJitter);

/// Freezes a hair group into explicit guide curves: its generator (a cap, a fringe, a curtain, framing locks, a tail…)
/// is replaced by the clumps it makes now, each a few control points whose curve stays within half a millimetre of
/// the original, so a procedural style can be edited point by point with `hair.guide.*`. A curtain's strip or a cap's
/// shell becomes tubes along the same curves. Undo restores the generator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "d-framing"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairGroupToGuides {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The group to freeze.
    pub group: GroupId,
}

impl Op for HairGroupToGuides {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.group.to_guides",
            summary: "Freeze a hair group into explicit guide curves, so a procedural style can be edited point by \
                      point",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        let Some(group) = found.hair.groups.get(&self.group) else {
            return Err(not_found("group", format!("`{}` has no group `{}`", self.part, self.group))
                .hint(format!("its groups: {}", list(found.hair.groups.keys()))));
        };
        if matches!(group.generator, Generator::Guides(_)) {
            return Err(OpError::invalid("group", format!("the group `{}` is already guides", self.group))
                .hint("edit it with hair.guide.move_point and the other hair.guide ops"));
        }
        if !group.enabled {
            return Err(OpError::invalid("group", format!("the group `{}` is turned off", self.group))
                .hint("enable it first: a group that doesn't build has no clumps to freeze"));
        }
        let services = BuildServices::default();
        let frozen = edit::freeze(&self.character, &found.def, &self.part, &found.hair, &self.group, &services)
            .map_err(|e| {
                OpError::invalid("group", format!("the group `{}` can't be frozen: {e}", self.group))
                    .hint("freeze a group of locks (a fringe, a curtain, framing locks, a tail, a bun)")
            })?
            .guides;
        if frozen.clumps.len() > MAX_CLUMPS {
            return Err(OpError::invalid("group", format!("the group makes {} clumps", frozen.clumps.len()))
                .hint(format!("a guides group holds at most {MAX_CLUMPS}; lower its count first")));
        }
        let mut after = found.hair.clone();
        let mirror = group.mirror && !is_bilateral(&group.generator);
        let from = serde_json::to_value(&group.generator).ok().and_then(|v| v.get("type").cloned());
        let points: usize = frozen.clumps.iter().map(|c| c.points.len()).sum();
        let clumps = frozen.clumps.len();
        if let Some(target) = after.groups.get_mut(&self.group) {
            target.generator = Generator::Guides(frozen);
            target.mirror = mirror;
        }
        let changes = write(ctx, &found, &after, &self.group, &[], "group")?;
        let summary = json!({
            "character": self.character, "part": self.part, "group": self.group, "from": from,
            "clumps": clumps, "points": points,
        });
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(HairGroupToGuides);
