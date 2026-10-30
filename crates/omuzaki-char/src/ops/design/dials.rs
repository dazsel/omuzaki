//! The named shape dials (#2612): `shape.list_dials`, `shape.set_dial`, `shape.randomise` and `shape.reset`.
//!
//! A dial is a regional control of the body's or the head's shape (cheek fullness, jaw width, calf size), placed by the
//! body's regions and the face's landmarks so it finds the same place on every template and profile. Its value is
//! stored in the character's design layer; the age adds its own value on top (`character.set_age`).

use omuzaki_char_api::CharId;
use omuzaki_char_api::design::{DialDef, DialSetting, DialSide};
use omuzaki_core::rng::Rng;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{dial, dials, load, store};
use crate::ops::character::access;

/// The dial's entry as the list shows it, with its setting in `setting` if there is one.
fn listed(d: &DialDef, setting: Option<&DialSetting>) -> Value {
    let mut entry = json!({
        "id": d.id, "name": d.name, "summary": d.summary, "group": d.group, "sided": d.sided,
        "range": [d.range.0, d.range.1], "prior": d.prior, "ages": !d.age.is_empty(),
    });
    if let (Some(s), Value::Object(fields)) = (setting, &mut entry) {
        fields.insert("setting".to_owned(), json!(s));
    }
    entry
}

/// Lists the shape dials: id, name, what `+1` and `-1` do, group, range, prior and whether age moves it; with a
/// character, its settings too. Writes nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {},
        {"group": "face"},
        {"character": "kit", "group": "legs"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct ShapeListDials {
    /// Only this group's dials (`face`, `head`, `neck`, `torso`, `arms`, `legs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// A character whose settings are shown beside each dial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<CharId>,
}

impl Op for ShapeListDials {
    fn meta() -> OpMeta {
        OpMeta {
            name: "shape.list_dials",
            summary: "List the named shape dials (face, head and body), with each one's range and, for a character, its setting.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let design = match &self.character {
            Some(id) => Some(load(ctx, id)?.1),
            None => None,
        };
        let all = dials()?;
        if let Some(group) = &self.group
            && !all.iter().any(|d| &d.group == group)
        {
            let mut groups: Vec<&str> = all.iter().map(|d| d.group.as_str()).collect();
            groups.dedup();
            groups.sort_unstable();
            groups.dedup();
            return Err(access::not_found("group", format!("there is no dial group `{group}`"))
                .hint(format!("the groups are {}", access::list(groups))));
        }
        let list: Vec<Value> = all
            .iter()
            .filter(|d| self.group.as_ref().is_none_or(|g| &d.group == g))
            .map(|d| listed(d, design.as_ref().and_then(|x| x.dials.get(&d.id))))
            .collect();
        let mut out = json!({ "count": list.len(), "dials": list });
        if let (Some(design), Value::Object(fields)) = (&design, &mut out) {
            fields.insert("age".to_owned(), json!(design.age));
        }
        Ok(Plan::answer(out))
    }
}

register_op!(ShapeListDials);

/// Which side `shape.set_dial` sets.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetSide {
    /// Both sides alike (the offsets are cleared).
    #[default]
    Both,
    /// Only the character's left (+x): the right keeps its value.
    Left,
    /// Only the right.
    Right,
}

