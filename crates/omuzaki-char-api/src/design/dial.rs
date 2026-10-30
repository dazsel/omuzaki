//! Named shape dials (#2612): a library of regional controls for the head and the body, each one data.
//!
//! A dial is a set of [`DialEffect`]s, each a smooth bump of displacement:
//! - on the **body**, a radial swelling of one region round a fraction along it (`t`, `span`), shaped round the
//!   section by [`Lobes`] (front, back, outward, inward, up, down): `calf_size` swells the back of the lower leg;
//! - on the **head**, a bump round a [`Landmark`] of the face or a direction in the head's own proportions, pushing
//!   outward from the centre or along a direction: `jaw_width` pushes the jaw's angles apart.
//!
//! Because every effect is written in the frames of [`frame`](super::frame) (a fraction of a region, a landmark read
//! from the face layout), one definition serves every template, profile and body shape.
//!
//! **Sides.** A `sided` dial is written for the character's left (+x); its right side is the mirror. Its setting has a
//! `value` for both sides and a `left` and a `right` offset, so a face can be asymmetric (one eye a little lower,
//! one cheek fuller) without a second dial.
//!
//! **Priors.** Each dial has a `prior`: the spread (one standard deviation) of plausible values, used when a character
//! is sampled at random, so a random face stays a face.

use omuzaki_core::glam::Vec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::frame::{Landmark, RegionGroup};
use crate::ids::DialId;

/// Which side of a sided dial a value is for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DialSide {
    /// The character's left (+x).
    Left,
    /// The character's right.
    Right,
}

/// A dial's setting in a character: `value` on both sides, plus a per-side offset on sided dials.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DialSetting {
    /// The value on both sides, within the dial's range (`-1..=1` unless the dial says otherwise).
    pub value: f64,
    /// Added on the left side (sided dials only).
    #[serde(skip_serializing_if = "is_zero")]
    pub left: f64,
    /// Added on the right side (sided dials only).
    #[serde(skip_serializing_if = "is_zero")]
    pub right: f64,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

impl DialSetting {
    /// The same value on both sides.
    pub fn new(value: f64) -> Self {
        Self { value, left: 0.0, right: 0.0 }
    }

    /// True when the dial does nothing.
    #[allow(clippy::float_cmp)] // exactly zero is "unset"
    pub fn is_zero(&self) -> bool {
        self.value == 0.0 && self.left == 0.0 && self.right == 0.0
    }

    /// The value on `side` (`None`: an unsided dial's one value).
    pub fn on(&self, side: Option<DialSide>) -> f64 {
        match side {
            None => self.value,
            Some(DialSide::Left) => self.value + self.left,
            Some(DialSide::Right) => self.value + self.right,
        }
    }

    /// Checks the setting against its dial: every side within the range, offsets only on sided dials.
    ///
    /// # Errors
    /// The field that is wrong and why (`left: ...`).
    pub fn check(&self, dial: &DialDef) -> Result<(), String> {
        self.check_in(dial.range, dial.sided, dial.id.as_str())
    }

    /// [`check`](Self::check) against a range and sidedness given directly (an anatomy structure's, say); `id` names
    /// what it sets in the message.
    ///
    /// # Errors
    /// The field that is wrong and why (`left: ...`).
    pub fn check_in(&self, range: (f64, f64), sided: bool, id: &str) -> Result<(), String> {
        let (lo, hi) = range;
        let within = |name: &str, v: f64| {
            if v.is_finite() && (lo..=hi).contains(&v) {
                Ok(())
            } else {
                Err(format!("{name}: {v} is outside the range {lo} to {hi}"))
            }
        };
        within("value", self.value)?;
        if sided {
            within("left", self.on(Some(DialSide::Left)))?;
            within("right", self.on(Some(DialSide::Right)))?;
        } else if !self.is_unsided() {
            return Err(format!("left: `{id}` is the same on both sides, so it has no left or right offset"));
        }
        Ok(())
    }

    #[allow(clippy::float_cmp)] // exactly zero is "unset"
    fn is_unsided(&self) -> bool {
        self.left == 0.0 && self.right == 0.0
    }
}

