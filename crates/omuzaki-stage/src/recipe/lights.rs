//! The light-rig library: the prototype's seven rigs, as data. Each rig is one TOML file under `library/light-rigs/`,
//! transcribed from the golden manifest's `spec.rig` blocks. Only lighting numbers are stored here; no image, mesh or
//! likeness is.
//!
//! Units are three.js's (render-api D10), so every number is copied as the prototype wrote it and never rescaled.
//! Parts of a rig that the render contract has no field for (the key light's shadow frustum, the skin floor) stay in
//! the file, so the file round-trips, and the parts the contract does take are read by [`LightRig::to_lights`],
//! [`LightRig::post`] and [`LightRig::background`].

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{Background, Bloom, Colour, Light, Post, Tonemap};
use serde::{Deserialize, Serialize};

use super::RecipeError;

/// The rigs shipped with the crate, by name. Each file is parsed on use, so a broken file fails the test that loads it.
static LIBRARY: [(&str, &str); 7] = [
    ("viewer-studio", include_str!("../../library/light-rigs/viewer-studio.toml")),
    ("viewer-candle", include_str!("../../library/light-rigs/viewer-candle.toml")),
    ("viewer-moonlit", include_str!("../../library/light-rigs/viewer-moonlit.toml")),
    ("dev-studio", include_str!("../../library/light-rigs/dev-studio.toml")),
    ("dev-night", include_str!("../../library/light-rigs/dev-night.toml")),
    ("dev-warm", include_str!("../../library/light-rigs/dev-warm.toml")),
    ("perform-facecam", include_str!("../../library/light-rigs/perform-facecam.toml")),
];

/// Directional lights shine from `position` toward this point. Three.js aims every directional light at the origin
/// unless told otherwise, and no golden case sets a target.
const TARGET: Vec3 = Vec3::ZERO;

/// The unit direction from [`TARGET`] toward `position`, or `None` when there is none: the position is at the target,
/// or so close to it that its length underflows. [`LightRig::validate`] and [`LightRig::to_lights`] both use this one
/// function, so they accept and refuse the same positions.
fn direction(position: [f32; 3]) -> Option<Vec3> {
    (Vec3::from(position) - TARGET).try_normalize()
}

/// Why a directional light's position gives no direction, naming the field the rig must fix.
fn no_direction_reason(label: &str, position: [f32; 3]) -> String {
    format!("{label}.position {position:?} has no direction: it must sit away from the target at the origin")
}

/// One light rig: the lights, exposure, background and bloom a golden case is lit with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightRig {
    /// The rig's name, as the manifest's `rig` field spells it (`viewer-studio`).
    pub name: String,
    /// Where the numbers were read from, for whoever checks them next (`js/viewer.js LIGHTS.studio`).
    pub source: String,
    /// Scene-light multiplier before tone mapping, `>= 0`.
    pub exposure: f32,
    /// The flat background colour.
    pub background: Colour,
    /// The shadow floor of skin under this rig, `0..=1`. The material's `Toon.skin.floor` takes it when the
    /// character's materials are built; the rig only carries the number.
    pub skin_floor: f32,
    /// Sky-and-ground ambient light: the base of every rig.
    pub hemisphere: Hemisphere,
    /// The key light. Every rig has one.
    pub key: Directional,
    /// The rim light, if the rig has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rim: Option<Directional>,
    /// The fill light, if the rig has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Directional>,
    /// A point light placed relative to the feet (the candle), if the rig has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<Point>,
    /// Bloom settings. Whether a case uses them is the case's choice, not the rig's.
    pub bloom: BloomSettings,
}

/// Sky-and-ground ambient light.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hemisphere {
    /// Colour of the light arriving from straight up.
    pub sky: Colour,
    /// Colour of the light arriving from straight down.
    pub ground: Colour,
    /// Brightness multiplier, `>= 0`.
    pub intensity: f32,
}

/// A directional light: sun, key, rim or fill.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Directional {
    /// Light colour.
    pub colour: Colour,
    /// Brightness multiplier, `>= 0`.
    pub intensity: f32,
    /// Where the light sits, in metres. It shines from here toward the origin.
    pub position: [f32; 3],
    /// Shadow settings, if the light casts shadows. The contract takes only the on/off switch; the renderer fits the
    /// frustum to the casters, and the map size comes from the render quality. The numbers are kept here so the rig
    /// round-trips and the prototype's values can be compared later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Shadow>,
}

