//! Age as one control (#2615): a number of years that moves every part coherently.
//!
//! Age reaches a character three ways, all derived at build time so changing it re-derives everything:
//! 1. **shape**: through the anatomy ([`StructureDef::age`](super::StructureDef::age), #2614): a child's full cheeks,
//!    small jaw and nose and round forehead; then the fat pads deflating and sliding down (the malar fat toward the
//!    jowl), the orbit widening and the maxilla and the jaw's angle receding as bone resorbs, the nose's cartilage
//!    lengthening, muscle thinning late, and the skin's folds (the nasolabial fold, marionette lines, crow's feet,
//!    forehead lines) cut into the surface as real grooves. A dial may carry an age curve of its own
//!    ([`DialDef::age`](super::DialDef::age)) where no structure stands under it;
//! 2. **skin**: the age model's curves ([`AgeModel`]) set implicit layers on the skin parts: wrinkle cavities in the
//!    folds a face makes (crow's feet, the nasolabial fold, the forehead), age spots, and a child's flush;
//! 3. **hair**: greying, as an implicit layer on the hair parts, which takes the hair's own colours toward silver.
//!
//! The person's own dials and layers are added on top, so an old character can still have full cheeks. A stylised
//! template keeps its proportions: age moves the features *within* the style, as an artist ages a chibi.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The ages a character may be given, in years.
pub const AGE_RANGE: (f64, f64) = (3.0, 100.0);

/// How strongly each age effect applies at one age, `0..=1` each.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct AgeLayers {
    /// Hair greying.
    pub grey: f32,
    /// Wrinkle cavities on the skin.
    pub wrinkles: f32,
    /// Age spots.
    pub spots: f32,
    /// A child's flush on the cheeks, nose and ears.
    pub flush: f32,
}

impl AgeLayers {
    /// True when nothing applies.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The age model's curves: `[age, strength]` points in age order for each effect, linear between and constant past
/// the ends.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AgeModel {
    /// Hair greying.
    pub grey: Vec<[f64; 2]>,
    /// Wrinkle cavities.
    pub wrinkles: Vec<[f64; 2]>,
    /// Age spots.
    pub spots: Vec<[f64; 2]>,
    /// A child's flush.
    pub flush: Vec<[f64; 2]>,
}

/// The value of a piecewise-linear curve at `x` (0 for an empty curve).
pub(crate) fn curve_at(curve: &[[f64; 2]], x: f64) -> f64 {
    match (curve.first(), curve.last()) {
        (None, _) | (_, None) => 0.0,
        (Some(first), _) if x <= first[0] => first[1],
        (_, Some(last)) if x >= last[0] => last[1],
        _ => curve.windows(2).find(|w| x >= w[0][0] && x <= w[1][0]).map_or(0.0, |w| {
            let s = (x - w[0][0]) / (w[1][0] - w[0][0]).max(1e-9);
            w[0][1] + (w[1][1] - w[0][1]) * s
        }),
    }
}

impl AgeModel {
    /// True when the model has no curves.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The effects at `age` years.
    #[allow(clippy::cast_possible_truncation)] // strengths are 0..=1
    pub fn at(&self, age: f64) -> AgeLayers {
        let f = |c: &[[f64; 2]]| curve_at(c, age).clamp(0.0, 1.0) as f32;
        AgeLayers { grey: f(&self.grey), wrinkles: f(&self.wrinkles), spots: f(&self.spots), flush: f(&self.flush) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_reads_its_curves_and_holds_past_their_ends() {
        let model = AgeModel { grey: vec![[40.0, 0.0], [80.0, 1.0]], ..AgeModel::default() };
        assert!((model.at(60.0).grey - 0.5).abs() < 1e-6);
        assert!(model.at(20.0).grey.abs() < 1e-6);
        assert!((model.at(95.0).grey - 1.0).abs() < 1e-6);
        assert!(model.at(60.0).wrinkles.abs() < 1e-6);
    }
}
