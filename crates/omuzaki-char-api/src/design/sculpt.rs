//! Sculpt strokes (#2613): brushes that push any part's surface, stored in the part's own coordinates so they survive
//! a rebuild.
//!
//! A stroke is one dab of a brush at a [`PartPoint`], with a radius and a strength that are fractions of the local
//! scale there (a limb's girth, the head's radius, the part's size), so the same stroke on a taller template, a broader
//! body or a rounder head lands on the same place at the same relative size. When a part builds, each point is placed
//! on its new surface (the nearest vertex of the part to where the point falls), so a stroke never floats off the
//! surface after a change.
//!
//! | Brush | What it does | `strength` |
//! |---|---|---|
//! | `grab` | moves the surface along `direction` | displacement, a fraction of the local scale |
//! | `inflate` | pushes it out along its normal (in, if negative) | displacement |
//! | `crease` | cuts a narrow groove (a ridge, if negative) | displacement |
//! | `flatten` | pulls it toward the plane through the point | `0..=1` |
//! | `pinch` | draws it in toward the point along the surface, sharpening a form | `0..=1` |
//! | `smooth` | relaxes the strokes before it (never the generator's own surface, which its parameters own) | `0..=1` |

use omuzaki_core::glam::Vec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::frame::RegionGroup;

/// A brush.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Brush {
    /// Moves the surface along the stroke's direction.
    Grab,
    /// Pushes the surface along its normal.
    Inflate,
    /// Cuts a narrow groove.
    Crease,
    /// Pulls the surface toward the plane through the point.
    Flatten,
    /// Draws the surface in toward the point.
    Pinch,
    /// Relaxes the earlier strokes.
    Smooth,
}

impl Brush {
    /// Every brush.
    pub const ALL: [Self; 6] = [Self::Grab, Self::Inflate, Self::Crease, Self::Flatten, Self::Pinch, Self::Smooth];

    /// Whether `strength` is a `0..=1` fraction (else a signed displacement).
    pub fn is_fractional(self) -> bool {
        matches!(self, Self::Flatten | Self::Pinch | Self::Smooth)
    }

    /// The brush's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grab => "grab",
            Self::Inflate => "inflate",
            Self::Crease => "crease",
            Self::Flatten => "flatten",
            Self::Pinch => "pinch",
            Self::Smooth => "smooth",
        }
    }
}

/// A point on a part, in coordinates that survive a rebuild.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "on", rename_all = "snake_case", deny_unknown_fields)]
pub enum PartPoint {
    /// On a body region: the fraction along it, the angle round it and how far out.
    Body {
        /// The region.
        region: RegionGroup,
        /// The fraction along it, `0..=1` from its start.
        t: f32,
        /// Radians round the section from the front, turning toward the region's `across` (world +x).
        angle: f32,
        /// How far out, as a fraction of the section's radius that way (`1`: on its surface).
        #[serde(default = "one")]
        lift: f32,
    },
    /// On the head: a direction from its centre in the head's own proportions (x left, y up, z forward).
    Head {
        /// The direction (any length; it is normalised).
        dir: [f32; 3],
        /// How far out, as a fraction of the head's radius that way (`1`: on its surface).
        #[serde(default = "one")]
        lift: f32,
    },
    /// In the part's bounding box, before the design layer moved it.
    Box {
        /// `0..=1` on each axis.
        at: [f32; 3],
    },
}

fn one() -> f32 {
    1.0
}

impl PartPoint {
    /// Checks the numbers: finite, `t` and box coordinates within `0..=1`, a lift above 0, a direction that is not 0.
    ///
    /// # Errors
    /// The field that is wrong.
    pub fn check(&self) -> Result<(), String> {
        let unit = |name: &str, v: f32| {
            if (0.0..=1.0).contains(&v) { Ok(()) } else { Err(format!("{name}: {v} is outside 0 to 1")) }
        };
        let lift = |v: f32| if v > 0.0 && v < 4.0 { Ok(()) } else { Err(format!("lift: {v} is outside 0 to 4")) };
        match self {
            Self::Body { t, angle, lift: l, .. } => {
                unit("t", *t)?;
                if !angle.is_finite() {
                    return Err("angle: not a number".to_owned());
                }
                lift(*l)
            }
            Self::Head { dir, lift: l } => {
                if !Vec3::from(*dir).is_finite() || Vec3::from(*dir).length() < 1e-6 {
                    return Err("dir: needs a direction".to_owned());
                }
                lift(*l)
            }
            Self::Box { at } => at.iter().try_for_each(|v| unit("at", *v)),
        }
    }
}

