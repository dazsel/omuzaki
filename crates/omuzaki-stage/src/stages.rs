//! One frame loop for any body (`docs/design/byom.md` D1): [`FrameLoop`] is what the live loop, the perform loop and
//! every output need of a stage, and both stages implement it: the kit's [`Stage`] (a built character, its painted face
//! and part runtimes) and the [`AvatarStage`] (an imported avatar, its morphs and expressions). [`AnyStage`] holds
//! either, so an app swaps a kit character for an imported one without changing type, and nothing downstream branches
//! on where a body came from.

use omuzaki_core::glam::Mat4;
use omuzaki_face_api::ExpressionSet;
use omuzaki_render_api::{Quality, Renderer, Scene, TargetId};
use omuzaki_stage_api::MemberId;

use crate::avatar::AvatarStage;
use crate::error::StageError;
use crate::frame::{FrameInput, FrameOutput, Stage};
use crate::scene::apply::Owed;
use crate::scene::resolve::ResolvedScene;

/// A body on a renderer with its frame loop: what the live and perform loops drive.
pub trait FrameLoop {
    /// Runs one frame with `input` and renders it into the stage's target.
    ///
    /// # Errors
    /// What the stage reports; the simulations have stepped and the next call carries on.
    fn tick(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError>;

    /// Renders the last frame's pose again with `input`'s channels, advancing nothing.
    ///
    /// # Errors
    /// What the stage reports.
    fn redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError>;

    /// The cold half of a frame, run once per loop iteration outside the frame's deadline (a kit character's face
    /// variant switch). Nothing to do by default.
    ///
    /// # Errors
    /// What the stage reports.
    fn service(&mut self, _renderer: &mut dyn Renderer, _input: &FrameInput) -> Result<(), StageError> {
        Ok(())
    }

    /// Makes the renderer quality of the following frames `quality`.
    fn set_quality(&mut self, quality: Quality);

    /// The quality frames render at.
    fn quality(&self) -> &Quality;

    /// Takes a resolved scene in for the next frame, the body standing at `subject_root`; returns the work owed.
    fn set_scene(&mut self, resolved: &ResolvedScene, subject_root: Mat4) -> Owed;

    /// Moves the subject member alone.
    fn set_subject_root(&mut self, subject_root: Mat4);

    /// What the last scene still owes the renderer.
    fn owed(&self) -> Owed;

    /// Records that the renderer did the work in `owed`.
    fn commit_scene(&mut self, owed: &Owed);

    /// Does the renderer work the last scene owes that the stage can do itself (the ground disc) and commits it.
    ///
    /// # Errors
    /// What the renderer refuses; the work stays owed.
    fn pay_scene(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError>;

    /// The scene as last rendered.
    fn scene(&self) -> &Scene;

    /// The scene the next frame starts from.
    fn scene_mut(&mut self) -> &mut Scene;

    /// The target frames render into.
    fn target(&self) -> TargetId;

    /// Changes whenever the body's expressions may have (a kit character's variant swap). Never changes for a body
    /// that can't swap.
    fn revision(&self) -> u64 {
        0
    }

    /// The expressions the performer's triggers bake against: the kit face profile's, or none. Cold: clones.
    fn trigger_expressions(&self) -> ExpressionSet {
        ExpressionSet::default()
    }

    /// Gives cast member `member` the input its next frame runs with, for a stage that draws a cast
    /// ([`CastStage`](crate::cast::CastStage)); the live loop calls it for every member before each frame. False when
    /// the stage draws no such member, which a single body never does. Allocation-free.
    fn set_member_input(&mut self, _member: &MemberId, _input: &FrameInput) -> bool {
        false
    }

    /// The expressions cast member `member`'s performer bakes its triggers against, for a stage that draws a cast.
    /// `None` when it draws no such member. Cold: clones.
    fn trigger_expressions_of(&self, _member: &MemberId) -> Option<ExpressionSet> {
        None
    }

    /// Gives every resource back to `renderer`.
    ///
    /// # Errors
    /// The first release the renderer refused.
    fn release(self, renderer: &mut dyn Renderer) -> Result<(), StageError>
    where
        Self: Sized;
}

impl FrameLoop for Stage {
    fn tick(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        Stage::tick(self, renderer, input)
    }
    fn redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        Stage::redraw(self, renderer, input)
    }
    fn service(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<(), StageError> {
        self.service_variants(renderer, input)
    }
    fn set_quality(&mut self, quality: Quality) {
        Stage::set_quality(self, quality);
    }
    fn quality(&self) -> &Quality {
        Stage::quality(self)
    }
    fn set_scene(&mut self, resolved: &ResolvedScene, subject_root: Mat4) -> Owed {
        Stage::set_scene(self, resolved, subject_root)
    }
    fn set_subject_root(&mut self, subject_root: Mat4) {
        Stage::set_subject_root(self, subject_root);
    }
    fn owed(&self) -> Owed {
        Stage::owed(self)
    }
    fn commit_scene(&mut self, owed: &Owed) {
        Stage::commit_scene(self, owed);
    }
    fn pay_scene(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        Stage::pay_scene(self, renderer)
    }
    fn scene(&self) -> &Scene {
        Stage::scene(self)
    }
    fn scene_mut(&mut self) -> &mut Scene {
        Stage::scene_mut(self)
    }
    fn target(&self) -> TargetId {
        Stage::target(self)
    }
    fn revision(&self) -> u64 {
        Stage::revision(self)
    }
    fn trigger_expressions(&self) -> ExpressionSet {
        self.built().face.as_ref().map(|face| face.profile.expressions.clone()).unwrap_or_default()
    }
    fn release(self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        Stage::release(self, renderer)
    }
}

impl FrameLoop for AvatarStage {
    fn tick(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        AvatarStage::tick(self, renderer, input)
    }
    fn redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        AvatarStage::redraw(self, renderer, input)
    }
    fn set_quality(&mut self, quality: Quality) {
        AvatarStage::set_quality(self, quality);
    }
    fn quality(&self) -> &Quality {
        AvatarStage::quality(self)
    }
    fn set_scene(&mut self, resolved: &ResolvedScene, subject_root: Mat4) -> Owed {
        AvatarStage::set_scene(self, resolved, subject_root)
    }
    fn set_subject_root(&mut self, subject_root: Mat4) {
        AvatarStage::set_subject_root(self, subject_root);
    }
    fn owed(&self) -> Owed {
        AvatarStage::owed(self)
    }
    fn commit_scene(&mut self, owed: &Owed) {
        AvatarStage::commit_scene(self, owed);
    }
    fn pay_scene(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        AvatarStage::pay_scene(self, renderer)
    }
    fn scene(&self) -> &Scene {
        AvatarStage::scene(self)
    }
    fn scene_mut(&mut self) -> &mut Scene {
        AvatarStage::scene_mut(self)
    }
    fn target(&self) -> TargetId {
        AvatarStage::target(self)
    }
    fn release(self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        AvatarStage::release(self, renderer)
    }
}

/// Either stage, so one live loop can run whichever body the person picked.
#[allow(clippy::large_enum_variant)] // one per app, moved rarely: boxing would only add an indirection to every frame
pub enum AnyStage {
    /// A kit character.
    Kit(Stage),
    /// An imported avatar.
    Avatar(AvatarStage),
}

impl std::fmt::Debug for AnyStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kit(_) => f.write_str("AnyStage::Kit"),
            Self::Avatar(stage) => write!(f, "AnyStage::Avatar({stage:?})"),
        }
    }
}

