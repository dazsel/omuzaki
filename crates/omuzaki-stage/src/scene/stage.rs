//! [`SceneStage`]: a scene of the document on one stage, as the studio and the outputs draw it (design D1, D10, #2401):
//! every visible cast member's body built from its character and drawn together ([`CastStage`]), the touches between
//! them (#2415), and the scene's backdrop, ground, lights, cameras, post and props resolved around them.
//!
//! It is made once from the document ([`SceneStage::open`]) and then follows it ([`SceneStage::sync`]): an edit to the
//! scene re-resolves it, which moves values and uploads only what changed (a prop's build, a ground, a backdrop
//! picture); a cast member added, removed, re-cast or whose character changed is built again alone; a new subject, or
//! an edit to the subject's character, stages the whole cast again. Nothing here runs per frame: the frames are the
//! cast's own ([`FrameLoop`]), and the stage is one.
//!
//! The bodies come from a [`BodySource`]: [`Bodies`] loads them as every render does (`load_body`), and an app passes
//! its own to dress them (the studio's starter expressions) or a test to count them.

use std::collections::BTreeMap;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_char_api::{CharId, Quality as BuildQuality, VariantId, paths as char_paths};
use omuzaki_core::glam::{Mat4, UVec2};
use omuzaki_doc_api::DocRead;
use omuzaki_face_api::ExpressionSet;
use omuzaki_ops_api::OpError;
use omuzaki_render_api::{AlphaEncoding, Camera, Quality, Renderer, Scene, TargetDesc, TargetId};
use omuzaki_stage_api::{Framing, MemberId, SceneDef, SceneId};
use omuzaki_store_api::Store;
use serde_json::Value;

use super::alone::post_in_look;
use super::apply::Owed;
use super::default::scene_for;
use super::landmarks::Landmarks;
use super::resolve::{CheckedScene, Figure, ResolvedScene};
use crate::body::{Body, BodyServices, load_body};
use crate::cast::{CastStage, touches_of};
use crate::error::StageError;
use crate::frame::face::FaceRig;
use crate::frame::{FrameInput, FrameOutput, StageClock};
use crate::stages::{AnyStage, FrameLoop};
use crate::upload::UploadOptions;

/// The side of the target a member other than the lead is made with: the cast draws it into the lead's target, so its
/// own is never drawn into, and the smallest a renderer takes does.
const MEMBER_TARGET: u32 = 8;

/// Where a scene's bodies come from.
pub trait BodySource {
    /// Character `id` of `doc` with `variant` applied, ready to stage.
    ///
    /// # Errors
    /// Why it can't be loaded: what [`load_body`] reports.
    fn load(&mut self, doc: &dyn DocRead, id: &CharId, variant: Option<&VariantId>) -> Result<Body, OpError>;

    /// The asset store an image backdrop or an HDRI is read from; `None` leaves such a scene unable to resolve.
    fn store(&self) -> Option<&dyn Store> {
        None
    }
}

/// The bodies every render loads ([`load_body`]), built at `quality`.
#[derive(Clone, Copy, Debug)]
pub struct Bodies<'a> {
    services: BodyServices<'a>,
    quality: BuildQuality,
}

impl<'a> Bodies<'a> {
    /// Bodies loaded through `services`, kit characters built at `quality`.
    #[must_use]
    pub fn new(services: BodyServices<'a>, quality: BuildQuality) -> Self {
        Self { services, quality }
    }
}

impl BodySource for Bodies<'_> {
    fn load(&mut self, doc: &dyn DocRead, id: &CharId, variant: Option<&VariantId>) -> Result<Body, OpError> {
        load_body(doc, &self.services, id, variant, self.quality)
    }

    fn store(&self) -> Option<&dyn Store> {
        Some(self.services.store)
    }
}