/// The prototype's shadow camera for one directional light (three.js `OrthographicCamera` bounds and map size).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shadow {
    /// Shadow map width and height, in texels (a power of two in the prototype).
    pub map_size: u32,
    /// Left bound of the shadow camera, metres.
    pub left: f32,
    /// Right bound of the shadow camera, metres.
    pub right: f32,
    /// Top bound of the shadow camera, metres.
    pub top: f32,
    /// Bottom bound of the shadow camera, metres.
    pub bottom: f32,
    /// Near plane of the shadow camera, metres.
    pub near: f32,
    /// Far plane of the shadow camera, metres.
    pub far: f32,
    /// Depth bias, added to every shadow test.
    pub bias: f32,
    /// Bias along the surface normal, which hides acne on curved surfaces.
    pub normal_bias: f32,
}

/// A point light. Its falloff is three.js's at the rig's decay (2, the default; the manifest has no other value).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    /// Light colour.
    pub colour: Colour,
    /// Brightness multiplier, `>= 0`.
    pub intensity: f32,
    /// Position in metres, relative to the feet (which the recipe puts at the origin).
    pub position: [f32; 3],
    /// Three.js `distance`: where the light reaches zero. It is the contract's `Light::Point::range`.
    pub distance: f32,
}

/// Bloom: how much the bright parts glow.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BloomSettings {
    /// How much glow is added, `>= 0`.
    pub strength: f32,
    /// How far it spreads, `0..=1` of the image.
    pub radius: f32,
    /// Brightness above which pixels glow.
    pub threshold: f32,
}

impl LightRig {
    /// The names of the rigs in the library, in file order.
    pub fn names() -> impl Iterator<Item = &'static str> {
        LIBRARY.iter().map(|(name, _)| *name)
    }

    /// The library rig called `name`, parsed and validated.
    ///
    /// # Errors
    /// [`RecipeError::UnknownRig`] if the library has no rig of that name, or the errors of [`LightRig::from_toml`].
    pub fn load(name: &str) -> Result<Self, RecipeError> {
        let (_, text) =
            LIBRARY.iter().find(|(n, _)| *n == name).ok_or_else(|| RecipeError::UnknownRig(name.to_owned()))?;
        let rig = Self::from_toml(text)?;
        if rig.name != name {
            return Err(RecipeError::invalid(name, format!("the file says it is '{}'", rig.name)));
        }
        Ok(rig)
    }

    /// Parses a rig file and validates it.
    ///
    /// # Errors
    /// [`RecipeError::Parse`] if the text is not a rig, [`RecipeError::Invalid`] if a number is out of range.
    pub fn from_toml(text: &str) -> Result<Self, RecipeError> {
        let rig: Self = toml::from_str(text).map_err(|e| RecipeError::parse("light rig", &e))?;
        rig.validate()?;
        Ok(rig)
    }

    /// Checks every number is finite and in range, so a rig that loads can always be lit.
    ///
    /// # Errors
    /// [`RecipeError::Invalid`] naming the rig and the first value that is out of range.
    pub fn validate(&self) -> Result<(), RecipeError> {
        let bad = |reason: String| RecipeError::invalid(&self.name, reason);
        if self.name.is_empty() {
            return Err(bad("the rig needs a name".into()));
        }
        non_negative(self.exposure, "exposure").map_err(bad)?;
        if !(0.0..=1.0).contains(&self.skin_floor) {
            return Err(bad(format!("skin_floor {} must be in 0..=1", self.skin_floor)));
        }
        non_negative(self.hemisphere.intensity, "hemisphere intensity").map_err(bad)?;
        self.key.validate("key").map_err(bad)?;
        for (label, light) in [("rim", &self.rim), ("fill", &self.fill)] {
            if let Some(light) = light {
                light.validate(label).map_err(bad)?;
            }
        }
        if let Some(point) = &self.point {
            non_negative(point.intensity, "point intensity").map_err(bad)?;
            finite_vec(point.position, "point position").map_err(bad)?;
            if !(point.distance.is_finite() && point.distance > 0.0) {
                return Err(bad(format!("point distance {} must be above 0", point.distance)));
            }
        }
        let b = self.bloom;
        non_negative(b.strength, "bloom strength").map_err(bad)?;
        non_negative(b.threshold, "bloom threshold").map_err(bad)?;
        if !(0.0..=1.0).contains(&b.radius) {
            return Err(bad(format!("bloom radius {} must be in 0..=1", b.radius)));
        }
        Ok(())
    }

    /// The lights this rig adds to a scene, in the order the prototype adds them: hemisphere, key, rim, fill, point.
    ///
    /// # Errors
    /// [`RecipeError::Invalid`] if a directional light has no direction (its position is the target).
    pub fn to_lights(&self) -> Result<Vec<Light>, RecipeError> {
        let mut lights = vec![Light::Hemisphere {
            sky: self.hemisphere.sky,
            ground: self.hemisphere.ground,
            intensity: self.hemisphere.intensity,
        }];
        for (label, light) in [("key", Some(&self.key)), ("rim", self.rim.as_ref()), ("fill", self.fill.as_ref())] {
            if let Some(light) = light {
                lights.push(light.to_light(&self.name, label)?);
            }
        }
        if let Some(point) = &self.point {
            lights.push(Light::Point {
                position: Vec3::from(point.position),
                colour: point.colour,
                intensity: point.intensity,
                range: point.distance,
            });
        }
        Ok(lights)
    }

    /// The look of the frame: this rig's exposure and ACES tone mapping, with bloom when the case asks for it.
    pub fn post(&self, bloom_enabled: bool) -> Post {
        let bloom = bloom_enabled.then_some(Bloom {
            strength: self.bloom.strength,
            radius: self.bloom.radius,
            threshold: self.bloom.threshold,
        });
        Post { exposure: self.exposure, tonemap: Tonemap::Aces, bloom, ..Post::default() }
    }

    /// The flat background this rig is lit against.
    pub fn background(&self) -> Background {
        Background::Solid { colour: self.background }
    }
}

