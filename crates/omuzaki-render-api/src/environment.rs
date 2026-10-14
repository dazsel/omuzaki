//! Image-based light: an environment is a sky that lights the scene from every direction (LK-08).
//!
//! An environment is a resource, like a texture: the caller describes it once ([`EnvironmentDesc`]), gets an
//! [`EnvironmentId`](crate::EnvironmentId) back, and each frame a [`Scene`](crate::Scene) says how to use it
//! ([`EnvironmentUse`]). The renderer derives what shading needs from the radiance map at creation (diffuse
//! irradiance and a prefiltered specular chain), so a frame never filters anything.
//!
//! **Conventions** (the GPU renderer and the mock share these, so a scene looks the same on both):
//! - Radiance is *linear* light, scene-referred: 1.0 is a white diffuse surface lit by a unit directional light's
//!   worth of sky. Environment intensity 1 and a uniform sky of colour `c` light a diffuse surface to `albedo * c`
//!   (three.js `envMapIntensity` with an irradiance map).
//! - An HDRI is equirectangular in the three.js layout: the horizontal coordinate is `u = atan2(z, x) / 2π + 0.5`, so
//!   direction `+X` is the middle of the image, `+Z` is three quarters across and `-Z` a quarter; rows run top
//!   (straight up) to bottom (straight down).
//! - [`EnvironmentUse::rotation_deg`] turns the sky about `+Y`, positive counter-clockwise seen from above, exactly
//!   like rotating the sky's picture in the world.

use omuzaki_core::glam::{UVec2, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Colour, EnvironmentId, RenderError};

/// What an environment is made of.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnvironmentDesc {
    /// A procedural sky: no asset, so seed rigs can use it (ADR 0019 D15). Its radiance depends on elevation only,
    /// see [`sky_radiance`].
    Sky {
        /// Colour straight up.
        zenith: Colour,
        /// Colour at the horizon.
        horizon: Colour,
        /// Colour straight down.
        ground: Colour,
    },
    /// An HDRI: equirectangular linear RGB (see the module docs for the layout).
    Hdri {
        /// Width and height in pixels, each at least 1 and within `Capabilities::max_texture_size`.
        #[schemars(with = "[u32; 2]")]
        size: UVec2,
        /// `size.x * size.y * 3` finite, non-negative floats: linear red, green and blue per pixel, rows top to
        /// bottom.
        rgb: Vec<f32>,
    },
}

impl EnvironmentDesc {
    /// Checks the rules every renderer shares. The renderer adds its texture-size limit.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for a zero side, a pixel count that doesn't match, or a negative or non-finite value.
    pub fn validate(&self) -> Result<(), RenderError> {
        let Self::Hdri { size, rgb } = self else { return Ok(()) };
        if size.x == 0 || size.y == 0 {
            return Err(RenderError::invalid("environment", format!("size {}x{} has a zero side", size.x, size.y)));
        }
        // Two 32-bit sides times three can pass 64 bits, so the product is checked, not assumed.
        let want = u64::from(size.x).checked_mul(u64::from(size.y)).and_then(|pixels| pixels.checked_mul(3));
        if want != Some(rgb.len() as u64) {
            let needs = want.map_or_else(|| "more than fits in 64 bits".to_owned(), |w| w.to_string());
            let reason = format!("{} floats for a {}x{} HDRI (needs {needs})", rgb.len(), size.x, size.y);
            return Err(RenderError::invalid("environment", reason));
        }
        if rgb.iter().any(|v| !(v.is_finite() && *v >= 0.0)) {
            return Err(RenderError::invalid("environment", "HDRI values must be finite and >= 0"));
        }
        Ok(())
    }
}

/// The radiance of a procedural sky towards a direction whose height is `y` (the sine of its elevation, `-1..=1`), in
/// linear light: the horizon colour blends linearly (in `y`) to the zenith above it and to the ground below it.
///
/// Defined once here because it is the contract: renderers sample it into whatever they light with, and conformance
/// checks hold them to it.
pub fn sky_radiance(zenith: Colour, horizon: Colour, ground: Colour, y: f32) -> Vec3 {
    let y = y.clamp(-1.0, 1.0);
    let horizon = horizon.to_linear();
    if y >= 0.0 { horizon.lerp(zenith.to_linear(), y) } else { horizon.lerp(ground.to_linear(), -y) }
}