/// How a [`SceneStage`] stages its bodies and draws.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct SceneStageOptions {
    /// The face atlas density of kit characters.
    pub upload: UploadOptions,
    /// The quality frames render at.
    pub quality: Quality,
    /// Where a frame's time comes from.
    pub clock: StageClock,
    /// The face rig every kit character's painted face runs on; `None` draws the faces still.
    pub face: Option<fn() -> FaceRig>,
    /// The framing a project with no scene of its own opens on, among the built-in default's cameras; `None` keeps
    /// the default's own (`full`). A stored scene always opens on its own camera.
    pub unsaved_framing: Option<Framing>,
}

impl SceneStageOptions {
    /// Frames at `quality` on `clock`, faces on the standard rig.
    #[must_use]
    pub fn new(quality: Quality, clock: StageClock) -> Self {
        Self { upload: UploadOptions::default(), quality, clock, face: Some(FaceRig::standard), unsaved_framing: None }
    }

    /// The same, opening a project with no scene on the default scene's camera of `framing` (the live view's bust).
    #[must_use]
    pub fn with_unsaved_framing(mut self, framing: Framing) -> Self {
        self.unsaved_framing = Some(framing);
        self
    }

    /// The same, with kit faces on `face`'s rig (a test's inline executor), or still for `None`.
    #[must_use]
    pub fn with_face(mut self, face: Option<fn() -> FaceRig>) -> Self {
        self.face = face;
        self
    }

    /// The same, with kit faces painted at `upload`'s density.
    #[must_use]
    pub fn with_upload(mut self, upload: UploadOptions) -> Self {
        self.upload = upload;
        self
    }
}

/// Why a scene couldn't be staged or followed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SceneStageError {
    /// The scene couldn't be read from the document.
    #[error("the scene can't be read: {0}")]
    Scene(OpError),
    /// The scene has nobody in it to draw: no visible cast member to stand as its subject.
    #[error("the scene has no visible cast member to stage")]
    NoCast,
    /// A cast member's character couldn't be loaded.
    #[error("cast member `{member}` can't be staged: {source}")]
    Member {
        /// The member.
        member: MemberId,
        /// Why its body didn't load.
        source: OpError,
    },
    /// The stage or the renderer refused.
    #[error(transparent)]
    Stage(#[from] StageError),
}

/// What a member was built from: a change to any of it builds the member again.
#[derive(Clone, Debug, PartialEq)]
struct BodyKey {
    character: CharId,
    variant: Option<VariantId>,
    /// `characters/<id>` as the document held it.
    node: Option<Value>,
}

impl BodyKey {
    fn of(doc: &dyn DocRead, character: &CharId, variant: Option<&VariantId>) -> Self {
        Self {
            character: character.clone(),
            variant: variant.cloned(),
            node: doc.get(&char_paths::character(character)).cloned(),
        }
    }
}

/// A member on the stage: what it was built from, what framing measured on it, the pose it stands in, and the target it
/// was made with (none for the lead, whose target is the caller's).
#[derive(Debug)]
struct Staged {
    key: BodyKey,
    landmarks: Landmarks,
    base: HumanoidPose,
    target: Option<TargetId>,
}

/// What a [`SceneStage::sync`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Synced {
    /// The whole cast was staged again: a new subject, or the subject's character changed.
    pub restaged: bool,
    /// The members built again, added or taken off, in id order.
    pub members: Vec<MemberId>,
    /// The scene was resolved again: the next frame shows its new values.
    pub resolved: bool,
}

impl Synced {
    /// Whether anything changed: the picture should be drawn again.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.restaged || self.resolved || !self.members.is_empty()
    }
}

/// A scene of the document staged whole: its cast on one [`CastStage`], its settings resolved around them.
pub struct SceneStage {
    cast: CastStage<AnyStage>,
    /// The scene asked for; `None` follows the one the project opens in.
    asked: Option<SceneId>,
    /// The scene staged: `None` for the built-in default of a project with no scene.
    id: Option<SceneId>,
    def: SceneDef,
    members: BTreeMap<MemberId, Staged>,
    resolved: ResolvedScene,
    aspect: f32,
    options: SceneStageOptions,
    /// The visible members left off the stage at the last open or sync, and why.
    skipped: Vec<(MemberId, OpError)>,
}

