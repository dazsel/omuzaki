//! The scene's ground on the renderer (design D2, D10 step 3): the shadow-catcher or floor disc a resolved scene owes, made
//! when the scene's ground changes and drawn under the subject every frame. Both stages hold one, so a kit character
//! and an imported avatar stand on the same ground.

use omuzaki_core::glam::Mat4;
use omuzaki_render_api::{AlphaMode, Colour, Instance, Material, MaterialId, MeshId, Renderer, Scene, Shading};
use omuzaki_stage_api::GroundKind;

use super::resolve::ResolvedGround;
use crate::error::StageError;
use crate::recipe::GroundRecipe;

/// The disc's edge segments: the stage's own ground (the golden manifest's `stage.ground`).
const SEGMENTS: u16 = GroundRecipe::STAGE.segments;

/// The ground disc on a renderer, or none.
#[derive(Debug, Default)]
pub(crate) struct StageGround {
    disc: Option<(MeshId, MaterialId)>,
}

impl StageGround {
    /// Makes the renderer's ground `ground`: the old disc is released and, for a shadow ground or a floor, a new one is
    /// made at the ground's radius (and a shadow ground's opacity, a floor's colour). Cold: it allocates on the renderer. On an error the stage has no ground.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses the disc, or [`StageError::Scene`]-like framing errors for a
    /// radius or opacity the recipe can't draw.
    pub(crate) fn set(&mut self, renderer: &mut dyn Renderer, ground: &ResolvedGround) -> Result<(), StageError> {
        self.release(renderer)?;
        if ground.kind == GroundKind::None {
            return Ok(());
        }
        let recipe = GroundRecipe { radius: ground.radius_m, segments: SEGMENTS, shadow_opacity: ground.opacity };
        let invalid = |e: crate::recipe::RecipeError| StageError::Framing { reason: format!("the ground: {e}") };
        let mesh = recipe.mesh().map_err(invalid)?;
        let material = if ground.kind == GroundKind::Floor {
            // A plain toon surface: lit like the cast, so a spot leaves its pool and the key its shadow. No rim: seen at
            // a grazing angle, a floor is all edge, and a rim (or the scene's back-light rim) would flood it.
            let mut floor = Material::toon(ground.colour);
            if let Shading::Toon(toon) = &mut floor.shading {
                toon.rim.strength = 0.0;
            }
            floor.receives_shadows = true;
            floor
        } else {
            let mut catcher = Material::toon(Colour::BLACK);
            catcher.shading = recipe.shading().map_err(invalid)?;
            catcher.alpha = AlphaMode::Blend;
            catcher.receives_shadows = true;
            catcher
        };
        let mesh = renderer.create_mesh(&mesh).map_err(|source| StageError::render("the ground disc", source))?;
        match renderer.create_material(&material) {
            Ok(material) => {
                self.disc = Some((mesh, material));
                Ok(())
            }
            Err(source) => {
                if let Err(cleanup) = renderer.release(mesh.into()) {
                    tracing::warn!(%cleanup, "releasing a ground disc whose material failed failed too");
                }
                Err(StageError::render("the ground material", source))
            }
        }
    }

    /// Adds the disc to `scene`, at the stage origin (the cast stands on `y = 0`), casting no shadow (a caster ground
    /// would widen the shadow map's fit). Hot path: one push into a reserved list.
    pub(crate) fn push(&self, scene: &mut Scene) {
        if let Some((mesh, material)) = self.disc {
            let mut instance = Instance::new(mesh, material, Mat4::IDENTITY);
            instance.casts_shadow = false;
            scene.instances.push(instance);
        }
    }

    /// Gives the disc back to `renderer`.
    ///
    /// # Errors
    /// [`StageError::Render`] with the first release the renderer refused.
    pub(crate) fn release(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        let Some((mesh, material)) = self.disc.take() else { return Ok(()) };
        let released = renderer.release(mesh.into());
        renderer.release(material.into()).map_err(|source| StageError::render("the ground material", source))?;
        released.map_err(|source| StageError::render("the ground disc", source))
    }
}