/// How a body effect is shaped round a region's section: the weight of each direction, read where a vertex lies round
/// the section. `out` and `in` are away from and toward the body's midline (on the torso and the neck, both sides).
/// A weight is `w · max(0, r·d)²` for the in-plane direction `r` of the vertex and the lobe's direction `d`, plus `all`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Lobes {
    /// The same all round.
    pub all: f32,
    /// Toward +z.
    pub front: f32,
    /// Toward -z.
    pub back: f32,
    /// Away from the midline.
    pub out: f32,
    /// Toward the midline.
    #[serde(rename = "in")]
    pub inward: f32,
    /// Toward +y.
    pub up: f32,
    /// Toward -y.
    pub down: f32,
}

impl Lobes {
    /// Each lobe's world direction for a region on `side` (`+1` left, `-1` right, `0` the midline) and its weight.
    pub(crate) fn directions(&self, side: f32) -> [(Vec3, f32); 6] {
        let out = if side == 0.0 { Vec3::X } else { Vec3::X * side };
        [
            (Vec3::Z, self.front),
            (-Vec3::Z, self.back),
            (out, self.out),
            (-out, self.inward),
            (Vec3::Y, self.up),
            (-Vec3::Y, self.down),
        ]
    }
}

/// One bump of a dial.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "frame", rename_all = "snake_case", deny_unknown_fields)]
pub enum DialEffect {
    /// A radial swelling of a body region.
    Body {
        /// The region: for a sided dial, the left one (the right is mirrored).
        region: RegionGroup,
        /// Where along the region the swelling peaks, `0..=1` from its start (hips, shoulder, neck base).
        t: f32,
        /// How far either side of `t` it reaches, as a fraction of the region.
        span: f32,
        /// Its shape round the section.
        lobes: Lobes,
        /// The radius's change at value `+1`, as a fraction (0.15: 15% fuller).
        amount: f32,
        /// The change at value `-1`, if not the same as `amount`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        amount_down: Option<f32>,
    },
    /// A bump on the head.
    Head {
        /// The landmark it centres on: for a sided dial, the left one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        landmark: Option<Landmark>,
        /// Or a direction in the head's own proportions (x left, y up, z forward).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dir: Option<[f32; 3]>,
        /// Moves the aim from the landmark, in the head's proportions, before the bump is placed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nudge: Option<[f32; 3]>,
        /// How far round the head the bump reaches, degrees.
        reach: f32,
        /// How it pushes.
        #[serde(default)]
        push: Push,
        /// The displacement at value `+1`, as a fraction of the distance from the head's centre.
        amount: f32,
        /// At value `-1`, if not the same.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        amount_down: Option<f32>,
    },
}

/// How a head effect moves the surface.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Push {
    /// Out from the head's centre (a negative value draws it in).
    #[default]
    Radial,
    /// Along +x on the left (mirrored on the right): widens.
    Out,
    /// Along +y.
    Up,
    /// Along +z.
    Forward,
}

impl Push {
    /// The direction on `side` (`None` for radial).
    pub(crate) fn vector(self, side: f32) -> Option<Vec3> {
        match self {
            Self::Radial => None,
            Self::Out => Some(Vec3::X * if side == 0.0 { 1.0 } else { side }),
            Self::Up => Some(Vec3::Y),
            Self::Forward => Some(Vec3::Z),
        }
    }
}

/// A dial, as the library holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DialDef {
    /// The dial's id.
    pub id: DialId,
    /// What a picker shows.
    pub name: String,
    /// One line: what `+1` does, and `-1`.
    pub summary: String,
    /// Where a picker files it: `face`, `head`, `neck`, `torso`, `arms`, `legs`.
    pub group: String,
    /// Whether it has a left and a right side.
    #[serde(default)]
    pub sided: bool,
    /// The values it takes.
    #[serde(default = "default_range")]
    pub range: (f64, f64),
    /// One standard deviation of plausible values, for sampling.
    #[serde(default = "default_prior")]
    pub prior: f64,
    /// The value the age model gives it, as `[age, value]` points in age order (linear between, constant past the
    /// ends); empty: age doesn't move it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub age: Vec<[f64; 2]>,
    /// What it does.
    pub effects: Vec<DialEffect>,
}

fn default_range() -> (f64, f64) {
    (-1.0, 1.0)
}

fn default_prior() -> f64 {
    0.3
}

