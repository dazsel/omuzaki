//! A cast on one stage: every visible member of a scene drawn in the same frame, each with its own body and frame loop.
//!
//! A [`Stage`] or [`AvatarStage`] runs one body: its rig, face, part runtimes and the scene it renders. A
//! [`CastStage`] holds a *lead* (the scene's subject, which owns the scene, the camera, the lights and the target) and
//! any number of other members, each a stage of its own keyed by its [`MemberId`]. A frame runs every member's frame
//! loop up to the render ([`StageMember::prepare`]), appends the members' instances, joints, morph weights and particles
//! to the lead's scene, and renders once: one render and one [`FrameReport`](omuzaki_render_api::FrameReport) for the
//! whole cast, so the frame-budget scheduler sees one frame, not one per member.
//!
//! - **Inputs.** Each member keeps the last [`FrameInput`] its performer gave it ([`CastStage::set_member_input`],
//!   or [`CastStage::tick_members`] for the whole cast). The frame's `dt` and time are the lead's, so the cast shares
//!   one clock.
//! - **Places.** Each member stands at its own root ([`CastStage::set_member_root`], or every member from the resolved
//!   figures with [`CastStage::set_member_roots`]), as the lead stands at the subject's.
//! - **One frame loop.** [`CastStage`] is a [`FrameLoop`], so [`LiveStage`](crate::perform::live::LiveStage) runs a
//!   cast as it runs one body: the subject's performer drives the lead through `tick`.
//!
//! The scene's skin floor reaches every member's skin materials when the cast pays the scene
//! ([`FrameLoop::pay_scene`]), as it reaches the lead's.
//!
//! The lead's own stage never sees the members: before every frame its scene is cut back to what the lead itself put
//! there, so its own invariants (a kit stage's empty morph weights, its particles) hold. Nothing here allocates in
//! steady state; adding a member is cold.

use std::collections::BTreeMap;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_core::glam::Mat4;
use omuzaki_face_api::ExpressionSet;
use omuzaki_render_api::{Quality, Renderer, Scene, TargetId};
use omuzaki_stage_api::MemberId;

use crate::avatar::AvatarStage;
use crate::error::StageError;
use crate::frame::{FrameInput, FrameOutput, Prepared, Stage};
use crate::scene::apply::Owed;
use crate::scene::resolve::{Figure, ResolvedScene};
use crate::stages::{AnyStage, FrameLoop};

mod sealed {
    pub trait Sealed {}
    impl Sealed for crate::frame::Stage {}
    impl Sealed for crate::avatar::AvatarStage {}
    impl Sealed for crate::stages::AnyStage {}
}

