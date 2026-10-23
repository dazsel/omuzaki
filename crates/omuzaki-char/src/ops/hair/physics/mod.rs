//! The hair motion ops (CB-33, #321): a hair part's physics, its presets, each group's dynamics, and a settle query that
//! runs the simulation and says how the hair hangs and how fast it comes to rest.
//!
//! | Op | Does |
//! |---|---|
//! | `hair.physics.set` | one field of the part's [`HairPhysics`]: `damping`, `gravity_scale`, `wind_scale`, `iterations`, `collider_margin`, `fixed_hz`, `colliders` |
//! | `hair.physics.preset` | a named feel ([`Preset`]): the physics and every simulated group's stiffness at once |
//! | `hair.group.set_dynamics` | one group's `simulate`, `follow` and `stiffness` |
//! | `hair.settle` | (query) builds the character, lets the hair hang, turns it and reports the sag, the swing and the settle time |
//!
//! The edits write the part's `physics` or `groups` back as one change ([`part::changes`]), so undo restores it exactly.

use omuzaki_char_api::{
    CharId, Falloff, Follow, GroupId, HairParams, HairPhysics, JointRef, PartId, PoseSource, StepCtx,
};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{Change, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::part::{self, HairPart};
use crate::hair::physics::presets::Preset;
use crate::ops::character::access as character;
use crate::ops::character::access::{list, not_found};

/// The changes that store `physics` as the part's.
fn write_physics(ctx: &OpCtx<'_>, found: &HairPart, physics: HairPhysics, field: &str) -> Result<Vec<Change>, OpError> {
    let mut after = found.hair.clone();
    after.physics = physics;
    part::changes(ctx, found, &after, &["physics"], field)
}

/// Sets one field of a hair part's physics: `damping` (velocity kept each step, 0.5 to 1; higher swings longer),
/// `gravity_scale` (0 to 3), `wind_scale` (0 to 5), `iterations` (constraint passes, 1 to 8), `collider_margin`
/// (metres, up to 0.05), `fixed_hz` (the simulation rate, 30 to 240) or `colliders` (the part's own spheres; empty
/// uses the ones made from the body).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "field": "damping", "value": 0.95}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairPhysicsSet {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The physics field to set.
    pub field: String,
    /// The new value, of the type the field takes.
    pub value: Value,
}

impl Op for HairPhysicsSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.physics.set",
            summary: "Set one field of a hair part's physics: damping, gravity_scale, wind_scale, iterations, \
                      collider_margin, fixed_hz or colliders",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        let before = &found.hair.physics;
        let after = part::with_field(before, &self.field, &self.value, &[])?;
        let read = |p: &HairPhysics| serde_json::to_value(p).ok().and_then(|v| v.get(&self.field).cloned());
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "field": self.field,
            "before": read(before),
            "after": read(&after),
        });
        Ok(character::edit_plan(write_physics(ctx, &found, after, "value")?, summary))
    }
}

register_op!(HairPhysicsSet);

/// Gives a hair part a named feel: `silky` (the default: light, swings freely, springs back slowly), `heavy` (falls
/// hard, swings little), `bouncy` (a strong spring, a quick rebound) or `stiff` (barely moves). It sets the physics'
/// damping, gravity, wind and passes and the stiffness of every simulated group; the rate, the collider margin and the
/// part's own colliders stay as they are. Tune further with `hair.physics.set` and `hair.group.set_dynamics`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "preset": "heavy"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairPhysicsPreset {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The feel.
    pub preset: Preset,
}

impl Op for HairPhysicsPreset {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.physics.preset",
            summary: "Give a hair part a named feel for its motion: silky, heavy, bouncy or stiff (physics and the \
                      simulated groups' stiffness at once)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        after.physics = self.preset.physics(&found.hair.physics);
        let stiffness = self.preset.stiffness();
        let mut groups = Vec::new();
        for (id, group) in &mut after.groups {
            if group.dynamics.simulate {
                group.dynamics.stiffness = stiffness;
                groups.push(id.to_string());
            }
        }
        let mut changes = part::changes(ctx, &found, &after, &["physics"], "preset")?;
        // The groups are a second branch: write it from the state with the physics already in place.
        let staged = found.holding(HairParams { physics: after.physics.clone(), ..found.hair.clone() });
        changes.extend(part::changes(ctx, &staged, &after, &["groups"], "preset")?);
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "preset": self.preset,
            "physics": after.physics,
            "stiffness": stiffness,
            "groups": groups,
        });
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(HairPhysicsPreset);

