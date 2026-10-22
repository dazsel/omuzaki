//! The light rig as values (design D3): `lighting.toml`. A light-rig file is this type plus metadata.

use std::collections::BTreeMap;

use omuzaki_render_api::{Bloom, Colour};
use omuzaki_store_api::ContentHash;
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
    /// A spot light (a stage light, a torch): a point light confined to a cone from `position` toward `target`
    /// (LK-08, phase 2).
    Spot {
        /// The lamp, in the anchor's units (design D4).
        position: [f32; 3],
        /// What it points at, in the anchor's units; not the lamp's own position.
        target: [f32; 3],
        /// The cone's outer half-angle, axis to edge, in degrees, `(0, 90]`.
        cone: f32,
        /// How much of the cone is soft, `0..=1`: 0 is a hard edge, 1 fades from the axis.
        penumbra: f32,
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
    /// Brightness (three.js units, render-api D10), `>= 0`. Point and spot intensities are given for a 1 m anchor.
    pub intensity: f32,
    /// Casts shadows. At most one directional light of a scene may; a spot's is the author's wish, which a renderer
    /// without spot shadows ignores.
    pub shadow: bool,
    /// What the shape's lengths are measured against.
    pub anchor: Anchor,
    /// Directional, point or spot, with the numbers each needs.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cone: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    penumbra: Option<f32>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum WireKind {
    Directional,
    Point,
    Spot,
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
                let extra = [
                    ("position", w.position.is_some()),
                    ("range", w.range.is_some()),
                    ("target", w.target.is_some()),
                    ("cone", w.cone.is_some()),
                    ("penumbra", w.penumbra.is_some()),
                ];
                if let Some(e) = wrong(&extra, "directional") {
                    return Err(e);
                }
                LightShape::Directional {
                    azimuth: w.azimuth.ok_or("a directional light needs `azimuth`")?,
                    elevation: w.elevation.ok_or("a directional light needs `elevation`")?,
                }
            }
            WireKind::Point => {
                let extra = [
                    ("azimuth", w.azimuth.is_some()),
                    ("elevation", w.elevation.is_some()),
                    ("target", w.target.is_some()),
                    ("cone", w.cone.is_some()),
                    ("penumbra", w.penumbra.is_some()),
                ];
                if let Some(e) = wrong(&extra, "point") {
                    return Err(e);
                }
                LightShape::Point {
                    position: w.position.ok_or("a point light needs `position`")?,
                    range: w.range.ok_or("a point light needs `range`")?,
                }
            }
            WireKind::Spot => {
                if let Some(e) =
                    wrong(&[("azimuth", w.azimuth.is_some()), ("elevation", w.elevation.is_some())], "spot")
                {
                    return Err(e);
                }
                LightShape::Spot {
                    position: w.position.ok_or("a spot light needs `position`")?,
                    target: w.target.ok_or("a spot light needs `target`")?,
                    cone: w.cone.ok_or("a spot light needs `cone`")?,
                    penumbra: w.penumbra.unwrap_or(0.0),
                    range: w.range.ok_or("a spot light needs `range`")?,
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
            target: None,
            cone: None,
            penumbra: None,
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
            LightShape::Spot { position, target, cone, penumbra, range } => {
                w.kind = WireKind::Spot;
                w.position = Some(position);
                w.target = Some(target);
                w.cone = Some(cone);
                w.penumbra = Some(penumbra);
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

/// Linear distance fog (LK-08, phase 2): surfaces fade to `colour` between `near` and `far`, measured from the camera
/// along its view in subject units (design D4), so a rig fogs every stature alike. The backdrop is not fogged.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FogDef {
    /// The colour distant surfaces fade to.
    pub colour: Colour,
    /// Where the fog starts, in subject units from the camera, `>= 0`.
    pub near: f32,
    /// Where surfaces are fully fogged, in subject units from the camera, `> near`.
    pub far: f32,
}

/// What an environment's light comes from.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum EnvironmentSource {
    /// A procedural gradient: no asset, the same on every machine, so seed rigs can use it (ADR 0019 D15).
    Sky {
        /// The colour straight up.
        zenith: Colour,
        /// The colour at the horizon.
        horizon: Colour,
        /// The colour straight down.
        ground: Colour,
    },
    /// An equirectangular HDRI from the asset store (a Radiance `.hdr`), its provenance and licence in the store.
    Hdri {
        /// The asset's content hash.
        asset: ContentHash,
    },
}

/// Image-based light (LK-08, phase 2): a sky that lights the cast from every direction, and that the `environment`
/// backdrop shows. Not a light: it takes no slot of the renderer's [`MAX_LIGHTS`](omuzaki_render_api::limits::MAX_LIGHTS).
/// In the files the source's fields sit beside the common ones (`kind = "sky"`, `zenith`...).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "EnvironmentWire", into = "EnvironmentWire")]
#[schemars(with = "EnvironmentWire")]
pub struct EnvironmentDef {
    /// Where the light comes from.
    pub source: EnvironmentSource,
    /// Brightness, `>= 0`.
    pub intensity: f32,
    /// Turns the sky about the vertical, in degrees, counter-clockwise seen from above.
    pub rotation: f32,
    /// Quantise the sky's diffuse light onto each material's toon ramp, keeping the cel look.
    pub toon_ramp: bool,
}

/// The sky of the design's example (scenes D3): a dusk blue over a warm horizon.
const SKY_ZENITH: Colour = Colour::hex(0x006f_86b8);
const SKY_HORIZON: Colour = Colour::hex(0x00e8_c9a0);
const SKY_GROUND: Colour = Colour::hex(0x003b_3026);

impl Default for EnvironmentDef {
    /// The design's example sky at full strength, through the toon ramp.
    fn default() -> Self {
        Self {
            source: EnvironmentSource::Sky { zenith: SKY_ZENITH, horizon: SKY_HORIZON, ground: SKY_GROUND },
            intensity: 1.0,
            rotation: 0.0,
            toon_ramp: true,
        }
    }
}

/// The file shape of an [`EnvironmentDef`]: flat, with the source's fields optional so a wrong one is named.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EnvironmentWire {
    kind: EnvironmentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    zenith: Option<Colour>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    horizon: Option<Colour>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ground: Option<Colour>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    asset: Option<ContentHash>,
    #[serde(default = "one")]
    intensity: f32,
    #[serde(default)]
    rotation: f32,
    #[serde(default = "yes")]
    toon_ramp: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum EnvironmentKind {
    Sky,
    Hdri,
}

fn yes() -> bool {
    true
}

impl TryFrom<EnvironmentWire> for EnvironmentDef {
    type Error = String;

    fn try_from(w: EnvironmentWire) -> Result<Self, String> {
        let source = match w.kind {
            EnvironmentKind::Sky => {
                if w.asset.is_some() {
                    return Err("a sky environment has no `asset`".into());
                }
                EnvironmentSource::Sky {
                    zenith: w.zenith.unwrap_or(SKY_ZENITH),
                    horizon: w.horizon.unwrap_or(SKY_HORIZON),
                    ground: w.ground.unwrap_or(SKY_GROUND),
                }
            }
            EnvironmentKind::Hdri => {
                let colours = [("zenith", w.zenith), ("horizon", w.horizon), ("ground", w.ground)];
                if let Some((name, _)) = colours.iter().find(|(_, c)| c.is_some()) {
                    return Err(format!("an hdri environment has no `{name}`"));
                }
                EnvironmentSource::Hdri { asset: w.asset.ok_or("an hdri environment needs `asset`")? }
            }
        };
        Ok(Self { source, intensity: w.intensity, rotation: w.rotation, toon_ramp: w.toon_ramp })
    }
}

impl From<EnvironmentDef> for EnvironmentWire {
    fn from(e: EnvironmentDef) -> Self {
        let mut w = Self {
            kind: EnvironmentKind::Sky,
            zenith: None,
            horizon: None,
            ground: None,
            asset: None,
            intensity: e.intensity,
            rotation: e.rotation,
            toon_ramp: e.toon_ramp,
        };
        match e.source {
            EnvironmentSource::Sky { zenith, horizon, ground } => {
                w.zenith = Some(zenith);
                w.horizon = Some(horizon);
                w.ground = Some(ground);
            }
            EnvironmentSource::Hdri { asset } => {
                w.kind = EnvironmentKind::Hdri;
                w.asset = Some(asset);
            }
        }
        w
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
    /// How strongly lights behind the cast edge its silhouette in their colour, `>= 0`: a stage's back light. 0 (the
    /// default) lights with the toon ramp alone.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub back_light_rim: f32,
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
    /// Distance fog, if wanted (phase 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fog: Option<FogDef>,
    /// Image-based light, if wanted (phase 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<EnvironmentDef>,
}

fn black() -> Colour {
    Colour::BLACK
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference.
fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            rig: None,
            exposure: 1.0,
            background: Colour::BLACK,
            outline_light: 1.0,
            back_light_rim: 0.0,
            skin_floor: None,
            ambient: Ambient::default(),
            bloom: None,
            lights: BTreeMap::new(),
            fog: None,
            environment: None,
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