/// A body that can stand in a [`CastStage`]: its frame loop split at the render, so a cast renders all its members at
/// once. Implemented by [`Stage`], [`AvatarStage`] and [`AnyStage`].
pub trait StageMember: FrameLoop + sealed::Sealed {
    /// What [`FrameLoop::tick`] does before the render: the stage's scene holds the frame.
    ///
    /// # Errors
    /// As [`FrameLoop::tick`], short of the render.
    fn prepare(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<Prepared, StageError>;

    /// What [`FrameLoop::redraw`] does before the render.
    ///
    /// # Errors
    /// As [`FrameLoop::redraw`], short of the render.
    fn prepare_redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<Prepared, StageError>;

    /// Puts the scene's skin floor on the body's skin materials, or each back to its own for `None`: what a member, which
    /// has no scene of its own, takes from the lead's. Cold. A body without a skin floor (an imported avatar) ignores it.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses a material.
    fn set_skin_floor(&mut self, renderer: &mut dyn Renderer, floor: Option<f32>) -> Result<(), StageError>;

    /// Renders the scene a prepare left into the stage's target.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses the frame.
    fn submit(
        &mut self,
        renderer: &mut dyn Renderer,
        prepared: Prepared,
        what: &'static str,
    ) -> Result<FrameOutput, StageError>;

    /// Where humanoid `bone` of the body was in the last frame, in the world. `None` for a bone it lacks.
    fn humanoid_world(&self, bone: HumanoidBone) -> Option<Mat4>;

    /// Adds the scene's props attached to cast member `member` to the stage's scene, under `bone`'s answers: what the
    /// lead does for each member once that member's frame is posed. Hot: allocation-free.
    fn push_member_props(&mut self, member: &MemberId, bone: &dyn Fn(HumanoidBone) -> Option<Mat4>);
}

impl StageMember for Stage {
    fn prepare(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<Prepared, StageError> {
        self.prepare_tick(renderer, input)
    }
    fn prepare_redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<Prepared, StageError> {
        Stage::prepare_redraw(self, renderer, input)
    }
    fn set_skin_floor(&mut self, renderer: &mut dyn Renderer, floor: Option<f32>) -> Result<(), StageError> {
        Stage::set_skin_floor(self, renderer, floor)
    }
    fn submit(
        &mut self,
        renderer: &mut dyn Renderer,
        prepared: Prepared,
        what: &'static str,
    ) -> Result<FrameOutput, StageError> {
        Stage::submit(self, renderer, prepared, what)
    }
    fn humanoid_world(&self, bone: HumanoidBone) -> Option<Mat4> {
        self.bone_world(bone)
    }
    fn push_member_props(&mut self, member: &MemberId, bone: &dyn Fn(HumanoidBone) -> Option<Mat4>) {
        Stage::push_member_props(self, member, bone);
    }
}

impl StageMember for AvatarStage {
    fn prepare(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<Prepared, StageError> {
        self.prepare_tick(renderer, input)
    }
    fn prepare_redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<Prepared, StageError> {
        AvatarStage::prepare_redraw(self, renderer, input)
    }
    fn set_skin_floor(&mut self, _renderer: &mut dyn Renderer, _floor: Option<f32>) -> Result<(), StageError> {
        // An imported avatar's materials keep their own shading, as its own scene's floor does (`pay_scene`).
        Ok(())
    }
    fn submit(
        &mut self,
        renderer: &mut dyn Renderer,
        prepared: Prepared,
        what: &'static str,
    ) -> Result<FrameOutput, StageError> {
        AvatarStage::submit(self, renderer, prepared, what)
    }
    fn humanoid_world(&self, bone: HumanoidBone) -> Option<Mat4> {
        AvatarStage::humanoid_world(self, bone)
    }
    fn push_member_props(&mut self, member: &MemberId, bone: &dyn Fn(HumanoidBone) -> Option<Mat4>) {
        AvatarStage::push_member_props(self, member, bone);
    }
}

impl StageMember for AnyStage {
    fn prepare(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<Prepared, StageError> {
        match self {
            Self::Kit(stage) => StageMember::prepare(stage, renderer, input),
            Self::Avatar(stage) => StageMember::prepare(stage, renderer, input),
        }
    }
    fn prepare_redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<Prepared, StageError> {
        match self {
            Self::Kit(stage) => StageMember::prepare_redraw(stage, renderer, input),
            Self::Avatar(stage) => StageMember::prepare_redraw(stage, renderer, input),
        }
    }
    fn set_skin_floor(&mut self, renderer: &mut dyn Renderer, floor: Option<f32>) -> Result<(), StageError> {
        match self {
            Self::Kit(stage) => StageMember::set_skin_floor(stage, renderer, floor),
            Self::Avatar(stage) => StageMember::set_skin_floor(stage, renderer, floor),
        }
    }
    fn submit(
        &mut self,
        renderer: &mut dyn Renderer,
        prepared: Prepared,
        what: &'static str,
    ) -> Result<FrameOutput, StageError> {
        match self {
            Self::Kit(stage) => StageMember::submit(stage, renderer, prepared, what),
            Self::Avatar(stage) => StageMember::submit(stage, renderer, prepared, what),
        }
    }
    fn humanoid_world(&self, bone: HumanoidBone) -> Option<Mat4> {
        match self {
            Self::Kit(stage) => stage.bone_world(bone),
            Self::Avatar(stage) => stage.humanoid_world(bone),
        }
    }
    fn push_member_props(&mut self, member: &MemberId, bone: &dyn Fn(HumanoidBone) -> Option<Mat4>) {
        match self {
            Self::Kit(stage) => Stage::push_member_props(stage, member, bone),
            Self::Avatar(stage) => AvatarStage::push_member_props(stage, member, bone),
        }
    }
}

/// One member besides the lead: its body, the last input its performer gave it, and the skin floor its materials have.
struct Member<M> {
    id: MemberId,
    stage: M,
    input: FrameInput,
    /// The floor on its skin materials: `None` (their own) until the cast pays the scene's.
    floor: Option<f32>,
}

/// How far the lead's scene lists reach before the members are appended: what the lead itself put there.
#[derive(Copy, Clone, Debug, Default)]
struct Own {
    instances: usize,
    joints: usize,
    morph_weights: usize,
    particles: usize,
}

impl Own {
    fn of(scene: &Scene) -> Self {
        Self {
            instances: scene.instances.len(),
            joints: scene.joints.len(),
            morph_weights: scene.morph_weights.len(),
            particles: scene.particles.len(),
        }
    }