/// Sets a hair group's motion: `simulate` (whether it moves with physics or stays rigid to the head), `follow` (`head`,
/// or `body`: the head near the scalp and the chest further down, for hair that lies on the back) and `stiffness`
/// (how firmly it springs back to the styled shape: `root` and `tip` from 0 free to 1 rigid, changing between `from`
/// and `to` along the lock). Give any of the three; `stiffness` may give only some of its fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "group": "c-curtain", "simulate": true, "stiffness": {"tip": 0.15}}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairGroupSetDynamics {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The group.
    pub group: GroupId,
    /// Whether the group is simulated.
    #[serde(default)]
    pub simulate: Option<bool>,
    /// What the group hangs from.
    #[serde(default)]
    pub follow: Option<Follow>,
    /// The fields of the group's stiffness to change.
    #[serde(default)]
    pub stiffness: Option<Value>,
}

impl Op for HairGroupSetDynamics {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.group.set_dynamics",
            summary: "Set a hair group's motion: whether it is simulated, what it follows (head or body) and its \
                      stiffness from root to tip",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.simulate.is_none() && self.follow.is_none() && self.stiffness.is_none() {
            return Err(OpError::invalid("simulate", "give at least one of simulate, follow and stiffness")
                .hint("for example {\"simulate\": true, \"stiffness\": {\"tip\": 0.15}}"));
        }
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let Some(group) = after.groups.get_mut(&self.group) else {
            return Err(not_found("group", format!("`{}` has no group `{}`", self.part, self.group))
                .hint(format!("its groups: {}", list(found.hair.groups.keys()))));
        };
        let before = group.dynamics.clone();
        if let Some(simulate) = self.simulate {
            group.dynamics.simulate = simulate;
        }
        if let Some(follow) = self.follow {
            group.dynamics.follow = follow;
        }
        if let Some(given) = &self.stiffness {
            group.dynamics.stiffness = merged(&group.dynamics.stiffness, given)?;
        }
        let dynamics = group.dynamics.clone();
        let steps = ["groups", self.group.as_str(), "dynamics"];
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "group": self.group,
            "before": before,
            "after": dynamics,
        });
        Ok(character::edit_plan(part::changes(ctx, &found, &after, &steps, "stiffness")?, summary))
    }
}

register_op!(HairGroupSetDynamics);

/// `current` with the fields of `given` (an object of some of a [`Falloff`]'s fields) set.
fn merged(current: &Falloff, given: &Value) -> Result<Falloff, OpError> {
    let Some(fields) = given.as_object() else {
        return Err(OpError::invalid("stiffness", "stiffness is an object of root, tip, from and to")
            .hint("for example {\"tip\": 0.15}"));
    };
    fields.iter().try_fold(*current, |falloff, (name, value)| {
        part::with_field(&falloff, name, value, &[]).map_err(|mut e| {
            e.field = Some("stiffness".to_owned());
            e
        })
    })
}

// ------------------------------------------------------------------------------------------------------- settle

/// Frame time the settle steps at, seconds.
const FRAME: f32 = 1.0 / 60.0;
/// How long the hair hangs before the turn, seconds.
const REST_S: f32 = 3.0;
/// How long the turn takes, seconds.
const TURN_S: f32 = 0.2;
/// The hair counts as settled once every point stays this close to its turned rest, metres.
const SETTLED: f32 = 0.002;
/// Longest the settle may watch after the turn, seconds.
const MAX_SECONDS: f64 = 20.0;

/// Builds the character, lets a hair part hang at rest, then turns the whole character `turn_deg` about the vertical
/// over 0.2 s and watches for `seconds`. Reports how far the hanging hair sags from its styled shape (`rest`), how far
/// it swings off its turned rest after the turn (`swing_mm`), and how long it takes to settle within 2 mm of it
/// (`settle_s`, from the end of the turn; null when it hasn't settled by the end). Read-only: use it to compare
/// presets or to check a tuning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "turn_deg": 25.0}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairSettle {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// How far the character turns, degrees, 0 to 180.
    #[serde(default = "default_turn")]
    pub turn_deg: f64,
    /// How long to watch after the turn, seconds, up to 20.
    #[serde(default = "default_seconds")]
    pub seconds: f64,
}

fn default_turn() -> f64 {
    25.0
}

fn default_seconds() -> f64 {
    4.0
}

