//! The light rig as values (design D3): `lighting.toml`. A light-rig file is this type plus metadata.

use std::collections::BTreeMap;

use omuzaki_render_api::{Bloom, Colour};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::anchor::Anchor;
use crate::ids::LightId;

/// What a light is for, for people, agents and "match a reference" (LK-04). It changes nothing about how it renders.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LightRole {
    /// The main light.
    Key,
    /// Softens the key's shadows.
    Fill,
    /// Separates the subject from the background.
    Rim,
    /// A coloured accent.
    Accent,
    /// A light that is in the scene (a lamp, a candle).
    Practical,
}

/// The shape of a light: where its light comes from.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LightShape {
    /// Parallel light from far away.
    Directional {
        /// Degrees around the anchor: 0 = from the front, + = from screen right (a front camera's view). `-180..=180`.
        azimuth: f32,
        /// Degrees above the horizon, `-90..=90`.
        elevation: f32,
    },
    /// A point light that falls off to zero at `range`.
    Point {
        /// In the anchor's units (design D4): x right, y up, z toward the front camera.
        position: [f32; 3],
        /// Where the light reaches zero, in the anchor's units, `> 0`.
        range: f32,
    },
}

/// One light of a rig. In the files the shape's fields sit beside the common ones (`kind = "point"`, `position`...).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "LightWire", into = "LightWire")]
#[schemars(with = "LightWire")]
pub struct LightDef {
    /// What the light is for.
    pub role: Option<LightRole>,
    /// Light colour.
    pub colour: Colour,
    /// Brightness (three.js units, render-api D10), `>= 0`. Point intensities are given for a 1 m anchor.
    pub intensity: f32,
    /// Casts shadows. At most one directional light of a scene may.
    pub shadow: bool,
    /// What the shape's lengths are measured against.
    pub anchor: Anchor,
    /// Directional or point, with the numbers each needs.
    pub shape: LightShape,
}

/// The file shape of a [`LightDef`]: flat, with the shape's fields optional so a wrong one is named.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LightWire {
    kind: WireKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<LightRole>,
    #[serde(default = "white")]
    colour: Colour,
    #[serde(default = "one")]
    intensity: f32,
    #[serde(default)]
    shadow: bool,
    #[serde(default)]
    anchor: Anchor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    azimuth: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    elevation: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    position: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    range: Option<f32>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum WireKind {
    Directional,
    Point,
}

fn white() -> Colour {
    Colour::WHITE
}

fn one() -> f32 {
    1.0
}

impl TryFrom<LightWire> for LightDef {
    type Error = String;

    fn try_from(w: LightWire) -> Result<Self, String> {
        let wrong = |names: &[(&str, bool)], kind: &str| {
            names.iter().find(|(_, present)| *present).map(|(name, _)| format!("a {kind} light has no `{name}`"))
        };
        let shape = match w.kind {
            WireKind::Directional => {
                if let Some(e) =
                    wrong(&[("position", w.position.is_some()), ("range", w.range.is_some())], "directional")
                {
                    return Err(e);
                }
                LightShape::Directional {
                    azimuth: w.azimuth.ok_or("a directional light needs `azimuth`")?,
                    elevation: w.elevation.ok_or("a directional light needs `elevation`")?,
                }
            }
            WireKind::Point => {
                if let Some(e) =
                    wrong(&[("azimuth", w.azimuth.is_some()), ("elevation", w.elevation.is_some())], "point")
                {
                    return Err(e);
                }
                LightShape::Point {
                    position: w.position.ok_or("a point light needs `position`")?,
                    range: w.range.ok_or("a point light needs `range`")?,
                }
            }
        };
        Ok(Self { role: w.role, colour: w.colour, intensity: w.intensity, shadow: w.shadow, anchor: w.anchor, shape })
    }
}

impl From<LightDef> for LightWire {
    fn from(l: LightDef) -> Self {
        let mut w = Self {
            kind: WireKind::Directional,
            role: l.role,
            colour: l.colour,
            intensity: l.intensity,
            shadow: l.shadow,
            anchor: l.anchor,
            azimuth: None,
            elevation: None,
            position: None,
            range: None,
        };
        match l.shape {
            LightShape::Directional { azimuth, elevation } => {
                w.azimuth = Some(azimuth);
                w.elevation = Some(elevation);
            }
            LightShape::Point { position, range } => {
                w.kind = WireKind::Point;
                w.position = Some(position);
                w.range = Some(range);
            }
        }
        w
    }
}