impl From<Stage> for AnyStage {
    fn from(stage: Stage) -> Self {
        Self::Kit(stage)
    }
}

impl From<AvatarStage> for AnyStage {
    fn from(stage: AvatarStage) -> Self {
        Self::Avatar(stage)
    }
}

/// Calls the same method on whichever stage `self` holds.
macro_rules! either {
    ($self:expr, $stage:ident => $call:expr) => {
        match $self {
            AnyStage::Kit($stage) => $call,
            AnyStage::Avatar($stage) => $call,
        }
    };
}

impl AnyStage {
    /// Runs the surface fx `fx` (a burn, a dissolve, turning to stone) over the body's materials, sparing those whose
    /// names contain any of `spare`, or takes it off with `None`. Cold.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses a material; the materials are as they were.
    pub fn set_surface_fx(
        &mut self,
        renderer: &mut dyn Renderer,
        fx: Option<omuzaki_render_api::SurfaceFx>,
        spare: &[String],
    ) -> Result<(), StageError> {
        either!(self, s => s.set_surface_fx(renderer, fx, spare))
    }
}

impl FrameLoop for AnyStage {
    fn tick(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        either!(self, s => FrameLoop::tick(s, renderer, input))
    }
    fn redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        either!(self, s => FrameLoop::redraw(s, renderer, input))
    }
    fn service(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<(), StageError> {
        either!(self, s => FrameLoop::service(s, renderer, input))
    }
    fn set_quality(&mut self, quality: Quality) {
        either!(self, s => FrameLoop::set_quality(s, quality));
    }
    fn quality(&self) -> &Quality {
        either!(self, s => FrameLoop::quality(s))
    }
    fn set_scene(&mut self, resolved: &ResolvedScene, subject_root: Mat4) -> Owed {
        either!(self, s => FrameLoop::set_scene(s, resolved, subject_root))
    }
    fn set_subject_root(&mut self, subject_root: Mat4) {
        either!(self, s => FrameLoop::set_subject_root(s, subject_root));
    }
    fn owed(&self) -> Owed {
        either!(self, s => FrameLoop::owed(s))
    }
    fn commit_scene(&mut self, owed: &Owed) {
        either!(self, s => FrameLoop::commit_scene(s, owed));
    }
    fn pay_scene(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        either!(self, s => FrameLoop::pay_scene(s, renderer))
    }
    fn scene(&self) -> &Scene {
        either!(self, s => FrameLoop::scene(s))
    }
    fn scene_mut(&mut self) -> &mut Scene {
        either!(self, s => FrameLoop::scene_mut(s))
    }
    fn target(&self) -> TargetId {
        either!(self, s => FrameLoop::target(s))
    }
    fn revision(&self) -> u64 {
        either!(self, s => FrameLoop::revision(s))
    }
    fn trigger_expressions(&self) -> ExpressionSet {
        either!(self, s => FrameLoop::trigger_expressions(s))
    }
    fn release(self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        either!(self, s => FrameLoop::release(s, renderer))
    }
}
