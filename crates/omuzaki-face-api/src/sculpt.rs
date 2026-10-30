//! The sculpted 3D features: every number that shapes one nose, one mouth and one ear, in physical units (metres,
//! degrees, scales on the default) with defaults that build a believable adult feature and an `anime()` preset for each.
//! Each is stored on the face (`face/nose3d`, `face/lips3d`, `face/ear3d`, see [`FaceProfile`](crate::FaceProfile)), so
//! `face.set_style` edits any one field by path and `face.describe_style` lists them with their ranges;
//! `omuzaki_face::feature` sculpts each as a signed-distance field and meshes it.
//!
//! Every feature has a `style` from 0 (photoreal) to 1 (anime); between the two its forms blend, so one feature morphs
//! from one style to the other.

use omuzaki_core::colour::Colour;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::check::{Check, Validate};

/// Every number that shapes one nose, stored on the face as `nose3d`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct NoseDesign {
    /// 0 photoreal, 1 anime.
    pub style: f32,
    /// From between the eyes (the nasion) to where the nose meets the lip (the subnasale), metres.
    pub length: f32,
    /// How far the tip stands out of the face, metres.
    pub projection: f32,
    /// How far the bridge stands out between the eyes, metres.
    pub bridge: f32,
    /// The width across the wings, metres.
    pub width: f32,
    /// The radius of each of the tip's domes, metres.
    pub tip: f32,
    /// How far apart the tip's domes sit, as a share of their radius (0 one round tip, 1 a clearly split one).
    pub bifid: f32,
    /// How far the tip turns up, degrees (negative droops).
    pub upturn: f32,
    /// A bump on the bridge, metres (0 straight, negative a scooped bridge).
    pub hump: f32,
    /// The nostrils' size, a scale on the default.
    pub nostrils: f32,
    /// How full the wings are, a scale on the default.
    pub flare: f32,
    /// The skin's colour.
    pub tone: Colour,
}

impl Default for NoseDesign {
    fn default() -> Self {
        Self {
            style: 0.0,
            length: 0.046,
            projection: 0.021,
            bridge: 0.009,
            width: 0.033,
            tip: 0.0052,
            bifid: 0.7,
            upturn: 4.0,
            hump: 0.0006,
            nostrils: 1.0,
            flare: 1.0,
            tone: Colour::hex(0x009e_745e),
        }
    }
}

impl NoseDesign {
    /// A drawn anime nose: small, low and smooth, on fair skin.
    pub fn anime() -> Self {
        Self {
            style: 1.0,
            length: 0.036,
            projection: 0.009,
            bridge: 0.003,
            width: 0.022,
            tip: 0.0035,
            bifid: 0.0,
            upturn: 12.0,
            hump: 0.0,
            nostrils: 0.6,
            flare: 0.6,
            tone: Colour::hex(0x00f6_dccf),
        }
    }
}

/// Every number that shapes one mouth, stored on the face as `lips3d`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LipsDesign {
    /// 0 photoreal, 1 anime.
    pub style: f32,
    /// Corner to corner, metres.
    pub width: f32,
    /// The upper lip's red height at its middle, metres.
    pub upper: f32,
    /// The lower lip's red height at its middle, metres.
    pub lower: f32,
    /// How far the lips stand out, a scale on the default.
    pub fullness: f32,
    /// How deep the Cupid's bow dips between its peaks, a share of the upper lip (0 a straight line).
    pub bow: f32,
    /// How far the corners turn up, metres (negative turns them down).
    pub corners: f32,
    /// The philtrum's ridges, a scale on the default (0 smooth).
    pub philtrum: f32,
    /// The skin's colour.
    pub tone: Colour,
    /// The lips' own colour.
    pub colour: Colour,
}

impl Default for LipsDesign {
    fn default() -> Self {
        Self {
            style: 0.0,
            width: 0.05,
            upper: 0.0082,
            lower: 0.0098,
            fullness: 1.0,
            bow: 0.3,
            corners: 0.0,
            philtrum: 1.0,
            tone: Colour::hex(0x009e_745e),
            colour: Colour::hex(0x009c_5652),
        }
    }
}