/// One stroke of a brush.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SculptStroke {
    /// The brush.
    pub brush: Brush,
    /// Where.
    pub at: PartPoint,
    /// The brush's radius, as a fraction of the local scale (a region's mean radius, the head's mean radius or the
    /// part's diagonal). `> 0`, at most [`SculptStroke::MAX_RADIUS`].
    pub radius: f32,
    /// How hard (see the [module](self) table).
    pub strength: f32,
    /// For `grab`: the direction to move, in bind space (x left, y up, z forward); normalised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<[f32; 3]>,
    /// Mirror the stroke onto the other side as well (x negated).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub symmetric: bool,
}

impl SculptStroke {
    /// The largest radius, as a fraction of the local scale.
    pub const MAX_RADIUS: f32 = 4.0;
    /// The largest displacement strength, as a fraction of the local scale.
    pub const MAX_STRENGTH: f32 = 1.0;

    /// Checks the stroke.
    ///
    /// # Errors
    /// The field that is wrong.
    pub fn check(&self) -> Result<(), String> {
        self.at.check().map_err(|e| format!("at/{e}"))?;
        if !(self.radius > 0.0 && self.radius <= Self::MAX_RADIUS) {
            return Err(format!("radius: {} is outside 0 to {}", self.radius, Self::MAX_RADIUS));
        }
        let ok = if self.brush.is_fractional() {
            (0.0..=1.0).contains(&self.strength)
        } else {
            self.strength.abs() <= Self::MAX_STRENGTH
        };
        if !ok {
            let range =
                if self.brush.is_fractional() { "0 to 1".to_owned() } else { format!("±{}", Self::MAX_STRENGTH) };
            return Err(format!("strength: {} is outside {range} for {}", self.strength, self.brush.as_str()));
        }
        match (self.brush, self.direction) {
            (Brush::Grab, None) => Err("direction: grab needs a direction".to_owned()),
            (Brush::Grab, Some(d)) if !(Vec3::from(d).length() > 1e-6) => Err("direction: zero".to_owned()),
            _ => Ok(()),
        }
    }

    /// The same stroke on the other side of the body: a body region is swapped for its mirror (the angle turns the
    /// other way), a head or box point has its x mirrored, and so does the direction.
    pub fn mirrored(&self) -> Self {
        let at = match self.at {
            PartPoint::Body { region, t, angle, lift } => {
                let mirrored = region.mirrored();
                // Round a limb, `across` is +x on both sides, so the angle mirrors to its negative.
                PartPoint::Body { region: mirrored, t, angle: -angle, lift }
            }
            PartPoint::Head { dir, lift } => PartPoint::Head { dir: [-dir[0], dir[1], dir[2]], lift },
            PartPoint::Box { at } => PartPoint::Box { at: [1.0 - at[0], at[1], at[2]] },
        };
        Self { at, direction: self.direction.map(|d| [-d[0], d[1], d[2]]), symmetric: false, ..self.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(brush: Brush, strength: f32) -> SculptStroke {
        SculptStroke {
            brush,
            at: PartPoint::Head { dir: [0.4, -0.2, 1.0], lift: 1.0 },
            radius: 0.3,
            strength,
            direction: None,
            symmetric: false,
        }
    }

    #[test]
    fn strengths_are_checked_by_brush() {
        assert!(stroke(Brush::Inflate, -0.2).check().is_ok());
        assert!(stroke(Brush::Flatten, -0.2).check().unwrap_err().starts_with("strength"));
        assert!(stroke(Brush::Grab, 0.1).check().unwrap_err().starts_with("direction"));
        let grab = SculptStroke { direction: Some([0.0, 1.0, 0.0]), ..stroke(Brush::Grab, 0.1) };
        assert!(grab.check().is_ok());
    }

    #[test]
    fn a_mirrored_stroke_lands_on_the_other_side() {
        let s = SculptStroke {
            at: PartPoint::Body { region: RegionGroup::LegLeft, t: 0.6, angle: 2.5, lift: 1.0 },
            direction: Some([1.0, 0.0, 0.0]),
            ..stroke(Brush::Grab, 0.1)
        };
        let m = s.mirrored();
        assert_eq!(m.at, PartPoint::Body { region: RegionGroup::LegRight, t: 0.6, angle: -2.5, lift: 1.0 });
        assert_eq!(m.direction, Some([-1.0, 0.0, 0.0]));
        assert_eq!(stroke(Brush::Inflate, 0.1).mirrored().at, PartPoint::Head { dir: [-0.4, -0.2, 1.0], lift: 1.0 });
    }
}