impl Directional {
    /// Checks the intensity, the position and the shadow block; `label` names the light in the error.
    fn validate(&self, label: &str) -> Result<(), String> {
        non_negative(self.intensity, &format!("{label} intensity"))?;
        finite_vec(self.position, &format!("{label} position"))?;
        if direction(self.position).is_none() {
            return Err(no_direction_reason(label, self.position));
        }
        if let Some(s) = &self.shadow {
            s.validate(label)?;
        }
        Ok(())
    }

    /// The contract's directional light: the direction toward the light, from `position` and the target.
    fn to_light(&self, rig: &str, label: &str) -> Result<Light, RecipeError> {
        let to_light = direction(self.position)
            .ok_or_else(|| RecipeError::invalid(rig, no_direction_reason(label, self.position)))?;
        Ok(Light::Directional {
            to_light,
            colour: self.colour,
            intensity: self.intensity,
            casts_shadow: self.shadow.is_some(),
        })
    }
}

impl Shadow {
    /// Checks the bounds form a box with positive size and the map has texels.
    fn validate(&self, label: &str) -> Result<(), String> {
        if self.map_size == 0 {
            return Err(format!("{label} shadow map_size must be above 0"));
        }
        let values = [self.left, self.right, self.top, self.bottom, self.near, self.far, self.bias, self.normal_bias];
        if !values.iter().all(|v| v.is_finite()) {
            return Err(format!("{label} shadow numbers must be finite"));
        }
        if !(self.left < self.right && self.bottom < self.top && self.near < self.far) {
            return Err(format!("{label} shadow needs left < right, bottom < top and near < far"));
        }
        Ok(())
    }
}

/// `value` if it is finite and not below zero, else a message naming `what`.
fn non_negative(value: f32, what: &str) -> Result<(), String> {
    if value.is_finite() && value >= 0.0 { Ok(()) } else { Err(format!("{what} {value} must be a number >= 0")) }
}

/// `v` if every component is finite, else a message naming `what`.
fn finite_vec(v: [f32; 3], what: &str) -> Result<(), String> {
    if v.iter().all(|c| c.is_finite()) { Ok(()) } else { Err(format!("{what} {v:?} must be finite")) }
}
