//! The shared character-design layer (`docs/quality/character-design.md`, levels 2, 3 and 7): what every part kind
//! gets for free, on top of its own generator, so a person or an agent can shape and surface any character with the
//! same few tools.
//!
//! `characters/<id>/design` is a [`DesignDef`]:
//! - **dials** ([`dial`]): named regional shape controls for the head and body (jaw width, cheek fullness, calf size),
//!   each with a range, a prior for sampling, and a left and a right side for asymmetry (#2612);
//! - **sculpt** ([`sculpt`]): brush strokes on any part (grab, inflate, flatten, crease, pinch, smooth), stored in the
//!   part's own coordinates so they re-apply after any slider or template change (#2613);
//! - **layers** ([`layer`]): a material layer stack on every part (tint, detail, wear, wetness, makeup, paint, polish,
//!   decals, greying, cavity), each layer a parameterised generator with masks (#2609, #2610);
//! - **age** ([`age`]): one number that drives the dials, the skin layers and the hair coherently (#2615).
//!
//! # How it builds
//!
//! The generators never see this layer. [`build_part`](crate::build_part) builds a part as before, scales it to the
//! stature, and then [`deform::apply`] moves its vertices by one **shape field**: the dials (and the age's dials) for
//! the body and the head, and the part's own strokes for any part. A field is a smooth function of a vertex's
//! position and of which body region it belongs to (read from its skin weights), so meshes that meet at a seam (the
//! neck's ring on the body and on the head) move together. Normals follow the field's Jacobian, never a recompute, so
//! hard edges stay hard.
//!
//! What a deformed part publishes is deformed with it: the body's fit (so garments wrap the new calf), the head's
//! surface (so hair sits on the new cranium) and every anchor (so ears and hats ride along). Parts that build later see
//! only that, so nothing downstream knows a dial exists.
//!
//! The layer stack runs in the [`Assembler`](crate::Assembler), after a part is built, like the weight brushes: it
//! writes vertex colours (and the skin program's `skin-info` attribute) and the colour parameters of program
//! materials, so a palette swap or an age change re-derives every layer instead of repainting it.
//!
//! # Coordinates that survive a rebuild
//!
//! Everything a person places (a stroke, a dab of paint, a decal) is stored as a [`PartPoint`]: on the body, a region,
//! a fraction along it and an angle round it; on the head, a direction in the head's own proportions; on any other
//! part, a point in its bounding box. Sizes are fractions of the local scale there (a limb's girth, the head's radius,
//! the part's size). So a stroke on the calf stays on the calf of a taller template, and a brow-ridge stroke stays on
//! the brow of a rounder head.

pub mod age;
pub mod deform;
pub mod dial;
pub mod frame;
pub mod layer;
pub mod library;
pub mod locate;
pub mod sculpt;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{DialId, PartId};

pub use age::{AGE_RANGE, AgeLayers, AgeModel};
pub use deform::{BodyFrameFact, HeadFrameFact};
pub use dial::{DialDef, DialEffect, DialSetting, DialSide, Lobes, Push};
pub use frame::{BodyFrame, HeadFrame, Landmark, PartFrame, RegionGroup};
pub use layer::{Blend, DecalShape, LayerKind, MakeupZone, Mask, MaskKind, MaterialLayer};
pub use library::{DesignLibrary, DesignSource, design_library, find_dial};
pub use locate::locate;
pub use sculpt::{Brush, PartPoint, SculptStroke};

/// `characters/<id>/design`: the character's dials, sculpt strokes, material layers and age. Every field is optional,
/// so the empty value is the generators' character exactly (bit for bit: nothing runs).
///
/// `#[non_exhaustive]`: other crates start from [`DesignDef::default`] and assign the public fields.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct DesignDef {
    /// The character's age in years, within [`AGE_RANGE`]; `None` leaves the character as its parts build it (an
    /// ageless design). Applied through the age model of the [`design_library`]: its dials, its skin layers and the
    /// hair's greying.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<f64>,
    /// Dial settings by dial id. A dial that is not listed is at 0.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub dials: BTreeMap<DialId, DialSetting>,
    /// Sculpt strokes by part, in the order they were made.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub sculpt: BTreeMap<PartId, Vec<SculptStroke>>,
    /// Material layers by part, bottom first.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub layers: BTreeMap<PartId, Vec<MaterialLayer>>,
}

