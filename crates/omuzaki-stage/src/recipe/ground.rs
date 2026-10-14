//! The ground recipe: the stage's ground disc under the feet, which only shows the character's shadow, and its shading
//! program (`shadow-catcher`).

use std::collections::BTreeMap;

use omuzaki_core::glam::Vec3;
use omuzaki_core::id::Slug;
use omuzaki_core::mesh::MeshData;
use omuzaki_render_api::{ParamValue, ProgramParams, Shading};

use super::RecipeError;

/// The shadow-catcher program's name in the renderer's registry.
const SHADOW_CATCHER: &str = "shadow-catcher";
/// The program parameter that sets how dark the shadow is, `0..=1`.
const OPACITY: &str = "opacity";

/// The stage's ground: a flat disc in the `y = 0` plane, with a shadow-catcher material at a fixed opacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundRecipe {
    /// Disc radius, metres.
    pub radius: f32,
    /// Number of edge segments around the disc, at least 3.
    pub segments: u16,
    /// How dark the shadow on the disc is, `0..=1`.
    pub shadow_opacity: f32,
}

impl GroundRecipe {
    /// The stage's ground as the golden manifest's `stage.ground` gives it.
    pub const STAGE: Self = Self { radius: 0.8, segments: 48, shadow_opacity: 0.25 };

    /// The disc as a mesh: a centre vertex and a ring of `segments` vertices, normals up (+Y), triangles
    /// counter-clockwise seen from above.
    ///
    /// # Errors
    /// [`RecipeError::Invalid`] if the radius is not positive or there are fewer than three segments.
    pub fn mesh(&self) -> Result<MeshData, RecipeError> {
        if !(self.radius.is_finite() && self.radius > 0.0) {
            return Err(RecipeError::invalid("ground", format!("radius {} must be above 0", self.radius)));
        }
        if self.segments < 3 {
            return Err(RecipeError::invalid("ground", format!("{} segments; a disc needs at least 3", self.segments)));
        }
        let n = self.segments;
        let mut positions = Vec::with_capacity(usize::from(n) + 1);
        positions.push(Vec3::ZERO);
        for i in 0..n {
            let angle = std::f32::consts::TAU * f32::from(i) / f32::from(n);
            positions.push(Vec3::new(self.radius * angle.cos(), 0.0, self.radius * angle.sin()));
        }
        let mut indices = Vec::with_capacity(3 * usize::from(n));
        for i in 0..n {
            // The ring runs clockwise seen from above, so each triangle takes its two rim vertices in reverse order to
            // face +Y.
            let (a, b) = (1 + u32::from(i), 1 + u32::from((i + 1) % n));
            indices.extend([0, b, a]);
        }
        Ok(MeshData { normals: vec![Vec3::Y; usize::from(n) + 1], positions, indices, ..MeshData::default() })
    }

    /// The disc's material: the shadow-catcher program with `opacity` set to `shadow_opacity`. Until the renderer
    /// registers the program it draws as the toon fallback (D13).
    ///
    /// # Errors
    /// [`RecipeError::Invalid`] if the opacity is outside `0..=1`.
    pub fn shading(&self) -> Result<Shading, RecipeError> {
        if !(0.0..=1.0).contains(&self.shadow_opacity) {
            return Err(RecipeError::invalid(
                "ground",
                format!("shadow opacity {} must be in 0..=1", self.shadow_opacity),
            ));
        }
        let key = |name: &str| Slug::new(name).map_err(|e| RecipeError::invalid("ground", e.to_string()));
        let mut params: ProgramParams = BTreeMap::new();
        params.insert(key(OPACITY)?, ParamValue::Float(self.shadow_opacity));
        Ok(Shading::Program { name: key(SHADOW_CATCHER)?, params })
    }
}