impl std::fmt::Debug for SceneStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneStage")
            .field("scene", &self.id)
            .field("members", &self.members.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl SceneStage {
    /// Stages `scene` of `doc` (the one the project opens in, for `None`, or the built-in default when it has none)
    /// into `target`, whose sides are `aspect` wide to 1 high: each visible member's body from `bodies`, the subject as
    /// the lead. Cold: builds and uploads every body.
    ///
    /// # Errors
    /// [`SceneStageError`]; nothing is left on the renderer then (the target stays the caller's).
    pub fn open(
        doc: &dyn DocRead,
        scene: Option<&SceneId>,
        bodies: &mut dyn BodySource,
        renderer: &mut dyn Renderer,
        target: TargetId,
        aspect: f32,
        options: SceneStageOptions,
    ) -> Result<Self, SceneStageError> {
        let (id, def) = read_scene(doc, scene, &options)?;
        let lead_id = subject(&def).ok_or(SceneStageError::NoCast)?;
        let (lead, staged) = stage_member(doc, &def, &lead_id, bodies, renderer, Some(target), &options)?;
        let mut this = Self {
            cast: CastStage::new(lead_id.clone(), lead),
            asked: scene.cloned(),
            id,
            def,
            members: BTreeMap::from([(lead_id.clone(), staged)]),
            resolved: ResolvedScene::new(),
            aspect,
            options,
            skipped: Vec::new(),
        };
        this.cast.set_member_input(&lead_id, &this.base_input(&lead_id));
        let built = this.add_members(doc, bodies, renderer, &mut Vec::new()).and_then(|()| this.resolve(bodies.store(), renderer));
        match built {
            Ok(()) => Ok(this),
            Err(error) => {
                if let Err(cleanup) = this.release_all(renderer) {
                    tracing::warn!(%cleanup, "releasing a scene that failed to stage failed too");
                }
                Err(error)
            }
        }
    }

    /// Follows the document: what changed since the last open or sync is staged again, and only that (see the module
    /// documentation). Cold.
    ///
    /// # Errors
    /// [`SceneStageError`]. The stage keeps drawing what it had: a member that failed to build stays off it, a scene
    /// that failed to resolve keeps its last values, and the next sync tries again.
    pub fn sync(
        &mut self,
        doc: &dyn DocRead,
        bodies: &mut dyn BodySource,
        renderer: &mut dyn Renderer,
    ) -> Result<Synced, SceneStageError> {
        let (id, def) = read_scene(doc, self.asked.as_ref(), &self.options)?;
        let lead_id = subject(&def).ok_or(SceneStageError::NoCast)?;
        let lead_key = def.cast.get(&lead_id).map(|m| BodyKey::of(doc, &m.character, m.variant.as_ref()));
        if lead_id != *self.cast.lead_id() || lead_key.as_ref() != self.members.get(&lead_id).map(|s| &s.key) {
            let target = self.cast.target();
            let fresh = Self::open(doc, self.asked.as_ref(), bodies, renderer, target, self.aspect, self.options)?;
            let old = std::mem::replace(self, fresh);
            old.release(renderer)?;
            let members = self.members.keys().cloned().collect();
            return Ok(Synced { restaged: true, members, resolved: true });
        }
        let scene_changed = def != self.def || id != self.id;
        self.id = id;
        self.def = def;
        let mut members = self.drop_members(doc, renderer)?;
        // The members staged before a renderer failure stay on the stage, so the scene is resolved around them first.
        let added = self.add_members(doc, bodies, renderer, &mut members);
        let resolved = if scene_changed || !members.is_empty() {
            self.resolve(bodies.store(), renderer)?;
            true
        } else {
            false
        };
        added?;
        members.sort();
        members.dedup();
        Ok(Synced { restaged: false, members, resolved })
    }

    /// Frames the scene's cameras for a target `aspect` wide to 1 high from now on. Cold.
    ///
    /// # Errors
    /// What resolving the scene again reports; the cameras keep their last framing then.
    pub fn set_aspect(
        &mut self,
        aspect: f32,
        store: Option<&dyn Store>,
        renderer: &mut dyn Renderer,
    ) -> Result<(), SceneStageError> {
        if (aspect - self.aspect).abs() <= f32::EPSILON {
            return Ok(());
        }
        self.aspect = aspect;
        self.resolve(store, renderer)
    }

    /// The scene staged: `None` for the built-in default of a project with no scene.
    pub fn scene_id(&self) -> Option<&SceneId> {
        self.id.as_ref()
    }

    /// The scene as last staged.
    pub fn def(&self) -> &SceneDef {
        &self.def
    }

    /// The scene in render values, as last resolved.
    pub fn resolved(&self) -> &ResolvedScene {
        &self.resolved
    }

    /// The cast drawing it.
    pub fn cast(&self) -> &CastStage<AnyStage> {
        &self.cast
    }

    /// The cast, to drive a member or reach a body.
    pub fn cast_mut(&mut self) -> &mut CastStage<AnyStage> {
        &mut self.cast
    }

    /// The subject's member id: the lead of the cast, whom the scene's anchors and the live performer follow.
    pub fn subject(&self) -> &MemberId {
        self.cast.lead_id()
    }

    /// The pose member `id` stands in when nothing drives it: its body's bind, arms at ease for an imported one.
    pub fn base_pose(&self, id: &MemberId) -> Option<HumanoidPose> {
        self.members.get(id).map(|staged| staged.base)
    }

    /// The visible members left off the stage at the last open or sync because their body didn't load, and why: a
    /// character that is missing or doesn't build. The rest of the cast is drawn.
    pub fn skipped(&self) -> &[(MemberId, OpError)] {
        &self.skipped
    }

    /// The members on the stage, the subject among them, in id order.
    pub fn members(&self) -> impl Iterator<Item = &MemberId> {
        self.members.keys()
    }

    /// What `id` stands in until a performer drives it.
    fn base_input(&self, id: &MemberId) -> FrameInput {
        FrameInput { pose: self.base_pose(id).unwrap_or(HumanoidPose::REST), ..FrameInput::default() }
    }

    /// Takes off the members the scene no longer has visible, or whose character changed. Returns them.
    fn drop_members(
        &mut self,
        doc: &dyn DocRead,
        renderer: &mut dyn Renderer,
    ) -> Result<Vec<MemberId>, SceneStageError> {
        let lead = self.cast.lead_id().clone();
        let gone: Vec<MemberId> = self
            .members
            .iter()
            .filter(|(id, _)| **id != lead)
            .filter(|(id, staged)| match self.def.cast.get(*id) {
                Some(member) if member.visible => {
                    staged.key != BodyKey::of(doc, &member.character, member.variant.as_ref())
                }
                _ => true,
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut result = Ok(());
        for id in &gone {
            let staged = self.members.remove(id);
            let released = self.cast.remove_member(id).map_or(Ok(()), |stage| stage.release(renderer));
            let target = staged.and_then(|s| s.target).map_or(Ok(()), |t| release_target(renderer, t));
            if result.is_ok() {
                result = released.and(target);
            }
        }
        result.map(|()| gone).map_err(SceneStageError::Stage)
    }

    /// Stages the visible members the cast doesn't have yet, adding each to `added`. One whose body doesn't load is
    /// skipped ([`skipped`](SceneStage::skipped)); a renderer failure ends it, with the members staged so far kept.
    fn add_members(
        &mut self,
        doc: &dyn DocRead,
        bodies: &mut dyn BodySource,
        renderer: &mut dyn Renderer,
        added: &mut Vec<MemberId>,
    ) -> Result<(), SceneStageError> {
        let missing: Vec<MemberId> = self
            .def
            .cast
            .iter()
            .filter(|(id, member)| member.visible && !self.members.contains_key(*id))
            .map(|(id, _)| id.clone())
            .collect();
        self.skipped.clear();
        for id in missing {
            // A member whose character is missing or doesn't build stays off the stage and is reported; the rest of the
            // cast is drawn (design D6). The next sync tries it again.
            let (stage, staged) = match stage_member(doc, &self.def, &id, bodies, renderer, None, &self.options) {
                Ok(staged) => staged,
                Err(SceneStageError::Member { member, source }) => {
                    tracing::warn!(%member, %source, "a cast member can't be staged: the scene is drawn without it");
                    self.skipped.push((member, source));
                    continue;
                }
                Err(error) => return Err(error),
            };
            let root = Mat4::from(Figure::new(&self.def.cast[&id], staged.landmarks).root);
            self.members.insert(id.clone(), staged);
            if let Some(replaced) = self.cast.add_member(id.clone(), stage, root) {
                replaced.release(renderer)?;
            }
            self.cast.set_member_input(&id, &self.base_input(&id));
            added.push(id);
        }
        Ok(())
    }

    /// Resolves the scene around the members on the stage and hands it to the cast: placements, touches, and the
    /// renderer work it owes. Cold.
    fn resolve(&mut self, store: Option<&dyn Store>, renderer: &mut dyn Renderer) -> Result<(), SceneStageError> {
        let figures: BTreeMap<MemberId, Figure> = self
            .members
            .iter()
            .filter_map(|(id, staged)| self.def.cast.get(id).map(|m| (id.clone(), Figure::new(m, staged.landmarks))))
            .collect();
        self.resolved.resolve_into(CheckedScene::new(&self.def)?, &figures, self.aspect)?;
        if let Some(store) = store {
            if self.resolved.environment.and_then(|e| e.hdri()).is_some() {
                self.resolved.load_hdri(store)?;
            }
            if self.resolved.backdrop_image.is_some() {
                self.resolved.load_backdrop(store)?;
            }
        }
        if let AnyStage::Kit(lead) = self.cast.lead() {
            // A kit character's look sets part of the frame too, as in every render of it.
            post_in_look(&mut self.resolved.post, &lead.built().look().scene);
        }
        let lead = self.cast.lead_id().clone();
        let root = figures.get(&lead).map_or(Mat4::IDENTITY, |figure| Mat4::from(figure.root));
        self.cast.set_member_roots(&figures);
        self.cast.set_touches(touches_of(&self.def.cast));
        self.cast.set_scene(&self.resolved, root);
        self.cast.pay_scene(renderer)?;
        Ok(())
    }

    /// Gives back every body, and the targets the members were made with.
    fn release_all(self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        let targets: Vec<TargetId> = self.members.values().filter_map(|s| s.target).collect();
        let mut result = self.cast.release(renderer);
        for target in targets {
            let released = release_target(renderer, target);
            if result.is_ok() {
                result = released;
            }
        }
        result
    }
}

/// Scene `scene` of `doc` ([`scene_for`]), the built-in default opened on `options`' framing when it is the one.
fn read_scene(
    doc: &dyn DocRead,
    scene: Option<&SceneId>,
    options: &SceneStageOptions,
) -> Result<(Option<SceneId>, SceneDef), SceneStageError> {
    let (id, mut def) = scene_for(doc, scene).map_err(SceneStageError::Scene)?;
    if id.is_none()
        && let Some(framing) = options.unsaved_framing
        && let Some(camera) = def.cameras.iter().find(|(_, c)| c.framing == framing).map(|(id, _)| id.clone())
    {
        def.scene.camera = Some(camera);
    }
    Ok((id, def))
}

/// The scene's subject: the member it names, else the first of its cast; `None` when that member is hidden or there is
/// none.
fn subject(def: &SceneDef) -> Option<MemberId> {
    let id = def.scene.subject.as_ref().or_else(|| def.cast.keys().next())?;
    def.cast.get(id).filter(|m| m.visible).map(|_| id.clone())
}

/// Loads member `id`'s body and stages it: into `target` for the lead, into a small target of its own otherwise.
fn stage_member(
    doc: &dyn DocRead,
    def: &SceneDef,
    id: &MemberId,
    bodies: &mut dyn BodySource,
    renderer: &mut dyn Renderer,
    target: Option<TargetId>,
    options: &SceneStageOptions,
) -> Result<(AnyStage, Staged), SceneStageError> {
    let member = def.cast.get(id).ok_or(SceneStageError::NoCast)?;
    let key = BodyKey::of(doc, &member.character, member.variant.as_ref());
    let body = bodies
        .load(doc, &member.character, member.variant.as_ref())
        .map_err(|source| SceneStageError::Member { member: id.clone(), source })?;
    let landmarks = body.landmarks()?;
    let base = body.base_pose();
    let own = match target {
        Some(_) => None,
        None => Some(
            renderer
                .create_target(&TargetDesc { size: UVec2::splat(MEMBER_TARGET), alpha: AlphaEncoding::Straight })
                .map_err(|source| StageError::Render { what: "a cast member's target".into(), source })?,
        ),
    };
    let Some(drawn) = target.or(own) else { return Err(SceneStageError::NoCast) };
    let staged = body
        .stage(renderer, options.upload, Scene::new(Camera::default()), drawn, options.quality, options.clock)
        .and_then(|mut stage| match (&mut stage, options.face) {
            (AnyStage::Kit(kit), Some(rig)) if kit.built().face.is_some() => match kit.set_face(renderer, rig()) {
                Ok(()) => Ok(stage),
                Err(error) => {
                    if let Err(cleanup) = stage.release(renderer) {
                        tracing::warn!(%cleanup, "releasing a member whose face failed failed too");
                    }
                    Err(error)
                }
            },
            _ => Ok(stage),
        });
    match staged {
        Ok(stage) => Ok((stage, Staged { key, landmarks, base, target: own })),
        Err(error) => {
            if let Some(own) = own
                && let Err(cleanup) = release_target(renderer, own)
            {
                tracing::warn!(%cleanup, "releasing a member's target failed");
            }
            Err(error.into())
        }
    }
}

fn release_target(renderer: &mut dyn Renderer, target: TargetId) -> Result<(), StageError> {
    renderer.release(target.into()).map_err(|source| StageError::Render { what: "a cast member's target".into(), source })
}

impl FrameLoop for SceneStage {
    /// Runs one frame of the whole cast: `input` drives the subject, every other member runs with its last input (its
    /// base pose until something drives it).
    fn tick(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        self.cast.tick(renderer, input)
    }
    fn redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        self.cast.redraw(renderer, input)
    }
    fn service(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<(), StageError> {
        self.cast.service(renderer, input)
    }
    fn set_quality(&mut self, quality: Quality) {
        self.options.quality = quality;
        self.cast.set_quality(quality);
    }
    fn quality(&self) -> &Quality {
        self.cast.quality()
    }
    fn set_scene(&mut self, resolved: &ResolvedScene, subject_root: Mat4) -> Owed {
        self.cast.set_scene(resolved, subject_root)
    }
    fn set_subject_root(&mut self, subject_root: Mat4) {
        self.cast.set_subject_root(subject_root);
    }
    fn owed(&self) -> Owed {
        self.cast.owed()
    }
    fn commit_scene(&mut self, owed: &Owed) {
        self.cast.commit_scene(owed);
    }
    fn pay_scene(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        self.cast.pay_scene(renderer)
    }
    fn scene(&self) -> &Scene {
        self.cast.scene()
    }
    fn scene_mut(&mut self) -> &mut Scene {
        self.cast.scene_mut()
    }
    fn target(&self) -> TargetId {
        self.cast.target()
    }
    fn revision(&self) -> u64 {
        self.cast.revision()
    }
    fn trigger_expressions(&self) -> ExpressionSet {
        self.cast.trigger_expressions()
    }
    fn set_member_input(&mut self, member: &MemberId, input: &FrameInput) -> bool {
        self.cast.set_member_input(member, input)
    }
    fn trigger_expressions_of(&self, member: &MemberId) -> Option<ExpressionSet> {
        self.cast.trigger_expressions_of(member)
    }
    /// Gives back every body and the targets the members were made with; the subject's target stays the caller's.
    fn release(self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        self.release_all(renderer)
    }
}