impl DesignDef {
    /// True when it sets nothing: building ignores it.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// True when it moves any vertex of the body and the head: a dial off zero, or an age.
    pub fn shapes_body(&self) -> bool {
        self.age.is_some() || self.dials.values().any(|d| !d.is_zero())
    }

    /// The strokes of `part`, oldest first.
    pub fn strokes(&self, part: &PartId) -> &[SculptStroke] {
        self.sculpt.get(part).map_or(&[], Vec::as_slice)
    }

    /// The layers of `part`, bottom first.
    pub fn layers_of(&self, part: &PartId) -> &[MaterialLayer] {
        self.layers.get(part).map_or(&[], Vec::as_slice)
    }

    /// Checks what can be checked without a build: the age's range, every dial's id against the library and its
    /// values against the dial's range and sides, every stroke's and layer's numbers.
    ///
    /// # Errors
    /// A message naming the first field that is wrong, as a path under `design` (`dials/jaw_width/left`).
    pub fn check(&self) -> Result<(), String> {
        if let Some(age) = self.age
            && !(AGE_RANGE.0..=AGE_RANGE.1).contains(&age)
        {
            return Err(format!("age: {age} is outside {} to {} years", AGE_RANGE.0, AGE_RANGE.1));
        }
        for (id, setting) in &self.dials {
            let def = find_dial(id.as_str()).map_err(|e| format!("dials/{id}: {e}"))?;
            setting.check(&def).map_err(|e| format!("dials/{id}/{e}"))?;
        }
        for (part, strokes) in &self.sculpt {
            for (i, stroke) in strokes.iter().enumerate() {
                stroke.check().map_err(|e| format!("sculpt/{part}/{i}/{e}"))?;
            }
        }
        for (part, layers) in &self.layers {
            let mut ids = std::collections::BTreeSet::new();
            for layer in layers {
                if !ids.insert(&layer.id) {
                    return Err(format!("layers/{part}: two layers are called `{}`", layer.id));
                }
                layer.check().map_err(|e| format!("layers/{part}/{}/{e}", layer.id))?;
            }
        }
        Ok(())
    }
}

/// A bump that is 1 at `x = 0`, falls smoothly to 0 at `|x| = 1` and stays 0 past it, with zero slope at both ends
/// (`(1 - x²)³`): the falloff of every dial and brush, so no edit leaves a crease at its edge.
pub(crate) fn bump(x: f32) -> f32 {
    let q = 1.0 - x * x;
    if q <= 0.0 { 0.0 } else { q * q * q }
}

/// Hermite smoothstep from `e0` to `e1` (`e0 < e1`).
pub(crate) fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bump_is_one_at_its_centre_and_zero_with_zero_slope_at_its_edge() {
        assert!((bump(0.0) - 1.0).abs() < 1e-6);
        assert!(bump(1.0).abs() < 1e-6);
        assert!(bump(1.5).abs() < 1e-6);
        let slope = (bump(1.0) - bump(0.999)) / 0.001;
        assert!(slope.abs() < 1e-3, "{slope}");
    }

    #[test]
    fn an_empty_design_is_empty_and_shapes_nothing() {
        let design = DesignDef::default();
        assert!(design.is_empty());
        assert!(!design.shapes_body());
        assert!(design.check().is_ok());
    }

    #[test]
    fn an_age_outside_the_range_is_refused() {
        let design = DesignDef { age: Some(400.0), ..DesignDef::default() };
        assert!(design.check().unwrap_err().starts_with("age:"));
    }
}