/// How one frame uses an environment.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EnvironmentUse {
    /// The environment, from [`Renderer::create_environment`](crate::Renderer::create_environment).
    pub id: EnvironmentId,
    /// Brightness multiplier, `>= 0`.
    pub intensity: f32,
    /// Turns the sky about `+Y` in degrees (an HDRI's sun moves; a procedural sky looks the same).
    pub rotation_deg: f32,
    /// Quantise the diffuse light onto the material's toon ramp, so the sky lights in the same two or three bands as
    /// a key light and keeps the cel look. Off, the sky shades smoothly with the surface normal.
    ///
    /// On, a surface takes the irradiance facing the sky's brightest side where it faces that side and the
    /// irradiance facing away elsewhere, blended by the material's ramp (`shift`, `softness`) over the dot of its
    /// normal and the brightest direction. The brightest direction is the luminance-weighted mean direction of the
    /// sky; a uniform sky has none, so it shades flat.
    pub toon_ramp: bool,
}

impl EnvironmentUse {
    /// The environment at full strength, unrotated and shaded smoothly.
    pub fn new(id: EnvironmentId) -> Self {
        Self { id, intensity: 1.0, rotation_deg: 0.0, toon_ramp: false }
    }

    pub(crate) fn validate(&self) -> Result<(), RenderError> {
        if self.intensity >= 0.0 && self.intensity.is_finite() && self.rotation_deg.is_finite() {
            return Ok(());
        }
        Err(RenderError::invalid("environment use", format!("{self:?} needs an intensity >= 0 and a finite rotation")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdri_validation_checks_size_count_and_values() {
        let hdri = |size: UVec2, rgb: Vec<f32>| EnvironmentDesc::Hdri { size, rgb };
        assert_eq!(hdri(UVec2::new(2, 1), vec![0.5; 6]).validate(), Ok(()));
        assert!(hdri(UVec2::new(0, 1), vec![]).validate().is_err(), "zero side");
        assert!(hdri(UVec2::new(2, 1), vec![0.5; 5]).validate().is_err(), "short");
        let huge = hdri(UVec2::new(u32::MAX, u32::MAX), vec![0.0; 3]).validate();
        assert!(matches!(huge, Err(RenderError::Invalid { .. })), "oversized dimensions don't overflow: {huge:?}");
        assert!(hdri(UVec2::new(1, 1), vec![0.0, -1.0, 0.0]).validate().is_err(), "negative");
        assert!(hdri(UVec2::new(1, 1), vec![0.0, f32::NAN, 0.0]).validate().is_err(), "NaN");
        assert_eq!(
            EnvironmentDesc::Sky { zenith: Colour::WHITE, horizon: Colour::WHITE, ground: Colour::BLACK }.validate(),
            Ok(())
        );
    }

    #[test]
    fn sky_blends_through_the_horizon() {
        let (z, h, g) = (Colour::WHITE, Colour::hex(0x0080_8080), Colour::BLACK);
        assert_eq!(sky_radiance(z, h, g, 1.0), Vec3::ONE);
        assert_eq!(sky_radiance(z, h, g, -1.0), Vec3::ZERO);
        assert_eq!(sky_radiance(z, h, g, 0.0), h.to_linear());
        let mid = sky_radiance(z, h, g, 0.5);
        assert!((mid - (h.to_linear() + Vec3::ONE) * 0.5).length() < 1e-6);
    }

    #[test]
    fn descriptions_round_trip_and_have_a_schema() {
        let sky =
            EnvironmentDesc::Sky { zenith: Colour::WHITE, horizon: Colour::hex(0x00e8_c9a0), ground: Colour::BLACK };
        let json = serde_json::to_value(&sky).unwrap();
        assert_eq!(json["kind"], "sky");
        assert_eq!(serde_json::from_value::<EnvironmentDesc>(json).unwrap(), sky);
        assert!(serde_json::to_value(schemars::schema_for!(EnvironmentDesc)).is_ok());
    }

    #[test]
    fn use_validation_rejects_negative_intensity_and_bad_rotation() {
        let mut u = EnvironmentUse::new(EnvironmentId::from_parts(0, 1));
        assert_eq!(u.validate(), Ok(()));
        u.intensity = -0.1;
        assert!(u.validate().is_err());
        u.intensity = 1.0;
        u.rotation_deg = f32::INFINITY;
        assert!(u.validate().is_err());
    }
}