impl LipsDesign {
    /// A drawn anime mouth: small and nearly flat on fair skin, its line inked.
    pub fn anime() -> Self {
        Self {
            style: 1.0,
            width: 0.016,
            upper: 0.002,
            lower: 0.003,
            fullness: 0.4,
            bow: 0.0,
            corners: 0.0005,
            philtrum: 0.0,
            tone: Colour::hex(0x00f6_dccf),
            colour: Colour::hex(0x00ec_b4b0),
        }
    }
}

/// Every number that shapes one ear, stored on the face as `ear3d`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EarDesign {
    /// 0 photoreal, 1 anime.
    pub style: f32,
    /// Top to the bottom of the lobe, metres.
    pub height: f32,
    /// Front to back, metres.
    pub width: f32,
    /// How far the ear stands out from the head at the back, degrees.
    pub protrusion: f32,
    /// The rim's roll, a scale on the default.
    pub helix: f32,
    /// The inner ridge, a scale on the default (0 smooth).
    pub antihelix: f32,
    /// The lobe, a scale on the default (0 attached and small).
    pub lobe: f32,
    /// The skin's colour.
    pub tone: Colour,
}

impl Default for EarDesign {
    fn default() -> Self {
        Self {
            style: 0.0,
            height: 0.062,
            width: 0.032,
            protrusion: 22.0,
            helix: 1.0,
            antihelix: 1.0,
            lobe: 1.0,
            tone: Colour::hex(0x009e_745e),
        }
    }
}

impl EarDesign {
    /// A drawn anime ear: a little smaller, one rim round a shallow bowl, on fair skin.
    pub fn anime() -> Self {
        Self {
            style: 1.0,
            height: 0.052,
            width: 0.028,
            protrusion: 16.0,
            helix: 1.1,
            antihelix: 0.3,
            lobe: 0.8,
            tone: Colour::hex(0x00f6_dccf),
        }
    }
}

impl Validate for NoseDesign {
    fn check(&self, c: &mut Check) {
        c.range_f32("style", self.style, 0.0, 1.0);
        c.range_f32("length", self.length, 0.025, 0.065);
        c.range_f32("projection", self.projection, 0.004, 0.035);
        c.range_f32("bridge", self.bridge, 0.0, 0.016);
        c.range_f32("width", self.width, 0.015, 0.045);
        c.range_f32("tip", self.tip, 0.002, 0.009);
        c.range_f32("bifid", self.bifid, 0.0, 1.0);
        c.range_f32("upturn", self.upturn, -20.0, 35.0);
        c.range_f32("hump", self.hump, -0.003, 0.004);
        c.range_f32("nostrils", self.nostrils, 0.2, 2.0);
        c.range_f32("flare", self.flare, 0.2, 2.0);
    }
}

impl Validate for LipsDesign {
    fn check(&self, c: &mut Check) {
        c.range_f32("style", self.style, 0.0, 1.0);
        c.range_f32("width", self.width, 0.01, 0.07);
        c.range_f32("upper", self.upper, 0.001, 0.016);
        c.range_f32("lower", self.lower, 0.001, 0.018);
        c.range_f32("fullness", self.fullness, 0.2, 2.0);
        c.range_f32("bow", self.bow, 0.0, 1.0);
        c.range_f32("corners", self.corners, -0.004, 0.004);
        c.range_f32("philtrum", self.philtrum, 0.0, 2.0);
    }
}

impl Validate for EarDesign {
    fn check(&self, c: &mut Check) {
        c.range_f32("style", self.style, 0.0, 1.0);
        c.range_f32("height", self.height, 0.035, 0.08);
        c.range_f32("width", self.width, 0.018, 0.045);
        c.range_f32("protrusion", self.protrusion, 0.0, 45.0);
        c.range_f32("helix", self.helix, 0.2, 2.0);
        c.range_f32("antihelix", self.antihelix, 0.0, 2.0);
        c.range_f32("lobe", self.lobe, 0.2, 2.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_and_the_anime_presets_are_in_range() {
        assert!(NoseDesign::default().validate().is_ok());
        assert!(NoseDesign::anime().validate().is_ok());
        assert!(LipsDesign::default().validate().is_ok());
        assert!(LipsDesign::anime().validate().is_ok());
        assert!(EarDesign::default().validate().is_ok());
        assert!(EarDesign::anime().validate().is_ok());
    }
}