impl Op for HairSettle {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.settle",
            summary: "Run a hair part's simulation: how far it sags at rest, how far a turn swings it and how long it \
                      takes to settle",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(0.0..=180.0).contains(&self.turn_deg) {
            return Err(OpError::invalid("turn_deg", format!("{} is out of range", self.turn_deg))
                .hint("use 0 to 180 degrees"));
        }
        if !(self.seconds > 0.0 && self.seconds <= MAX_SECONDS) {
            return Err(OpError::invalid("seconds", format!("{} is out of range", self.seconds))
                .hint(format!("use more than 0 and at most {MAX_SECONDS} seconds")));
        }
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut built = crate::ops::weights::build_character(&self.character, &found.def)?;
        let Some(slot) = built.runtimes.iter_mut().find(|s| s.part == self.part) else {
            return Ok(Plan::answer(json!({
                "character": self.character,
                "part": self.part,
                "simulated": false,
                "note": "no group of this hair is simulated; turn one on with hair.group.set_dynamics",
            })));
        };
        #[allow(clippy::cast_possible_truncation)] // checked ranges, well inside f32
        let (turn, seconds) = ((self.turn_deg as f32).to_radians(), self.seconds as f32);
        let report = settle(slot.runtime.as_mut(), turn, seconds)?;
        Ok(Plan::answer(json!({
            "character": self.character,
            "part": self.part,
            "simulated": true,
            "particles": slot.runtime.cost().particles,
            "rest": { "max_mm": mm(report.sag_max), "mean_mm": mm(report.sag_mean) },
            "turn_deg": self.turn_deg,
            "swing_mm": mm(report.swing),
            "settle_s": report.settle.map(|s| (f64::from(s) * 1000.0).round() / 1000.0),
            "seconds": self.seconds,
        })))
    }
}

register_op!(HairSettle);

/// Metres as millimetres to a tenth.
fn mm(m: f32) -> f64 {
    (f64::from(m) * 1e4).round() / 10.0
}

/// What [`settle`] measured.
struct Settled {
    /// The largest and mean distance of the hanging hair from its styled shape.
    sag_max: f32,
    sag_mean: f32,
    /// The largest distance from the turned rest after the turn.
    swing: f32,
    /// Seconds after the turn until the hair stays within [`SETTLED`]; `None` if it never does.
    settle: Option<f32>,
}

/// Every joint turned by `yaw` about the vertical through the origin: the character turning on the spot.
struct Turned(f32);

impl PoseSource for Turned {
    fn skin_matrix(&self, _: &JointRef) -> Option<Mat4> {
        Some(Mat4::from_rotation_y(self.0))
    }

    fn root(&self) -> Mat4 {
        Mat4::IDENTITY
    }
}

/// Hangs the hair of `rt` at rest, turns it by `turn` and watches it for `seconds`.
fn settle(rt: &mut dyn omuzaki_char_api::PartRuntime, turn: f32, seconds: f32) -> Result<Settled, OpError> {
    let positions = |rt: &dyn omuzaki_char_api::PartRuntime| {
        rt.dynamic_mesh(0)
            .map(|m| m.positions.to_vec())
            .ok_or_else(|| OpError::internal("a simulated hair has a dynamic mesh"))
    };
    let styled = positions(rt)?;
    let mut frame = 0u32;
    let mut step = |rt: &mut dyn omuzaki_char_api::PartRuntime, pose: &Turned| {
        rt.step(&StepCtx::new(FRAME, Seconds(f64::from(frame) * f64::from(FRAME)), pose, Vec3::ZERO));
        frame += 1;
    };
    let still = Turned(0.0);
    for _ in 0..frames(REST_S) {
        step(rt, &still);
    }
    let hanging = positions(rt)?;
    let gaps: Vec<f32> = styled.iter().zip(&hanging).map(|(a, b)| a.distance(*b)).collect();
    let sag_max = gaps.iter().copied().fold(0.0, f32::max);
    #[allow(clippy::cast_precision_loss)] // a vertex count
    let sag_mean = gaps.iter().sum::<f32>() / gaps.len().max(1) as f32;
    let rotation = Mat4::from_rotation_y(turn);
    let rest: Vec<Vec3> = hanging.iter().map(|p| rotation.transform_point3(*p)).collect();
    let turning = frames(TURN_S);
    for k in 1..=turning {
        #[allow(clippy::cast_precision_loss)] // a few frames
        let u = k as f32 / turning as f32;
        step(rt, &Turned(turn * u * u * (3.0 - 2.0 * u)));
    }
    let (mut swing, mut last_out) = (0.0f32, None);
    let watched = frames(seconds);
    let turned = Turned(turn);
    for k in 1..=watched {
        step(rt, &turned);
        let now = positions(rt)?;
        let gap = now.iter().zip(&rest).map(|(a, b)| a.distance(*b)).fold(0.0, f32::max);
        swing = swing.max(gap);
        if gap > SETTLED {
            last_out = Some(k);
        }
    }
    #[allow(clippy::cast_precision_loss)] // frame counts
    let settle = match last_out {
        Some(k) if k == watched => None,
        Some(k) => Some(k as f32 * FRAME),
        None => Some(0.0),
    };
    Ok(Settled { sag_max, sag_mean, swing, settle })
}

/// Whole frames in `seconds`.
fn frames(seconds: f32) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few seconds of frames
    let n = (seconds / FRAME).round() as u32;
    n
}