    /// Cuts `scene` back to these lengths: the members' part of the last frame goes, the lead's stays.
    fn restore(self, scene: &mut Scene) {
        scene.instances.truncate(self.instances);
        scene.joints.truncate(self.joints);
        scene.morph_weights.truncate(self.morph_weights);
        scene.particles.truncate(self.particles);
    }
}

/// Which half of a frame the cast runs.
#[derive(Copy, Clone)]
enum Pass {
    Tick,
    Redraw,
}

/// A scene's cast on one stage: the lead (the subject) and the other members, rendered together.
pub struct CastStage<M: StageMember = AnyStage> {
    lead_id: MemberId,
    lead: M,
    lead_input: FrameInput,
    /// The other members, in the order they were added; drawn after the lead in that order.
    members: Vec<Member<M>>,
    /// What the lead's scene held before the members were appended last frame.
    own: Own,
    /// The skin floor the last scene wants on every member, as on the lead.
    floor: Option<f32>,
}

impl<M: StageMember> std::fmt::Debug for CastStage<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CastStage")
            .field("lead", &self.lead_id)
            .field("members", &self.members.iter().map(|m| &m.id).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl<M: StageMember> CastStage<M> {
    /// A cast of one: `lead` is the scene's subject `lead_id`, and owns the scene, the target and the quality the
    /// whole cast renders with.
    pub fn new(lead_id: MemberId, lead: M) -> Self {
        let own = Own::of(lead.scene());
        Self { lead_id, lead, lead_input: FrameInput::default(), members: Vec::new(), own, floor: None }
    }

    /// The lead's member id.
    pub fn lead_id(&self) -> &MemberId {
        &self.lead_id
    }

    /// The lead's stage.
    pub fn lead(&self) -> &M {
        &self.lead
    }

    /// The lead's stage, to set its face, look or effects.
    pub fn lead_mut(&mut self) -> &mut M {
        &mut self.lead
    }

    /// Adds `stage` as member `id` standing at `root`, after the members already there; returns the stage it replaces
    /// for an `id` already in the cast, which the caller releases. The lead can't be replaced: `stage` comes back for
    /// the lead's id. Cold: the lead's scene reserves room for the member's draws.
    ///
    /// `stage` must render on the lead's renderer; its own target and scene camera are never used.
    pub fn add_member(&mut self, id: MemberId, mut stage: M, root: Mat4) -> Option<M> {
        if id == self.lead_id {
            return Some(stage);
        }
        stage.set_subject_root(root);
        stage.set_quality(*self.lead.quality());
        let theirs = stage.scene();
        let (instances, joints, weights, particles) = (
            theirs.instances.capacity(),
            theirs.joints.capacity(),
            theirs.morph_weights.capacity(),
            theirs.particles.capacity(),
        );
        let scene = self.lead.scene_mut();
        scene.instances.reserve(instances);
        scene.joints.reserve(joints);
        scene.morph_weights.reserve(weights);
        scene.particles.reserve(particles);
        if let Some(slot) = self.members.iter_mut().find(|m| m.id == id) {
            slot.floor = None;
            return Some(std::mem::replace(&mut slot.stage, stage));
        }
        self.members.push(Member { id, stage, input: FrameInput::default(), floor: None });
        None
    }

    /// Takes member `id` out of the cast, for the caller to release. `None` for the lead or an id not in the cast.
    pub fn remove_member(&mut self, id: &MemberId) -> Option<M> {
        let index = self.members.iter().position(|m| &m.id == id)?;
        Some(self.members.remove(index).stage)
    }

    /// Member `id`'s stage: the lead's for the lead's id.
    pub fn member(&self, id: &MemberId) -> Option<&M> {
        if *id == self.lead_id {
            return Some(&self.lead);
        }
        self.members.iter().find(|m| &m.id == id).map(|m| &m.stage)
    }

    /// Member `id`'s stage, to set its face, look or effects: the lead's for the lead's id.
    pub fn member_mut(&mut self, id: &MemberId) -> Option<&mut M> {
        if *id == self.lead_id {
            return Some(&mut self.lead);
        }
        self.members.iter_mut().find(|m| &m.id == id).map(|m| &mut m.stage)
    }

    /// Every member's id, the lead first, in draw order.
    pub fn ids(&self) -> impl Iterator<Item = &MemberId> {
        std::iter::once(&self.lead_id).chain(self.members.iter().map(|m| &m.id))
    }

    /// How many members the cast has, the lead included.
    pub fn len(&self) -> usize {
        1 + self.members.len()
    }

    /// Always false: a cast has its lead.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The expressions member `id`'s performer bakes its triggers against ([`FrameLoop::trigger_expressions`]). Cold.
    pub fn trigger_expressions_of(&self, id: &MemberId) -> Option<ExpressionSet> {
        self.member(id).map(FrameLoop::trigger_expressions)
    }

    /// Puts member `id` at `root` from the next frame (the lead: [`FrameLoop::set_subject_root`]). False for an id not
    /// in the cast.
    pub fn set_member_root(&mut self, id: &MemberId, root: Mat4) -> bool {
        match self.member_mut(id) {
            Some(stage) => {
                stage.set_subject_root(root);
                true
            }
            None => false,
        }
    }

    /// Puts every member that has a figure in `figures` (the visible cast resolve placed) at its figure's root, the lead
    /// included. Members without a figure stay where they were.
    pub fn set_member_roots(&mut self, figures: &BTreeMap<MemberId, Figure>) {
        if let Some(figure) = figures.get(&self.lead_id) {
            self.lead.set_subject_root(Mat4::from(figure.root));
        }
        for member in &mut self.members {
            if let Some(figure) = figures.get(&member.id) {
                member.stage.set_subject_root(Mat4::from(figure.root));
            }
        }
    }

    /// Gives member `id` the input its next frame runs with (the lead's too). Its `dt` and time are replaced by the
    /// lead's when the frame runs. False for an id not in the cast. Allocation-free.
    pub fn set_member_input(&mut self, id: &MemberId, input: &FrameInput) -> bool {
        if *id == self.lead_id {
            self.lead_input.clone_from(input);
            return true;
        }
        match self.members.iter_mut().find(|m| &m.id == id) {
            Some(member) => {
                member.input.clone_from(input);
                true
            }
            None => false,
        }
    }

    /// Runs one frame for the whole cast with the inputs given, each member keeping its last input when none is given
    /// for it, and renders it once into the lead's target. Ids not in the cast are skipped. The frame's `dt` and time
    /// are the lead's input's.
    ///
    /// # Errors
    /// The first error a member's frame reports, in draw order, and then the render's. The members that prepared have
    /// stepped; the next call carries on.
    pub fn tick_members(
        &mut self,
        renderer: &mut dyn Renderer,
        inputs: &[(MemberId, &FrameInput)],
    ) -> Result<FrameOutput, StageError> {
        for (id, input) in inputs {
            self.set_member_input(id, input);
        }
        self.run(renderer, Pass::Tick)
    }

    /// Prepares the lead, then every member, appends the members to the lead's scene and renders it.
    fn run(&mut self, renderer: &mut dyn Renderer, pass: Pass) -> Result<FrameOutput, StageError> {
        self.own.restore(self.lead.scene_mut());
        let (input, what) = (&self.lead_input, match pass {
            Pass::Tick => "the frame",
            Pass::Redraw => "the still",
        });
        let prepared = match pass {
            Pass::Tick => self.lead.prepare(renderer, input),
            Pass::Redraw => self.lead.prepare_redraw(renderer, input),
        };
        self.own = Own::of(self.lead.scene());
        let mut result = prepared;
        for member in &mut self.members {
            member.input.dt = self.lead_input.dt;
            member.input.time = self.lead_input.time;
            let prepared = match pass {
                Pass::Tick => member.stage.prepare(renderer, &member.input),
                Pass::Redraw => member.stage.prepare_redraw(renderer, &member.input),
            };
            // A member that failed still filled its scene with this frame's body, as a lone stage does.
            append(self.lead.scene_mut(), member.stage.scene());
            // The lead holds the scene's props; the ones in this member's hands follow its bones.
            let stage = &member.stage;
            self.lead.push_member_props(&member.id, &|bone| stage.humanoid_world(bone));
            if let (Ok(_), Err(error)) = (&result, prepared) {
                result = Err(error);
            }
        }
        let prepared = result?;
        self.lead.submit(renderer, prepared, what)
    }
}

impl<M: StageMember> CastStage<M> {
    /// Whether some member's skin materials don't have the floor the scene wants.
    fn members_owe_floor(&self) -> bool {
        self.members.iter().any(|m| m.floor != self.floor)
    }
}

/// Appends `member`'s draws to `scene`, moving its skin and morph ranges past what `scene` already holds. The member's
/// ground disc, if it has one, comes along: a member's stage is never given a scene, so it has none.
fn append(scene: &mut Scene, member: &Scene) {
    let joints = u32::try_from(scene.joints.len()).unwrap_or(u32::MAX);
    let weights = u32::try_from(scene.morph_weights.len()).unwrap_or(u32::MAX);
    scene.joints.extend_from_slice(&member.joints);
    scene.morph_weights.extend_from_slice(&member.morph_weights);
    scene.particles.extend_from_slice(&member.particles);
    scene.instances.extend(member.instances.iter().map(|instance| {
        let mut instance = *instance;
        if let Some(skin) = instance.skin.as_mut() {
            skin.start = skin.start.saturating_add(joints);
        }
        if let Some(morph) = instance.morph.as_mut() {
            morph.start = morph.start.saturating_add(weights);
        }
        instance
    }));
}

impl<M: StageMember> FrameLoop for CastStage<M> {
    /// Runs one frame for the cast: `input` is the lead's, every other member runs with its last input.
    fn tick(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        self.lead_input.clone_from(input);
        self.run(renderer, Pass::Tick)
    }

    fn redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        self.lead_input.clone_from(input);
        self.run(renderer, Pass::Redraw)
    }

    fn service(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<(), StageError> {
        let lead = self.lead.service(renderer, input);
        let mut members = Ok(());
        for member in &mut self.members {
            let result = member.stage.service(renderer, &member.input);
            if members.is_ok() {
                members = result;
            }
        }
        lead.and(members)
    }

    fn set_quality(&mut self, quality: Quality) {
        for member in &mut self.members {
            member.stage.set_quality(quality);
        }
        self.lead.set_quality(quality);
    }

    fn quality(&self) -> &Quality {
        self.lead.quality()
    }

    /// The lead takes the scene; every member takes its skin floor at the next [`pay_scene`](FrameLoop::pay_scene).
    fn set_scene(&mut self, resolved: &ResolvedScene, subject_root: Mat4) -> Owed {
        self.floor = resolved.skin_floor;
        let mut owed = self.lead.set_scene(resolved, subject_root);
        if self.members_owe_floor() {
            owed.skin_floor = Some(self.floor);
        }
        owed
    }

    fn set_subject_root(&mut self, subject_root: Mat4) {
        self.lead.set_subject_root(subject_root);
    }

    /// The lead's debt, and the skin floor while a member doesn't have it yet.
    fn owed(&self) -> Owed {
        let mut owed = self.lead.owed();
        if owed.skin_floor.is_none() && self.members_owe_floor() {
            owed.skin_floor = Some(self.floor);
        }
        owed
    }

    fn commit_scene(&mut self, owed: &Owed) {
        self.lead.commit_scene(owed);
    }

    fn pay_scene(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        let lead = self.lead.pay_scene(renderer);
        let mut members = Ok(());
        for member in self.members.iter_mut().filter(|m| m.floor != self.floor) {
            match member.stage.set_skin_floor(renderer, self.floor) {
                Ok(()) => member.floor = self.floor,
                Err(error) if members.is_ok() => members = Err(error),
                Err(_) => {}
            }
        }
        lead.and(members)
    }

    fn scene(&self) -> &Scene {
        self.lead.scene()
    }

    fn scene_mut(&mut self) -> &mut Scene {
        self.lead.scene_mut()
    }

    fn target(&self) -> TargetId {
        self.lead.target()
    }

    fn revision(&self) -> u64 {
        self.lead.revision()
    }

    fn trigger_expressions(&self) -> ExpressionSet {
        self.lead.trigger_expressions()
    }

    fn set_member_input(&mut self, id: &MemberId, input: &FrameInput) -> bool {
        CastStage::set_member_input(self, id, input)
    }

    fn trigger_expressions_of(&self, id: &MemberId) -> Option<ExpressionSet> {
        CastStage::trigger_expressions_of(self, id)
    }

    fn release(self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        let mut result = self.lead.release(renderer);
        for member in self.members {
            let released = member.stage.release(renderer);
            if result.is_ok() {
                result = released;
            }
        }
        result
    }
}