/// Sets one shape dial on a character: a value within the dial's range (`-1` to `1` for most), on both sides or, for
/// a sided dial, on one. `0` is the template's own shape; setting every side to `0` removes the dial from the design.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "dial": "jaw_width", "value": 0.6},
        {"character": "kit", "dial": "cheek_fullness", "value": -0.4, "side": "left"},
        {"character": "kit", "dial": "calf_size", "value": 0.5}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct ShapeSetDial {
    /// The character.
    pub character: CharId,
    /// The dial's id (`shape.list_dials` lists them).
    pub dial: String,
    /// The value, within the dial's range.
    pub value: f64,
    /// Which side (sided dials only): `both` (the default), `left` or `right`.
    #[serde(default, skip_serializing_if = "is_both")]
    pub side: SetSide,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
fn is_both(side: &SetSide) -> bool {
    *side == SetSide::Both
}

impl Op for ShapeSetDial {
    fn meta() -> OpMeta {
        OpMeta {
            name: "shape.set_dial",
            summary: "Set a named shape dial (cheek fullness, jaw width, calf size...) on a character, on both sides or one.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (_, mut design) = load(ctx, &self.character)?;
        let d = dial(&self.dial, "dial")?;
        let (lo, hi) = d.range;
        if !(self.value.is_finite() && (lo..=hi).contains(&self.value)) {
            return Err(OpError::invalid("value", format!("{} is outside `{}`'s range {lo} to {hi}", self.value, d.id))
                .hint(format!("give a value from {lo} to {hi}; 0 is the template's own shape")));
        }
        if self.side != SetSide::Both && !d.sided {
            return Err(OpError::invalid("side", format!("`{}` is one dial for both sides", d.id))
                .hint("leave `side` out (or set it to `both`)"));
        }
        let mut setting = design.dials.get(&d.id).copied().unwrap_or_default();
        match self.side {
            SetSide::Both => setting = DialSetting::new(self.value),
            // A side's value is `value + offset`: keep the other side where it is.
            SetSide::Left => setting.left = self.value - setting.value,
            SetSide::Right => setting.right = self.value - setting.value,
        }
        if setting.is_zero() {
            design.dials.remove(&d.id);
        } else {
            design.dials.insert(d.id.clone(), setting);
        }
        let changes = store(ctx, &self.character, &design)?;
        let summary = json!({
            "character": self.character, "dial": d.id, "setting": setting,
            "left": setting.on(Some(DialSide::Left)), "right": setting.on(Some(DialSide::Right)),
            "changed": !changes.is_empty(),
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(ShapeSetDial);

/// A standard normal sample (Box-Muller).
pub(super) fn normal(rng: &mut Rng) -> f64 {
    let u = f64::from(rng.next_f32()).max(1e-7);
    let v = f64::from(rng.next_f32());
    (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
}

/// Samples every dial (or a group's) from its prior, and the anatomy under them: a plausible new face or body from a
/// seed. `amount` scales the spread (`1`: each dial's prior; `0.5`: subtler); `asymmetry` adds a small, separate
/// offset on each side of the sided dials and structures, as real faces have. The same seed gives the same character.
/// Replaces the sampled dials' settings and the anatomy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "seed": 7},
        {"character": "kit", "seed": 3, "amount": 0.6, "asymmetry": 0.3, "group": "face"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct ShapeRandomise {
    /// The character.
    pub character: CharId,
    /// The seed: the same seed gives the same dials.
    pub seed: u64,
    /// How far from the template: `0` to `2`, `1` (the default) samples each dial at its prior.
    #[serde(default = "one")]
    #[schemars(range(min = 0.0, max = 2.0))]
    pub amount: f64,
    /// How different the two sides are: `0` (the default, symmetric) to `1`.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub asymmetry: f64,
    /// Only this group's dials (the anatomy is then left as it is).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Sample the anatomy under the dials too (`anatomy.randomise` with the same seed, amount and asymmetry), so the
    /// face and the body vary as a whole; `true` (the default) unless a `group` is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anatomy: Option<bool>,
}

fn one() -> f64 {
    1.0
}

/// How large a side's own offset is at `asymmetry: 1`, as a fraction of the dial's prior.
const SIDE_SPREAD: f64 = 0.35;

impl Op for ShapeRandomise {
    fn meta() -> OpMeta {
        OpMeta {
            name: "shape.randomise",
            summary: "Sample a plausible face and body from a seed: the anatomy, and every shape dial (or a group's) from its prior.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (_, mut design) = load(ctx, &self.character)?;
        if !(0.0..=2.0).contains(&self.amount) {
            return Err(OpError::invalid("amount", format!("{} is outside 0 to 2", self.amount)).hint("1 samples each dial at its prior"));
        }
        if !(0.0..=1.0).contains(&self.asymmetry) {
            return Err(OpError::invalid("asymmetry", format!("{} is outside 0 to 1", self.asymmetry)).hint("0 is symmetric"));
        }
        let picked: Vec<&DialDef> =
            dials()?.iter().filter(|d| self.group.as_ref().is_none_or(|g| &d.group == g)).collect();
        if picked.is_empty() {
            return Err(access::not_found("group", "no dial is in that group").hint("shape.list_dials shows the groups"));
        }
        let mut rng = Rng::new(self.seed);
        for d in &picked {
            // One stream per dial, so adding a dial to the library doesn't reshuffle the others.
            let mut r = rng.fork(d.id.as_str().bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)));
            let (lo, hi) = d.range;
            let value = (normal(&mut r) * d.prior * self.amount).clamp(lo, hi);
            let mut setting = DialSetting::new(value);
            if d.sided && self.asymmetry > 0.0 {
                let spread = d.prior * SIDE_SPREAD * self.asymmetry * self.amount.max(0.5);
                setting.left = (normal(&mut r) * spread).clamp(lo - value, hi - value);
                setting.right = (normal(&mut r) * spread).clamp(lo - value, hi - value);
            }
            design.dials.insert(d.id.clone(), setting);
        }
        let anatomy = if self.anatomy.unwrap_or(self.group.is_none()) {
            super::anatomy::sample(&mut design.anatomy, self.seed, self.amount, self.asymmetry)?
        } else {
            0
        };
        let changes = store(ctx, &self.character, &design)?;
        let summary =
            json!({ "character": self.character, "seed": self.seed, "dials": picked.len(), "structures": anatomy });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(ShapeRandomise);

/// Puts the shape dials back to the template's shape: every dial, a group's, or one. The age and the sculpt strokes stay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "group": "face"},
        {"character": "kit", "dial": "jaw_width"}
    ],
    "x-omuzaki-fixture" = "character-design"
))]
pub struct ShapeReset {
    /// The character.
    pub character: CharId,
    /// Only this group's dials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Only this dial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dial: Option<String>,
}

impl Op for ShapeReset {
    fn meta() -> OpMeta {
        OpMeta {
            name: "shape.reset",
            summary: "Reset a character's shape dials to the template's shape: all, a group's, or one.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (_, mut design) = load(ctx, &self.character)?;
        let only = self.dial.as_deref().map(|id| dial(id, "dial")).transpose()?;
        let all = dials()?;
        let before = design.dials.len();
        design.dials.retain(|id, _| {
            let def = all.iter().find(|d| &d.id == id);
            let in_group = self.group.as_ref().is_none_or(|g| def.is_some_and(|d| &d.group == g));
            let is_it = only.is_none_or(|d| &d.id == id);
            !(in_group && is_it)
        });
        let reset = before - design.dials.len();
        let changes = store(ctx, &self.character, &design)?;
        let summary = json!({ "character": self.character, "reset": reset, "dials": design.dials.len() });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(ShapeReset);