impl LightDef {
    /// True for a directional light.
    #[must_use]
    pub fn is_directional(&self) -> bool {
        matches!(self.shape, LightShape::Directional { .. })
    }
}

/// The hemisphere light every rig starts from.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ambient {
    /// Colour of light arriving from straight up.
    pub sky: Colour,
    /// Colour of light arriving from straight down.
    pub ground: Colour,
    /// Brightness, `>= 0`.
    pub intensity: f32,
}

impl Default for Ambient {
    /// No ambient light: a rig that wants some says so.
    fn default() -> Self {
        Self { sky: Colour::WHITE, ground: Colour::rgb(0x80, 0x80, 0x80), intensity: 0.0 }
    }
}

/// Bloom as authored. The render-api [`Bloom`] with a strict reader: the render type tolerates unknown fields, and a
/// misspelt `strenght` must be refused, not dropped on the next save (design D13).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BloomDef {
    /// How much glow is added, `>= 0`.
    pub strength: f32,
    /// How far it spreads, `0..=1` of the image.
    pub radius: f32,
    /// Brightness above which pixels glow.
    pub threshold: f32,
}

impl From<BloomDef> for Bloom {
    fn from(b: BloomDef) -> Self {
        Self { strength: b.strength, radius: b.radius, threshold: b.threshold }
    }
}

impl From<Bloom> for BloomDef {
    fn from(b: Bloom) -> Self {
        Self { strength: b.strength, radius: b.radius, threshold: b.threshold }
    }
}

/// A scene's lighting, materialised: a rig's values copied in, the rig's name kept only as information.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lighting {
    /// The rig these values came from. Informational: editing a light doesn't clear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig: Option<String>,
    /// Scene-light multiplier before tone mapping, `>= 0`.
    #[serde(default = "one")]
    pub exposure: f32,
    /// Shown when the backdrop is `lighting`.
    #[serde(default = "black")]
    pub background: Colour,
    /// Scales outline colours; lower it in dark scenes so line art doesn't glow. `>= 0`.
    #[serde(default = "one")]
    pub outline_light: f32,
    /// Overrides every cast skin material's shadow floor while set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin_floor: Option<f32>,
    /// The hemisphere light.
    #[serde(default)]
    pub ambient: Ambient,
    /// Bloom strength and shape; whether it runs is the scene's `post.bloom`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bloom: Option<BloomDef>,
    /// The lights, by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lights: BTreeMap<LightId, LightDef>,
}

fn black() -> Colour {
    Colour::BLACK
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            rig: None,
            exposure: 1.0,
            background: Colour::BLACK,
            outline_light: 1.0,
            skin_floor: None,
            ambient: Ambient::default(),
            bloom: None,
            lights: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn light(v: serde_json::Value) -> Result<LightDef, serde_json::Error> {
        serde_json::from_value(v)
    }

    #[test]
    fn lights_read_flat_and_write_back_the_same_keys() {
        let v = json!({"kind": "point", "role": "practical", "colour": "#ffd9a0", "intensity": 5.0, "shadow": false,
            "anchor": "member:kiri", "position": [1.5, 1.25, -1.0], "range": 7.0});
        let l = light(v.clone()).unwrap();
        assert_eq!(l.shape, LightShape::Point { position: [1.5, 1.25, -1.0], range: 7.0 });
        assert_eq!(serde_json::to_value(&l).unwrap(), v);
    }

    #[test]
    fn optional_light_fields_default() {
        let l = light(json!({"kind": "directional", "azimuth": 10.0, "elevation": 20.0})).unwrap();
        assert_eq!(
            (l.role, l.colour, l.intensity, l.shadow, l.anchor),
            (None, Colour::WHITE, 1.0, false, Anchor::Subject)
        );
    }

    #[test]
    fn a_light_with_the_wrong_shape_fields_is_refused_by_name() {
        let cases = [
            (json!({"kind": "directional", "azimuth": 1.0}), "needs `elevation`"),
            (json!({"kind": "directional", "azimuth": 1.0, "elevation": 2.0, "range": 3.0}), "no `range`"),
            (json!({"kind": "point", "range": 1.0}), "needs `position`"),
            (json!({"kind": "point", "position": [0, 0, 0], "range": 1.0, "azimuth": 2.0}), "no `azimuth`"),
            (json!({"kind": "spot", "azimuth": 1.0}), "spot"),
            (json!({"kind": "point", "position": [0, 0, 0], "range": 1.0, "colur": "#fff"}), "colur"),
        ];
        for (v, expect) in cases {
            let err = light(v).unwrap_err().to_string();
            assert!(err.contains(expect), "{err}");
        }
    }
}
