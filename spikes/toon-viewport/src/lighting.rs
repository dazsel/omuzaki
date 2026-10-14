//! The scene lights, as the prototype page sets them up, and the shadow camera for the key light.

use glam::camera::rh::{proj::directx, view};
use glam::{Mat4, Vec3};

/// sRGB hex (0xRRGGBB) to the linear colour three.js uses internally (`new THREE.Color(hex)`).
#[must_use]
pub fn linear_from_hex(hex: u32) -> Vec3 {
    let channel = |shift: u32| {
        let c = f32::from(u8::try_from((hex >> shift) & 0xff).unwrap_or(0)) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    Vec3::new(channel(16), channel(8), channel(0))
}

/// One directional light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirLight {
    /// Where the light sits; it shines at the origin.
    pub position: Vec3,
    /// Linear colour.
    pub color: Vec3,
    pub intensity: f32,
    /// Whether this light owns the shadow map (at most one does).
    pub casts_shadow: bool,
}

impl DirLight {
    /// Unit vector from the origin toward the light.
    #[must_use]
    pub fn direction(&self) -> Vec3 {
        self.position.normalize_or(Vec3::Y)
    }

    /// Azimuth (radians from +Z toward +X) and elevation (radians above the horizon).
    #[must_use]
    pub fn angles(&self) -> (f32, f32) {
        let d = self.direction();
        (d.x.atan2(d.z), d.y.asin())
    }

    /// Moves the light to the given angles at its current distance.
    pub fn set_angles(&mut self, azimuth: f32, elevation: f32) {
        let r = self.position.length();
        let (sa, ca) = azimuth.sin_cos();
        let (se, ce) = elevation.sin_cos();
        self.position = r * Vec3::new(sa * ce, se, ca * ce);
    }
}

/// A hemisphere light plus up to four directional lights, and the look of the backdrop.
#[derive(Clone, Debug, PartialEq)]
pub struct Lighting {
    pub sky: Vec3,
    pub ground: Vec3,
    pub hemi_intensity: f32,
    pub lights: Vec<DirLight>,
    /// Backdrop colour, sRGB 0..1 (it is cleared as-is, never tone mapped).
    pub background: [f32; 3],
    pub exposure: f32,
}

/// Half-extent, near and far of the shadow camera (as in the prototype).
const SHADOW_EXTENT: f32 = 0.8;
const SHADOW_NEAR: f32 = 0.1;
const SHADOW_FAR: f32 = 8.0;

fn backdrop(hex: u32) -> [f32; 3] {
    let c = |shift: u32| f32::from(u8::try_from((hex >> shift) & 0xff).unwrap_or(0)) / 255.0;
    [c(16), c(8), c(0)]
}

impl Lighting {
    /// `?light=studio`: neutral white key with a cool fill from behind.
    #[must_use]
    pub fn studio() -> Self {
        Self {
            sky: linear_from_hex(0x00ff_ffff),
            ground: linear_from_hex(0x0066_6677),
            hemi_intensity: 1.6,
            lights: vec![
                DirLight {
                    position: Vec3::new(1.2, 2.2, 2.4),
                    color: linear_from_hex(0x00ff_ffff),
                    intensity: 2.2,
                    casts_shadow: true,
                },
                DirLight {
                    position: Vec3::new(-2.0, 1.0, -1.5),
                    color: linear_from_hex(0x00c8_d4ff),
                    intensity: 0.6,
                    casts_shadow: false,
                },
            ],
            background: backdrop(0x008a_8f99),
            exposure: 1.0,
        }
    }

    /// `?light=night`: warm lantern key from the left, moonlit rim from behind.
    #[must_use]
    pub fn night() -> Self {
        Self {
            sky: linear_from_hex(0x0080_90c0),
            ground: linear_from_hex(0x0020_2030),
            hemi_intensity: 1.1,
            lights: vec![
                DirLight {
                    position: Vec3::new(-1.6, 1.8, 2.0),
                    color: linear_from_hex(0x00ff_cf98),
                    intensity: 2.4,
                    casts_shadow: true,
                },
                DirLight {
                    position: Vec3::new(2.0, 2.6, -2.0),
                    color: linear_from_hex(0x009f_b4ff),
                    intensity: 1.5,
                    casts_shadow: false,
                },
            ],
            background: backdrop(0x001b_2236),
            exposure: 1.0,
        }
    }

    /// `?light=warm`: a single peach key.
    #[must_use]
    pub fn warm() -> Self {
        Self {
            sky: linear_from_hex(0x00ff_e6c8),
            ground: linear_from_hex(0x0040_3028),
            hemi_intensity: 1.3,
            lights: vec![DirLight {
                position: Vec3::new(1.2, 1.5, 2.2),
                color: linear_from_hex(0x00ff_c890),
                intensity: 2.6,
                casts_shadow: true,
            }],
            background: backdrop(0x008a_8f99),
            exposure: 1.0,
        }
    }

    /// Looks a preset up by its prototype name.
    #[must_use]
    pub fn preset(name: &str) -> Option<Self> {
        match name {
            "studio" => Some(Self::studio()),
            "night" => Some(Self::night()),
            "warm" => Some(Self::warm()),
            _ => None,
        }
    }

    /// The light that owns the shadow map, if any.
    #[must_use]
    pub fn shadow_light(&self) -> Option<&DirLight> {
        self.lights.iter().find(|l| l.casts_shadow)
    }

    /// World to shadow-clip matrix: an orthographic camera at the shadow light looking at the origin.
    #[must_use]
    pub fn shadow_view_proj(&self) -> Mat4 {
        let position = self.shadow_light().map_or(Vec3::new(0.0, 3.0, 0.0), |l| l.position);
        // A light straight above would make look_at degenerate; nudge the up vector then.
        let up = if position.normalize_or(Vec3::Y).y.abs() > 0.999 { Vec3::Z } else { Vec3::Y };
        let view_matrix = view::look_at_mat4(position, Vec3::ZERO, up);
        let proj = directx::orthographic(
            -SHADOW_EXTENT,
            SHADOW_EXTENT,
            -SHADOW_EXTENT,
            SHADOW_EXTENT,
            SHADOW_NEAR,
            SHADOW_FAR,
        );
        proj * view_matrix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_decodes_to_linear() {
        assert!((linear_from_hex(0x00ff_ffff) - Vec3::ONE).length() < 1e-6);
        assert!(linear_from_hex(0x0000_0000).length() < 1e-6);
        // sRGB 0x80 is about 0.2159 linear.
        assert!((linear_from_hex(0x0080_8080).x - 0.2159).abs() < 1e-3);
    }

    #[test]
    fn light_angles_round_trip() {
        let mut l = Lighting::studio().lights[0];
        let before = l.position;
        let (az, el) = l.angles();
        l.set_angles(az, el);
        assert!((l.position - before).length() < 1e-5);
    }

    #[test]
    fn presets_are_found_by_name() {
        assert!(Lighting::preset("night").is_some());
        assert!(Lighting::preset("nope").is_none());
        assert!(Lighting::studio().shadow_light().is_some());
    }

    #[test]
    fn shadow_camera_contains_the_origin() {
        let clip = Lighting::studio().shadow_view_proj() * Vec3::ZERO.extend(1.0);
        let ndc = clip / clip.w;
        assert!(ndc.x.abs() < 1e-5 && ndc.y.abs() < 1e-5 && (0.0..1.0).contains(&ndc.z), "{ndc}");
    }
}
