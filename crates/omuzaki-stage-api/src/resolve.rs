//! The pure part of the light resolve (design D10 step 1): a [`Lighting`] and the anchor frames of its cast become
//! render-api [`Light`]s in world space, in a fixed-capacity buffer. The stage adds the cameras and the rest.

use omuzaki_core::glam::{Affine3A, Vec3};
use omuzaki_render_api::limits::MAX_LIGHTS;
use omuzaki_render_api::{Colour, Light};

use crate::anchor::Anchor;
use crate::error::SceneError;
use crate::lighting::{LightDef, LightShape, Lighting};

/// Where an anchor sits in the world: its units mapped to world space, and how many metres one of its units is.
///
/// `height_m` is the conversion from anchor units to metres (the member's height); the stage's is 1. `root` then maps
/// a point in local metres (feet at the origin, facing +Z) to world metres. It is a translation and a yaw, with no
/// scale: `height_m` alone turns units into metres, so scaling `root` as well would apply the height twice.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AnchorFrame {
    /// Local metres to world metres: the member's position and yaw, with no scale.
    pub root: Affine3A,
    /// Metres per anchor unit.
    pub height_m: f32,
}

impl AnchorFrame {
    /// The stage origin with fixed axes, and the 1 m figure at the origin facing +Z (design D4).
    const UNIT: Self = Self { root: Affine3A::IDENTITY, height_m: 1.0 };
}

/// The lights of a resolved rig: a fixed array of [`MAX_LIGHTS`] with a length, so resolving never allocates.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LightBuf {
    lights: [Light; MAX_LIGHTS],
    len: usize,
}

/// Filler for the unused slots of a [`LightBuf`]: never read, because `len` bounds every read.
const UNUSED: Light = Light::Hemisphere { sky: Colour::BLACK, ground: Colour::BLACK, intensity: 0.0 };

impl Default for LightBuf {
    fn default() -> Self {
        Self { lights: [UNUSED; MAX_LIGHTS], len: 0 }
    }
}

impl LightBuf {
    /// An empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The resolved lights in order: the ambient first, then the lights by id.
    #[must_use]
    pub fn as_slice(&self) -> &[Light] {
        &self.lights[..self.len]
    }

    /// How many lights are in the buffer.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when the buffer holds no lights.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends `light`. Refused, not dropped, when the buffer is full.
    fn push(&mut self, light: Light) -> Result<(), SceneError> {
        let slot = self.lights.get_mut(self.len).ok_or_else(|| {
            SceneError::new("lighting.lights", format!("the ambient and the lights take more than {MAX_LIGHTS}"))
        })?;
        *slot = light;
        self.len += 1;
        Ok(())
    }
}

impl Lighting {
    /// Resolves the rig into `out` (design D10 step 1): the ambient as a [`Light::Hemisphere`] first, then the lights
    /// in id order, each placed by its anchor. Returns how many lights were written. `out` is cleared first.
    ///
    /// `frames` answers the anchor frame of a `member:<id>` anchor, and of `subject` when there is a cast. For
    /// `stage` the frame is the stage's, whatever `frames` says. When `frames` returns `None` for `subject` (no cast),
    /// the subject is the 1 m figure at the origin facing +Z, so its units are metres. A `member:<id>` that `frames`
    /// doesn't know is a [`SceneError`] at `lighting.lights.<id>.anchor`.
    ///
    /// Directions: `azimuth` 0 is from +Z and +90 from +X; `elevation` is up from the horizon. The anchor's yaw turns
    /// them. Points and spots: `position`, a spot's `target` and `range` scale by the anchor's height, then go through
    /// its root; `intensity` scales by the height squared, so the light on a face is the same at any stature; a spot's
    /// cone and penumbra are angles and fractions, which don't scale. A directional or spot light's `shadow` becomes
    /// `casts_shadow`; a point light has no shadow in the render contract, so `shadow` is ignored for it.
    ///
    /// # Errors
    /// A `member:<id>` anchor `frames` doesn't know, or more lights than [`MAX_LIGHTS`] counting the ambient.
    pub fn resolve_into(
        &self,
        frames: &dyn Fn(&Anchor) -> Option<AnchorFrame>,
        out: &mut LightBuf,
    ) -> Result<usize, SceneError> {
        out.len = 0;
        out.push(Light::Hemisphere {
            sky: self.ambient.sky,
            ground: self.ambient.ground,
            intensity: self.ambient.intensity,
        })?;
        for (id, def) in &self.lights {
            let frame = frame_of(&def.anchor, frames).ok_or_else(|| {
                SceneError::new(format!("lighting.lights.{id}.anchor"), format!("`{}` isn't in the cast", def.anchor))
            })?;
            out.push(resolve_light(def, &frame))?;
        }
        Ok(out.len)
    }
}

/// The frame of an anchor. Stage is always the stage's own; subject falls back to the 1 m figure with no cast.
fn frame_of(anchor: &Anchor, frames: &dyn Fn(&Anchor) -> Option<AnchorFrame>) -> Option<AnchorFrame> {
    match anchor {
        Anchor::Stage => Some(AnchorFrame::UNIT),
        Anchor::Subject => Some(frames(anchor).unwrap_or(AnchorFrame::UNIT)),
        Anchor::Member(_) => frames(anchor),
    }
}

/// One light in world space, in the render-api's units.
fn resolve_light(def: &LightDef, frame: &AnchorFrame) -> Light {
    match def.shape {
        LightShape::Directional { azimuth, elevation } => {
            // Azimuth turns about the vertical from +Z toward +X, so the sine goes with X and the cosine with Z.
            let (az, el) = (azimuth.to_radians(), elevation.to_radians());
            let local = Vec3::new(el.cos() * az.sin(), el.sin(), el.cos() * az.cos());
            Light::Directional {
                to_light: frame.root.transform_vector3(local).normalize_or_zero(),
                colour: def.colour,
                intensity: def.intensity,
                casts_shadow: def.shadow,
            }
        }
        LightShape::Point { position, range } => {
            let h = frame.height_m;
            Light::Point {
                position: frame.root.transform_point3(Vec3::from_array(position) * h),
                colour: def.colour,
                intensity: def.intensity * h * h,
                range: range * h,
            }
        }
        LightShape::Spot { position, target, cone, penumbra, range } => {
            let h = frame.height_m;
            let lamp = frame.root.transform_point3(Vec3::from_array(position) * h);
            let aim = frame.root.transform_point3(Vec3::from_array(target) * h);
            Light::Spot {
                position: lamp,
                direction: aim - lamp,
                colour: def.colour,
                intensity: def.intensity * h * h,
                range: range * h,
                cone_outer_deg: cone,
                penumbra,
                casts_shadow: def.shadow,
            }
        }
    }
}