impl DialDef {
    /// The dial's value at `age` years from its age curve (0 if it has none).
    pub fn at_age(&self, age: f64) -> f64 {
        super::age::curve_at(&self.age, age)
    }

    /// Checks the definition: a range round 0, a positive prior, an age curve in order, effects with sane numbers,
    /// and body effects on a left region (or the midline) when sided.
    ///
    /// # Errors
    /// What is wrong, naming the dial.
    pub fn check(&self) -> Result<(), String> {
        let id = &self.id;
        if !(self.range.0 < 0.0 && self.range.1 > 0.0) {
            return Err(format!("dial `{id}`: its range must run from below 0 to above 0"));
        }
        if !(self.prior > 0.0 && self.prior.is_finite()) {
            return Err(format!("dial `{id}`: its prior must be positive"));
        }
        if self.age.windows(2).any(|w| w[1][0] <= w[0][0]) {
            return Err(format!("dial `{id}`: its age curve must run in age order"));
        }
        if self.effects.is_empty() {
            return Err(format!("dial `{id}`: it has no effects"));
        }
        for effect in &self.effects {
            match effect {
                DialEffect::Body { region, t, span, amount, .. } => {
                    if self.sided && region.side() < 0.0 {
                        return Err(format!("dial `{id}`: a sided dial is written for the left side"));
                    }
                    if region.region().is_none() {
                        return Err(format!("dial `{id}`: the {} has no body region", region.as_str()));
                    }
                    if !(0.0..=1.0).contains(t) || span.is_nan() || *span <= 0.0 || !amount.is_finite() {
                        return Err(format!("dial `{id}`: t must be 0 to 1, span positive and amount finite"));
                    }
                }
                DialEffect::Head { landmark, dir, reach, amount, .. } => {
                    if landmark.is_some() == dir.is_some() {
                        return Err(format!("dial `{id}`: a head effect has a landmark or a dir, not both"));
                    }
                    if !(0.0..=180.0).contains(reach) || *reach <= 0.0 || !amount.is_finite() {
                        return Err(format!("dial `{id}`: reach must be 0 to 180 degrees and amount finite"));
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dial(sided: bool) -> DialDef {
        DialDef {
            id: DialId::new("calf_size").unwrap(),
            name: "Calf".into(),
            summary: "s".into(),
            group: "legs".into(),
            sided,
            range: (-1.0, 1.0),
            prior: 0.3,
            age: vec![[10.0, -0.5], [30.0, 0.0], [70.0, 0.4]],
            effects: vec![DialEffect::Body {
                region: RegionGroup::LegLeft,
                t: 0.7,
                span: 0.2,
                lobes: Lobes { back: 1.0, ..Lobes::default() },
                amount: 0.15,
                amount_down: None,
            }],
        }
    }

    #[test]
    fn the_age_curve_interpolates_and_holds_past_its_ends() {
        let d = dial(true);
        assert!((d.at_age(5.0) + 0.5).abs() < 1e-9);
        assert!((d.at_age(20.0) + 0.25).abs() < 1e-9);
        assert!((d.at_age(50.0) - 0.2).abs() < 1e-9);
        assert!((d.at_age(90.0) - 0.4).abs() < 1e-9);
        assert!(d.check().is_ok());
    }

    #[test]
    fn a_setting_checks_its_sides_against_the_range() {
        let sided = dial(true);
        assert!(DialSetting { value: 0.5, left: 0.4, right: -0.2 }.check(&sided).is_ok());
        assert!(DialSetting { value: 0.8, left: 0.4, right: 0.0 }.check(&sided).unwrap_err().starts_with("left"));
        let unsided = dial(false);
        assert!(DialSetting { value: 0.2, left: 0.1, right: 0.0 }.check(&unsided).is_err());
        assert!(DialSetting::new(1.2).check(&unsided).unwrap_err().starts_with("value"));
    }

    #[test]
    fn a_sided_dial_is_written_for_the_left() {
        let mut d = dial(true);
        d.effects = vec![DialEffect::Body {
            region: RegionGroup::LegRight,
            t: 0.5,
            span: 0.2,
            lobes: Lobes::default(),
            amount: 0.1,
            amount_down: None,
        }];
        assert!(d.check().unwrap_err().contains("left side"));
    }
}
