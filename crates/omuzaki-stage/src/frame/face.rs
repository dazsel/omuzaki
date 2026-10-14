//! The face system's per-frame update: layer painting, variant selection by head coverage, settling for goldens, and
//! palette changes (#201).
//!
//! A stage gets a [`FaceRig`] with [`Stage::set_face`]; from then on step 5 of the frame ([`Stage::tick`]) does, in order:
//!
//! 1. **variant**: the head's size on screen ([`head_coverage`](Stage::head_coverage)) goes through `select_variant`
//!    (the rule behind `VariantSelector`, which borrows its set and so can't live in the stage). A new pick rebuilds
//!    through the [`CachingBuilder`], syncs the renderer ([`StageCharacter::sync`]: only what differs is sent), resets
//!    the runtimes and settles them;
//! 2. **map and paint**: [`FaceSystem::update`] maps the bus with the palette-resolved profile and says which layers
//!    changed; exactly those are painted into their textures with [`FaceLayerPainter`].
//!
//! A layer whose paint failed stays marked and is retried every frame until it succeeds, so a failing painter keeps the
//! old pixels on screen and never leaves the layer stale for good.

use std::sync::Arc;

use omuzaki_anim_api::Tick;
use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharacterBuilder, CharacterDef, JointRef, StepCtx, VariantId,
    VariantSet, resolve_variant, select_variant,
};
use omuzaki_core::glam::{Mat4, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::cpu::SkiaRasterizer;
use omuzaki_face::{FaceSystem, RealPainter};
use omuzaki_face_api::{FaceLayerId, FaceProfile, FaceProjection, FaceView, Palette};
use omuzaki_render_api::{Projection, Renderer};

use super::rig::{RigFrame, SkinPose};
use super::runtimes::DynamicMeshes;
use super::scene::ScenePlan;
use super::{FrameInput, Stage};
use crate::error::StageError;
use crate::upload::{MeshLookup, SyncReport};

/// Steps [`Stage::settle`] takes to warm hair and cloth before a golden frame.
pub const SETTLE_STEPS: u32 = 480;
/// The fixed step of [`Stage::settle`], seconds (90 Hz).
pub const SETTLE_DT: f32 = 1.0 / 90.0;

/// Makes the face system for a projection and a base density: called when the stage binds a face, when the head's
/// projection changes with a variant, and when [`Stage::resize_face`] changes the density.
pub type FaceFactory = Box<dyn Fn(&FaceProjection, f32) -> FaceSystem + Send>;

/// What rebuilds the character: the builder with its cache, the character before any variant, and the variants.
pub struct CharacterSource {
    builder: Arc<CachingBuilder>,
    base: CharacterDef,
    variants: VariantSet,
    options: BuildOptions,
    services: BuildServices,
}

impl CharacterSource {
    /// A source building `base` (the character without a variant) and its `variants` with `builder`.
    pub fn new(
        builder: Arc<CachingBuilder>,
        base: CharacterDef,
        variants: VariantSet,
        options: BuildOptions,
        services: BuildServices,
    ) -> Self {
        Self { builder, base, variants, options, services }
    }
}

/// Where the head is and how big, to read how much of the frame it fills.
struct Head {
    /// The head bone's index in the skin matrices.
    bone: usize,
    /// The bone's position in bind space.
    bind_position: Vec3,
    /// The head's height in metres: the face window's, which the head part fits to the head.
    height: f32,
}

/// The face bound to one built character.
struct Bound {
    system: FaceSystem,
    /// The face profile with palette slots resolved.
    profile: FaceProfile,
    projection: FaceProjection,
    head: Option<Head>,
}

/// What binding a rig to a character needs from it, worked out before anything is committed.
struct Resolved {
    profile: FaceProfile,
    projection: FaceProjection,
    head: Option<Head>,
}

/// The face of a stage: the face system and everything that rebuilds the character when the head's size or the palette
/// asks for it.
pub struct FaceRig {
    factory: FaceFactory,
    source: Option<CharacterSource>,
    settle_steps: u32,
    bound: Option<Bound>,
    /// Layers whose picture changed and whose texture was not painted yet (or failed to be).
    unpainted: [bool; FaceLayerId::COUNT],
}

impl FaceRig {
    /// A rig making its face systems with `factory`.
    pub fn new(factory: FaceFactory) -> Self {
        Self { factory, source: None, settle_steps: SETTLE_STEPS, bound: None, unpainted: [false; FaceLayerId::COUNT] }
    }

    /// The production rig: the real painter and the CPU rasteriser (the GPU one registers on the system later).
    pub fn standard() -> Self {
        Self::new(Box::new(|projection, px_per_m| {
            FaceSystem::new(Box::new(RealPainter::new(*projection, px_per_m)), Box::new(SkiaRasterizer::new()))
        }))
    }

    /// Lets the stage switch variants and apply palettes by rebuilding from `source`.
    #[must_use]
    pub fn with_source(mut self, source: CharacterSource) -> Self {
        self.source = Some(source);
        self
    }

    /// Steps taken to settle after a variant or palette change (default [`SETTLE_STEPS`]).
    #[must_use]
    pub fn with_settle_steps(mut self, steps: u32) -> Self {
        self.settle_steps = steps;
        self
    }

    /// Binds the rig to `built` at `px_per_m`: [`resolve`](Self::resolve), then [`commit_bound`](Self::commit_bound).
    fn bind(&mut self, built: &BuiltCharacter, px_per_m: f32) -> Result<(), StageError> {
        let resolved = Self::resolve(built)?;
        self.commit_bound(resolved, px_per_m);
        Ok(())
    }

    /// The part of binding that can fail: the face profile with its palette slots resolved, and the head. `None` for a
    /// character without a face.
    fn resolve(built: &BuiltCharacter) -> Result<Option<Resolved>, StageError> {
        let Some(face) = &built.face else { return Ok(None) };
        let profile = face
            .profile
            .resolve_colours(&built.palette)
            .map_err(|source| StageError::Face { reason: source.to_string() })?;
        Ok(Some(Resolved { profile, projection: face.projection, head: head_of(built, &face.projection) }))
    }

    /// Binds `resolved` (cannot fail): makes a new face system if there is none or the projection changed (else the old
    /// one keeps its fingerprints, so only layers whose picture changed repaint). No face unbinds it.
    fn commit_bound(&mut self, resolved: Option<Resolved>, px_per_m: f32) {
        let Some(Resolved { profile, projection, head }) = resolved else {
            self.bound = None;
            return;
        };
        let system = match self.bound.take() {
            Some(old) if old.projection == projection => old.system,
            _ => (self.factory)(&projection, px_per_m),
        };
        self.bound = Some(Bound { system, profile, projection, head });
    }
}

/// The head bone and size of `built`, if it has a head bone.
fn head_of(built: &BuiltCharacter, projection: &FaceProjection) -> Option<Head> {
    let bone = built.rig.bone(&JointRef::HEAD)?.index();
    let bind = built.rig.inverse_bind().get(bone)?.inverse();
    Some(Head { bone, bind_position: bind.w_axis.truncate(), height: projection.rect.y1 - projection.rect.y0 })
}

fn face_error(source: impl std::fmt::Display) -> StageError {
    StageError::Face { reason: source.to_string() }
}

fn rebuild_error(source: impl std::fmt::Display) -> StageError {
    StageError::Rebuild { reason: source.to_string() }
}

/// Steps every runtime `steps` times at [`SETTLE_DT`] under the given pose, with no wind.
fn step_runtimes(built: &mut BuiltCharacter, skin: &[Mat4], root: Mat4, steps: u32) {
    let pose = SkinPose { character: &built.rig, skin, root };
    for step in 0..steps {
        let time = Seconds(f64::from(step) * f64::from(SETTLE_DT));
        let ctx = StepCtx { dt: SETTLE_DT, time, pose: &pose, wind: Vec3::ZERO };
        DynamicMeshes::step(&mut built.runtimes, &ctx);
    }
}

impl Stage {
    /// Attaches the face system and binds it to the current character. Replaces an earlier rig.
    ///
    /// # Errors
    /// [`StageError::Face`] if the face profile names a palette slot the palette lacks.
    pub fn set_face(&mut self, mut rig: FaceRig) -> Result<(), StageError> {
        rig.bind(&self.built, self.character.face_px_per_m())?;
        self.face = Some(Box::new(rig));
        Ok(())
    }

    /// Warms the part runtimes (hair, cloth, tails) by `steps` steps of [`SETTLE_DT`] under the last frame's pose and
    /// root, so the first golden frame doesn't start from a rest shape ([`SETTLE_STEPS`] is the usual count). Nothing is
    /// sent to the renderer: the next [`tick`](Stage::tick) uploads the meshes that moved.
    pub fn settle(&mut self, steps: u32) {
        step_runtimes(&mut self.built, self.rig.skin(), self.root, steps);
    }

    /// Changes the face texture density (the frame-budget scheduler's knob) and repaints every layer on the next frame.
    ///
    /// # Errors
    /// As [`StageCharacter::resize_face`], and [`StageError::Face`] if the profile can't be resolved.
    pub fn resize_face(&mut self, renderer: &mut dyn Renderer, px_per_m: f32) -> Result<(), StageError> {
        let before = self.character.face_px_per_m();
        let resized = self.character.resize_face(renderer, &self.built, px_per_m);
        // A failed release of the old textures comes back after the new ones are in force: the face system must follow
        // the density that actually changed, and the release error is still the caller's to hear.
        if self.character.face_px_per_m().to_bits() != before.to_bits()
            && let Some(rig) = self.face.as_mut()
        {
            // A new system, because the painter's layer specs carry the density; the new textures are empty until its
            // first update reports every layer as changed.
            rig.bound = None;
            let bound = rig.bind(&self.built, self.character.face_px_per_m());
            resized?;
            return bound;
        }
        resized
    }

    /// Applies `palette` to the character: materials and face colours are re-resolved, the renderer's materials are
    /// updated in place, and no mesh is created unless a part baked a colour into its vertices. The simulations restart
    /// and settle, as after any rebuild. Needs a [`CharacterSource`] on the rig.
    ///
    /// # Errors
    /// [`StageError::Rebuild`] without a source or if the builder refuses the palette (a part names a slot it lacks),
    /// [`StageError::Render`] if the renderer refuses a call. On any error but a failed release of the old resources
    /// nothing has changed (the previous palette stays in force and a retry is sound); that one comes back after the
    /// palette took effect.
    pub fn apply_palette(&mut self, renderer: &mut dyn Renderer, palette: &Palette) -> Result<SyncReport, StageError> {
        let mut rig = self.face.take().ok_or_else(|| rebuild_error("no face rig attached: call set_face first"))?;
        let result = self.rebuild_for_palette(renderer, &mut rig, palette);
        self.face = Some(rig);
        result
    }

    fn rebuild_for_palette(
        &mut self,
        renderer: &mut dyn Renderer,
        rig: &mut FaceRig,
        palette: &Palette,
    ) -> Result<SyncReport, StageError> {
        let source = rig.source.as_ref().ok_or_else(|| rebuild_error("the face rig has no CharacterSource"))?;
        let mut def = self.built.def.clone();
        def.palette = palette.clone();
        let options = BuildOptions { variant: self.built.variant.clone(), ..source.options.clone() };
        let new = source.builder.build(&self.built.id, &def, &options, &source.services).map_err(rebuild_error)?;
        let (report, cleanup) = self.install(renderer, rig, new, None)?;
        // Only now is the palette the character's: a rejected install must not leak it into the next variant switch.
        if let Some(source) = rig.source.as_mut() {
            source.base.palette = palette.clone();
        }
        // The palette is in effect; a failed release of the old resources is the caller's to hear about.
        cleanup.map(|()| report)
    }

    /// The face half of step 5 of [`tick`](Stage::tick): variant, then map and paint. A no-op without a rig.
    pub(super) fn update_face(
        &mut self,
        renderer: &mut dyn Renderer,
        tick: Tick,
        input: &FrameInput,
    ) -> Result<(), StageError> {
        let Some(mut rig) = self.face.take() else { return Ok(()) };
        let result = self.drive_face(renderer, &mut rig, tick, input);
        self.face = Some(rig);
        result
    }

    fn drive_face(
        &mut self,
        renderer: &mut dyn Renderer,
        rig: &mut FaceRig,
        tick: Tick,
        input: &FrameInput,
    ) -> Result<(), StageError> {
        if let Some(target) = self.variant_to_switch_to(rig) {
            self.switch_variant(renderer, rig, &target, tick, input)?;
        }
        let Some(bound) = rig.bound.as_mut() else { return Ok(()) };
        // The gaze module's pure highlight shift is not available yet: no shift.
        let view = FaceView { highlight_shift: Vec2::ZERO };
        let update = bound.system.update(&input.channels, &bound.profile, tick.time, view).map_err(face_error)?;
        for layer in update.changed_layers() {
            rig.unpainted[layer as usize] = true;
        }
        let mut first_error = None;
        for layer in FaceLayerId::ALL {
            if !rig.unpainted[layer as usize] {
                continue;
            }
            // A layer no material composites has no texture: nothing to paint.
            let Some(texture) = self.character.face_layer(layer).map(|l| l.texture) else {
                rig.unpainted[layer as usize] = false;
                continue;
            };
            let Some(mut painter) = bound.system.layer_painter(layer) else { continue };
            match renderer.paint_texture(texture, &mut painter) {
                Ok(()) => rig.unpainted[layer as usize] = false,
                Err(source) => {
                    first_error.get_or_insert_with(|| {
                        StageError::render(format!("painting the {layer:?} face layer"), source)
                    });
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// The variant the head's size now calls for, if it differs from the one built. `None` without a source, a head, or
    /// a variant covering the size (the current one then stays).
    fn variant_to_switch_to(&self, rig: &FaceRig) -> Option<VariantId> {
        let source = rig.source.as_ref()?;
        let coverage = self.head_coverage(rig.bound.as_ref()?.head.as_ref()?)?;
        let pick = select_variant(&source.variants, coverage, self.built.variant.as_ref())?;
        (self.built.variant.as_ref() != Some(pick)).then(|| pick.clone())
    }

    /// The head's height over the frame's height at the head, from the camera and the head bone of the last solve.
    /// `None` for a head behind the camera.
    fn head_coverage(&self, head: &Head) -> Option<f64> {
        let model = self.rig.skin().get(head.bone)?.transform_point3(head.bind_position);
        let world = self.root.transform_point3(model);
        let camera = &self.scene.camera;
        let depth = (camera.position() - world).dot(camera.transform.z_axis.truncate());
        let frame_height = match camera.projection {
            Projection::Perspective { fov_y_deg, .. } if depth > 0.0 => {
                2.0 * depth * (fov_y_deg.to_radians() * 0.5).tan()
            }
            Projection::Perspective { .. } => return None,
            Projection::Orthographic { height, .. } => height,
        };
        let head_height = head.height * self.root.y_axis.truncate().length();
        (frame_height > f32::EPSILON).then(|| f64::from(head_height / frame_height))
    }

    fn switch_variant(
        &mut self,
        renderer: &mut dyn Renderer,
        rig: &mut FaceRig,
        target: &VariantId,
        tick: Tick,
        input: &FrameInput,
    ) -> Result<(), StageError> {
        let source = rig.source.as_ref().ok_or_else(|| rebuild_error("the face rig has no CharacterSource"))?;
        let def = resolve_variant(&source.base, &source.variants, target).map_err(rebuild_error)?;
        let options = BuildOptions { variant: Some(target.clone()), ..source.options.clone() };
        let new = source.builder.build(&self.built.id, &def, &options, &source.services).map_err(rebuild_error)?;
        let (_, cleanup) = self.install(renderer, rig, new, Some((tick, input)))?;
        if let Err(error) = cleanup {
            // The switch happened and the stage is coherent; the frame goes on, and the leak is only reported.
            tracing::warn!(%error, "releasing the old variant's resources failed");
        }
        Ok(())
    }

    /// Makes `new` the stage's character, all or nothing.
    ///
    /// Everything that can fail runs first, against the prospective state: the rig, the profile, the renderer sync
    /// ([`StageCharacter::prepare_sync`]), the dynamic-mesh refresh, the frame plans. Then the commit swaps tables and
    /// cannot fail. On an `Err` the previous character is untouched and coherent (its handles live, its plans current),
    /// so a later retry starts from a sound stage. `solve` re-solves the new rig when the skeleton changed.
    ///
    /// Returns the sync report and the outcome of releasing what the old character alone used: a failure there is
    /// reported but the new character is already in place.
    fn install(
        &mut self,
        renderer: &mut dyn Renderer,
        rig: &mut FaceRig,
        mut new: BuiltCharacter,
        solve: Option<(Tick, &FrameInput)>,
    ) -> Result<(SyncReport, Result<(), StageError>), StageError> {
        let mut new_rig = (new.rig != self.built.rig).then(|| RigFrame::new(&new.rig)).transpose()?;
        if let (Some(frame), Some((tick, input))) = (new_rig.as_mut(), solve) {
            frame.solve(tick, &input.pose, &input.goals);
        }
        let resolved = FaceRig::resolve(&new)?;
        let prepared = self.character.prepare_sync(renderer, &self.built, &new)?;

        let skin = new_rig.as_ref().map_or_else(|| self.rig.skin(), RigFrame::skin);
        let pose = SkinPose { character: &new.rig, skin, root: self.root };
        for slot in &mut new.runtimes {
            slot.runtime.reset(&pose);
        }
        step_runtimes(&mut new, skin, self.root, rig.settle_steps);
        let staged = DynamicMeshes::new(&new, &prepared).and_then(|dynamic| {
            let plan = ScenePlan::new(&new, &prepared)?;
            refresh_dynamic(&new, &prepared, renderer)?;
            Ok((dynamic, plan))
        });
        let (dynamic, plan) = match staged {
            Ok(staged) => staged,
            Err(error) => {
                prepared.abort(renderer);
                // The refresh may have rewritten meshes the old character shares: have its next step resend them.
                self.dynamic.resend_all();
                return Err(error);
            }
        };

        // Commit: nothing below can fail.
        let fresh: Vec<FaceLayerId> = prepared.fresh_face_layers(&self.character).collect();
        let retired = self.character.commit_sync(prepared);
        self.built = new;
        if let Some(frame) = new_rig {
            self.rig = frame;
        }
        self.dynamic = dynamic;
        self.plan = plan;
        rig.commit_bound(resolved, self.character.face_px_per_m());
        // A new layer texture is empty, and the face system's fingerprints still say its picture was shown.
        for layer in fresh {
            rig.unpainted[layer as usize] = true;
        }
        let report = retired.report;
        Ok((report, retired.release(renderer)))
    }
}

/// Sends the runtimes' current vertices for every dynamic mesh of `built`: a mesh kept across a rebuild still shows the
/// old simulation's last shape on the renderer.
fn refresh_dynamic(
    built: &BuiltCharacter,
    uploaded: &impl MeshLookup,
    renderer: &mut dyn Renderer,
) -> Result<(), StageError> {
    for slot in &built.runtimes {
        for index in 0..slot.runtime.dynamic_count() {
            let Some(view) = slot.runtime.dynamic_mesh(index) else { continue };
            let Some(mesh) = uploaded.mesh(&slot.part, view.mesh) else { continue };
            renderer
                .update_vertices(mesh.mesh, view.positions, view.normals)
                .map_err(|source| StageError::render("a dynamic mesh after a rebuild", source))?;
        }
    }
    Ok(())
}
