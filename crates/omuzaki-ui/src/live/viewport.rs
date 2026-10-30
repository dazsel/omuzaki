//! The Live workspace's stage view (#305, #312): the character the streamer performs, drawn in the middle of the
//! Perform workspace and in the output window from one render.
//!
//! [`LiveViewport`] is the render side of the live slice. Each UI frame it takes the newest perform frame, runs the
//! stage on the real renderer ([`LiveScene`]), shows the finished picture to egui through a [`Presenter`] and publishes
//! it ([`publish`]) as the [`Program`] that the stage panel ([`StagePanel`]) and the output window both draw. Nothing
//! renders twice. It also records the latency the health strip shows: the tracker's median plus the age of the frame's
//! data when its picture was drawn ([`LatencyMeter`]).
//!
//! What it draws is the scene the project opens in, whole (#2401): its backdrop, ground, lights, post, props and active
//! camera, and every visible cast member with the touches between them, the subject driven by the performer
//! ([`scene_stage`], `omuzaki_stage::scene::stage`). It follows the document ([`LiveViewport::follow`]), so an edit by a
//! person or an agent shows in the live view and every output. Each character is built from its definition as the
//! document holds it, with the starter expressions on its face until it brings its own ([`StudioBodies`]). A project
//! with no scene of its own stands in the built-in default, framed as a bust; [`kit_stage`] stages the kit default
//! that way for tests and tools with no document.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use eframe::egui_wgpu;
use egui::{Context, Id, Sense, Ui};
use omuzaki_char_api::{
    BodyTemplateId, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef,
    RegistryBuilder, VariantId,
};
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::{DocPath, DocRead, DocVersion};
use omuzaki_face_api::ExpressionSet;
use omuzaki_ops_api::OpError;
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, Background, Image, MtoonSupport, Quality, RenderError, Renderer, TargetDesc, TargetId,
};
use omuzaki_stage::StageError;
use omuzaki_stage::body::{Body, BodyServices, draw_in_look, load_body};
use omuzaki_stage::frame::StageClock;
use omuzaki_stage::ops::render::job::read_definition;
use omuzaki_stage::scene::stage::{BodySource, SceneStage, SceneStageError, SceneStageOptions, Synced};
use omuzaki_stage::stages::FrameLoop;
use omuzaki_stage_api::Framing;
use omuzaki_store_api::Store;
use omuzaki_track_api::tracker::{LatencyStats, Tracker};
use serde_json::Value;

use super::health::Rendered;
use super::latency::LatencyMeter;
use super::text::{Word, text};
use super::wiring::clock::ClockObserver;
use super::wiring::{FrameReader, LiveScene};
use crate::aux_window::PhysicalSize;
use crate::output_window::Program;

/// Why the stage view could not start or draw.
#[derive(Debug)]
#[non_exhaustive]
pub enum ViewportError {
    /// The kit character did not build.
    Character(BuildError),
    /// One of the kit's own ids is not a valid id: a bug here, not in the caller.
    KitId(String),
    /// The scene could not be staged or followed.
    Scene(SceneStageError),
    /// The stage refused: framing, upload, or a frame.
    Stage(StageError),
    /// The renderer refused a target or a readback.
    Render(RenderError),
    /// The live slice has stopped: there is no perform thread to draw frames from.
    Stopped,
}

impl std::fmt::Display for ViewportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Character(error) => write!(f, "the kit character did not build: {error}"),
            Self::KitId(error) => write!(f, "the kit character has a bad id: {error}"),
            Self::Scene(error) => write!(f, "the scene could not be staged: {error}"),
            Self::Stage(error) => write!(f, "the stage failed: {error}"),
            Self::Render(error) => write!(f, "the renderer failed: {error}"),
            Self::Stopped => f.write_str("the live slice has stopped"),
        }
    }
}

impl std::error::Error for ViewportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Character(error) => Some(error),
            Self::KitId(_) | Self::Stopped => None,
            Self::Scene(error) => Some(error),
            Self::Stage(error) => Some(error),
            Self::Render(error) => Some(error),
        }
    }
}

impl From<StageError> for ViewportError {
    fn from(error: StageError) -> Self {
        Self::Stage(error)
    }
}

impl From<RenderError> for ViewportError {
    fn from(error: RenderError) -> Self {
        Self::Render(error)
    }
}

/// The kit default character's definition: the same dressed character `character.create` gives a new project on the
/// soft-sd template (body, hands, head, hair and an outfit). One source with the Build workspace's sample, so the stage
/// never shows a bare stand-in.
///
/// # Errors
/// [`ViewportError::KitId`] if the kit's own ids don't read: a bug in the character crate, not in the caller.
pub fn kit_definition() -> Result<(CharId, CharacterDef), ViewportError> {
    omuzaki_char::link();
    let bad = |error: &dyn std::fmt::Display| ViewportError::KitId(error.to_string());
    let template = BodyTemplateId::new("soft-sd").map_err(|e| bad(&e))?;
    let def = omuzaki_char::ops::character::create::default_character(&template).map_err(|e| bad(&e))?;
    let id = CharId::new("kit").map_err(|e| bad(&e))?;
    Ok((id, def))
}

/// The kit default character ([`kit_definition`]), built for the stage with the starter expressions on its face.
///
/// # Errors
/// [`ViewportError::Character`] if the kit's own parts don't build: a bug in the character crate, not in the caller.
pub fn kit_character() -> Result<BuiltCharacter, ViewportError> {
    let (id, def) = kit_definition()?;
    let mut built = RegistryBuilder
        .build(&id, &def, &BuildOptions::default(), &BuildServices::default())
        .map_err(ViewportError::Character)?;
    with_starter_expressions(&mut built);
    Ok(built)
}

/// A character with no expressions of its own performs with the starter set, as the performer's triggers do
/// ([`LiveApp`](super::wiring::LiveApp)). The face profile is a copy of the definition's, so this is the same as
/// building with the set in the definition; the document is not changed.
fn with_starter_expressions(built: &mut BuiltCharacter) {
    if let Some(face) = built.face.as_mut()
        && face.profile.expressions.0.is_empty()
    {
        face.profile.expressions = ExpressionSet::starter();
    }
}

/// Where the live stage's bodies come from: each cast member's character as the document holds it, in its look, with
/// the starter expressions when it has none. An imported character needs the asset store its original is kept in.
pub struct StudioBodies {
    store: Option<Arc<dyn Store>>,
    mtoon: MtoonSupport,
}

impl StudioBodies {
    /// Bodies for a renderer that draws `mtoon`, imported characters read from `store`.
    pub fn new(store: Option<Arc<dyn Store>>, mtoon: MtoonSupport) -> Self {
        Self { store, mtoon }
    }
}

impl std::fmt::Debug for StudioBodies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StudioBodies").field("store", &self.store.is_some()).finish_non_exhaustive()
    }
}

impl BodySource for StudioBodies {
    fn load(&mut self, doc: &dyn DocRead, id: &CharId, variant: Option<&VariantId>) -> Result<Body, OpError> {
        let quality = BuildOptions::default().quality;
        let mut body = if let Some(store) = &self.store {
            let (clock, build) = (SystemClock::default(), BuildServices::default());
            let services = BodyServices::new(&RegistryBuilder, &build, store.as_ref(), self.mtoon, &clock);
            load_body(doc, &services, id, variant, quality)?
        } else {
            // No asset store: a kit character only, as `load_body` builds one.
            let def = read_definition(doc, id, variant)?;
            let options = BuildOptions { variant: variant.cloned(), quality, rig_export: false };
            let mut built = RegistryBuilder
                .build(id, &def, &options, &BuildServices::default())
                .map_err(|e| OpError::invalid("character", format!("character '{id}' doesn't build: {e}")))?;
            draw_in_look(doc, id, &mut built)?;
            Body::Kit(Box::new(built))
        };
        if let Body::Kit(built) = &mut body {
            with_starter_expressions(built);
        }
        Ok(body)
    }

    fn store(&self) -> Option<&dyn Store> {
        self.store.as_deref()
    }
}

/// A document of one tree: a copy of the app's document as the stage reads it, or a stage with no project.
#[derive(Debug)]
pub struct Tree(Value);

impl Tree {
    /// The document whose root is `root`.
    pub fn new(root: Value) -> Self {
        Self(root)
    }
}

impl DocRead for Tree {
    fn get(&self, path: &DocPath) -> Option<&Value> {
        omuzaki_doc_api::lookup(&self.0, path)
    }
    fn version(&self) -> DocVersion {
        DocVersion(0)
    }
}

/// A document read before the app core takes it: the project as its files hold it.
struct Reading<'a>(&'a crate::wiring::Document);

impl DocRead for Reading<'_> {
    fn get(&self, path: &DocPath) -> Option<&Value> {
        self.0.get(path)
    }
    fn version(&self) -> DocVersion {
        DocVersion(0)
    }
}

/// The open project's scene on `renderer` ([`scene_stage`]), read from `document` before the app core takes it.
///
/// # Errors
/// [`ViewportError`]; nothing is left on the renderer then.
pub fn document_stage(
    renderer: &mut dyn Renderer,
    document: &crate::wiring::Document,
    bodies: &mut dyn BodySource,
    size: UVec2,
) -> Result<SceneStage, ViewportError> {
    scene_stage(renderer, &Reading(document), bodies, size)
}

/// The kit character alone in the default scene, on `renderer`: [`project_stage`] with [`kit_definition`].
///
/// # Errors
/// [`ViewportError`]; nothing is left on the renderer then.
pub fn kit_stage(renderer: &mut dyn Renderer, size: UVec2) -> Result<SceneStage, ViewportError> {
    let (id, def) = kit_definition()?;
    project_stage(renderer, &id, &def, size)
}

/// Character `id`, as `def`, alone in the default scene on `renderer`: a project of that one character with no scene of
/// its own. The document is not changed.
///
/// # Errors
/// [`ViewportError`]; nothing is left on the renderer then.
pub fn project_stage(
    renderer: &mut dyn Renderer,
    id: &CharId,
    def: &CharacterDef,
    size: UVec2,
) -> Result<SceneStage, ViewportError> {
    let mut node = serde_json::json!({});
    def.to_node(&mut node).map_err(|e| ViewportError::KitId(e.to_string()))?;
    let doc = Tree(serde_json::json!({ "characters": { id.as_str(): node } }));
    scene_stage(renderer, &doc, &mut StudioBodies::new(None, MtoonSupport::NONE), size)
}

/// The scene `doc` opens in, with its whole cast, on `renderer`, rendering into a target of `size` at the live quality
/// on the live clock, every kit face on its rig. A project with no scene of its own stands in the built-in default,
/// framed as a bust.
///
/// # Errors
/// [`ViewportError`]; nothing is left on the renderer then.
pub fn scene_stage(
    renderer: &mut dyn Renderer,
    doc: &dyn DocRead,
    bodies: &mut dyn BodySource,
    size: UVec2,
) -> Result<SceneStage, ViewportError> {
    // Premultiplied, because egui blends its textures that way: a straight-alpha picture would fringe at every soft edge
    // once the backdrop is clear ([`LiveViewport::set_clean`]).
    let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Premultiplied })?;
    let options = SceneStageOptions::new(Quality::live(), StageClock::live()).with_unsaved_framing(Framing::Bust);
    match SceneStage::open(doc, None, bodies, renderer, target, aspect(size), options) {
        Ok(stage) => Ok(stage),
        Err(error) => {
            release_target(renderer, target);
            Err(ViewportError::Scene(error))
        }
    }
}

/// A picture's sides as the width over the height.
fn aspect(size: UVec2) -> f32 {
    #[allow(clippy::cast_precision_loss)] // a picture's sides are far below 2^24
    let aspect = size.x as f32 / size.y.max(1) as f32;
    aspect
}

fn release_target(renderer: &mut dyn Renderer, target: TargetId) {
    if let Err(error) = renderer.release(target.into()) {
        tracing::warn!(%error, "releasing the stage's target failed");
    }
}

/// How the finished picture reaches egui as a texture.
pub trait Presenter: Send {
    /// Makes `target`'s picture, as `renderer` just drew it, the texture egui paints, and returns its id.
    ///
    /// # Errors
    /// [`ViewportError::Render`] if the picture can't be fetched.
    fn present(
        &mut self,
        ctx: &Context,
        renderer: &mut WgpuRenderer,
        target: TargetId,
    ) -> Result<egui::TextureId, ViewportError>;
}

/// Shares the renderer's own texture with egui when both run on one device (the app): nothing is copied.
///
/// The registration with egui (a texture view, a sampler and a bind group, under the painter's write lock) is made once
/// and kept: the view points into the target's texture, so it shows each new picture by itself. It is made again only
/// when the target's texture is replaced (a resized target), so a frame that changes nothing about the target takes no
/// lock and allocates nothing here.
pub struct NativePresenter {
    state: egui_wgpu::RenderState,
    registered: Option<Registered>,
    registrations: u64,
}

/// A texture egui holds a view of, and the id it paints it under.
struct Registered {
    id: egui::TextureId,
    texture: egui_wgpu::wgpu::Texture,
}

impl NativePresenter {
    /// A presenter on the app's wgpu state.
    pub fn new(state: egui_wgpu::RenderState) -> Self {
        Self { state, registered: None, registrations: 0 }
    }

    /// How many times the texture was registered or re-pointed at egui: once for a stage whose target never changes.
    pub fn registrations(&self) -> u64 {
        self.registrations
    }
}

impl Presenter for NativePresenter {
    fn present(
        &mut self,
        _ctx: &Context,
        renderer: &mut WgpuRenderer,
        target: TargetId,
    ) -> Result<egui::TextureId, ViewportError> {
        let texture = renderer.target_texture(target)?;
        if let Some(registered) = &self.registered
            && registered.texture == texture
        {
            return Ok(registered.id);
        }
        let view = texture.create_view(&egui_wgpu::wgpu::TextureViewDescriptor::default());
        let filter = egui_wgpu::wgpu::FilterMode::Linear;
        let mut painter = self.state.renderer.write();
        let id = match &self.registered {
            Some(registered) => {
                painter.update_egui_texture_from_wgpu_texture(&self.state.device, &view, filter, registered.id);
                registered.id
            }
            None => painter.register_native_texture(&self.state.device, &view, filter),
        };
        drop(painter);
        self.registered = Some(Registered { id, texture });
        self.registrations += 1;
        Ok(id)
    }
}

impl Drop for NativePresenter {
    fn drop(&mut self) {
        if let Some(registered) = self.registered.take() {
            self.state.renderer.write().free_texture(&registered.id);
        }
    }
}

/// Reads the picture back and uploads it as an ordinary egui texture: for a renderer on its own device (tests and the
/// evidence pictures, where egui's device isn't the renderer's).
#[derive(Default)]
pub struct ReadbackPresenter {
    texture: Option<egui::TextureHandle>,
}

impl Presenter for ReadbackPresenter {
    fn present(
        &mut self,
        ctx: &Context,
        renderer: &mut WgpuRenderer,
        target: TargetId,
    ) -> Result<egui::TextureId, ViewportError> {
        let Image { size, rgba8, alpha } = renderer.read_target(target)?;
        let sides = [size.x as usize, size.y as usize];
        // egui blends premultiplied pixels; a straight-alpha target (another stage's) is converted on the way in.
        let image = match alpha {
            AlphaEncoding::Premultiplied => egui::ColorImage::from_rgba_premultiplied(sides, &rgba8),
            AlphaEncoding::Straight => egui::ColorImage::from_rgba_unmultiplied(sides, &rgba8),
        };
        let handle = match self.texture.take() {
            Some(mut handle) => {
                handle.set(image, egui::TextureOptions::LINEAR);
                handle
            }
            None => ctx.load_texture("live-stage", image, egui::TextureOptions::LINEAR),
        };
        let id = handle.id();
        self.texture = Some(handle);
        Ok(id)
    }
}

/// The tracker as the viewport reads its latency while the input owns it: one handle for the input, one for the
/// viewport.
#[derive(Clone)]
pub struct SharedTracker(Arc<Mutex<Box<dyn Tracker>>>);

impl SharedTracker {
    /// Shares `tracker`.
    pub fn new(tracker: Box<dyn Tracker>) -> Self {
        Self(Arc::new(Mutex::new(tracker)))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Box<dyn Tracker>> {
        // A tracker that panicked mid-call left nothing the readers depend on: take the guard anyway.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Tracker for SharedTracker {
    fn devices(&self) -> Vec<omuzaki_track_api::camera::CameraInfo> {
        self.lock().devices()
    }

    fn start(
        &mut self,
        device: &omuzaki_track_api::camera::CameraId,
        config: &omuzaki_track_api::tracker::TrackerConfig,
    ) -> Result<Box<dyn omuzaki_anim_api::FrameSource>, omuzaki_track_api::error::TrackError> {
        self.lock().start(device, config)
    }

    fn stop(&mut self) {
        self.lock().stop();
    }

    fn latency(&self) -> LatencyStats {
        self.lock().latency()
    }

    fn diagnoses(&self) -> Vec<omuzaki_track_api::doctor::Diagnosis> {
        self.lock().diagnoses()
    }

    fn recheck(&self) {
        self.lock().recheck();
    }

    fn track_body(
        &mut self,
        config: omuzaki_track_api::body::BodyConfig,
        board: &Arc<omuzaki_track_api::body::BodyBoard>,
    ) -> Result<omuzaki_track_api::body::BodyConfig, omuzaki_track_api::error::TrackError> {
        self.lock().track_body(config, board)
    }

    fn body_status(&self) -> omuzaki_track_api::body::BodyStatus {
        self.lock().body_status()
    }
}

/// The render side of the live slice.
pub struct LiveViewport {
    renderer: WgpuRenderer,
    scene: LiveScene,
    presenter: Box<dyn Presenter>,
    size: PhysicalSize,
    program: Option<Program>,
    /// The scene's backdrop, drawn while the output window is closed.
    studio: Background,
    /// The program carries the character alone ([`set_clean`](Self::set_clean)).
    clean: bool,
    clock: Option<ClockObserver>,
    tracker: Option<SharedTracker>,
    meter: Arc<LatencyMeter>,
    /// The tracker's frames seen at the last sample: a camera that restarted starts its numbers over.
    last_frames: u64,
    /// The frame drawn since the publisher last took one.
    rendered: Option<Rendered>,
}

impl LiveViewport {
    /// The view of `stage` (made by [`scene_stage`] on `renderer` at `size`), fed from `reader` and shown through
    /// `presenter`.
    /// `clock` is the perform thread's clock, read to date a frame's arrival; `tracker` supplies the first half of the
    /// latency. Without either the latency stays unknown.
    pub fn new(
        renderer: WgpuRenderer,
        stage: SceneStage,
        size: UVec2,
        reader: FrameReader,
        presenter: Box<dyn Presenter>,
        clock: Option<ClockObserver>,
        tracker: Option<SharedTracker>,
    ) -> Self {
        let size = PhysicalSize::new(size.x, size.y);
        let studio = stage.resolved().background;
        Self {
            renderer,
            scene: LiveScene::new(stage, reader),
            presenter,
            size,
            program: None,
            studio,
            clean: false,
            clock,
            tracker,
            meter: LatencyMeter::shared(),
            last_frames: 0,
            rendered: None,
        }
    }

    /// The frame drawn since the last call, for the health publisher; `None` when none was.
    pub fn take_rendered(&mut self) -> Option<Rendered> {
        self.rendered.take()
    }

    /// Records its latency in `meter` instead of the shared one (tests).
    pub fn set_meter(&mut self, meter: Arc<LatencyMeter>) {
        self.meter = meter;
    }

    /// Draws the program frame with nothing behind the character (`true`) or over the studio backdrop the stage was made
    /// with (`false`), from the next frame on.
    ///
    /// The output window shows the program over its own backdrop, a key colour or the desktop, so while it is open the
    /// studio backdrop would cover exactly what the capture app keys out: the frame must carry the character alone,
    /// with alpha.
    pub fn set_clean(&mut self, clean: bool) {
        self.clean = clean;
        self.scene.stage_mut().scene_mut().background = if clean { Background::Transparent } else { self.studio };
    }

    /// Follows `doc`: what changed in the scene or its cast since the last call is staged again
    /// ([`SceneStage::sync`]), and the next frame draws it. Cold: a changed character is built again here.
    ///
    /// # Errors
    /// [`ViewportError::Scene`]; the stage keeps drawing what it had, and the next call tries again.
    pub fn follow(&mut self, doc: &dyn DocRead, bodies: &mut dyn BodySource) -> Result<Synced, ViewportError> {
        let synced = self.scene.stage_mut().sync(doc, bodies, &mut self.renderer).map_err(ViewportError::Scene)?;
        if synced.changed() {
            // The scene's own backdrop, from now on, under the program's clean rule.
            self.studio = self.scene.stage().resolved().background;
            self.set_clean(self.clean);
            self.scene.redraw_next();
        }
        Ok(synced)
    }

    /// Puts `scene` on from the next [`follow`](Self::follow) on (`None`: the one the project opens in): the program's
    /// scene switch.
    pub fn ask_scene(&mut self, scene: Option<omuzaki_stage_api::SceneId>) {
        self.scene.stage_mut().ask(scene);
    }

    /// Adaptive quality on or off, held still while on ([`LiveScene::set_adaptive`]).
    pub fn set_adaptive(&mut self, on: bool, held: bool) {
        self.scene.set_adaptive(on, held);
    }

    /// Draws the newest perform frame, if one arrived, and returns the program frame: the newest picture, or `None`
    /// before the first. `dt` is the measured time since the last call. Publishes the program for the panels.
    ///
    /// # Errors
    /// [`ViewportError`]; the previous picture stays on screen and the next call tries again.
    pub fn frame(&mut self, ctx: &Context, dt: f32) -> Result<Option<Program>, ViewportError> {
        if let Some(out) = self.scene.frame(&mut self.renderer, dt)? {
            let texture = self.presenter.present(ctx, &mut self.renderer, self.scene.stage().target())?;
            self.program = Some(Program { texture, size: self.size });
            let track_to_submit = self.measure();
            self.rendered = Some(Rendered { report: out.report, track_to_submit });
        }
        publish(ctx, self.program);
        Ok(self.program)
    }

    /// One latency sample for the frame just drawn, when its data came from a tracked face; returns it.
    fn measure(&mut self) -> Option<Duration> {
        let frame = self.scene.last_frame()?;
        if frame.tracking != omuzaki_stage::perform::Tracking::Tracking {
            return None;
        }
        let (Some(arrived), Some(clock)) = (frame.arrived, self.clock.as_ref()) else { return None };
        let age = (clock().0 - arrived.0).max(0.0);
        let stats = self.tracker.as_ref().map(Tracker::latency).unwrap_or_default();
        if stats.frames < self.last_frames {
            self.meter.reset();
        }
        self.last_frames = stats.frames;
        let latency = stats.p50 + Duration::from_secs_f64(age);
        self.meter.record(latency);
        Some(latency)
    }

    /// The program frame's pixels, as last drawn: a thumbnail, or a test looking at what the streamer sees.
    ///
    /// # Errors
    /// [`ViewportError::Render`] if the renderer can't read the target back.
    pub fn snapshot(&mut self) -> Result<Image, ViewportError> {
        Ok(self.renderer.read_target(self.scene.stage().target())?)
    }

    /// The scene as last drawn.
    pub fn scene(&self) -> &LiveScene {
        &self.scene
    }

    /// The scene, to reach its cast between frames.
    pub fn scene_mut(&mut self) -> &mut LiveScene {
        &mut self.scene
    }

    /// Gives the stage's resources back to the renderer (the first step of the quit order, `live::wiring`).
    ///
    /// # Errors
    /// The first release the renderer refused.
    pub fn release(self) -> Result<(), StageError> {
        let Self { mut renderer, scene, presenter, .. } = self;
        let released = scene.release(&mut renderer);
        // The texture the presenter registered points into the renderer's target: drop it first.
        drop(presenter);
        released
    }
}

impl std::fmt::Debug for LiveViewport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveViewport").field("size", &self.size).field("program", &self.program).finish_non_exhaustive()
    }
}

/// The key the program frame is kept under in egui's memory.
fn program_id() -> Id {
    Id::new("omuzaki-live-program")
}

/// Shows `program` to every panel on this context: the stage panel reads it, the output window is handed it.
pub fn publish(ctx: &Context, program: Option<Program>) {
    ctx.data_mut(|data| {
        if let Some(program) = program {
            data.insert_temp(program_id(), program);
        } else {
            data.remove::<Program>(program_id());
        }
    });
}

/// The program frame last published on `ctx`.
pub fn published(ctx: &Context) -> Option<Program> {
    ctx.data(|data| data.get_temp(program_id()))
}

/// The Perform workspace's centre: the program frame, fitted and centred, with its place held while the stage starts.
#[derive(Debug, Default)]
pub struct StagePanel;

impl StagePanel {
    /// Draws the program frame in the space the panel has.
    pub fn ui(&mut self, ui: &mut Ui) {
        let label = text(ui.ctx(), Word::StageLabel);
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, &label));
        let Some(program) = published(ui.ctx()) else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text(ui.ctx(), Word::StageStarting),
                egui::FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
            return;
        };
        let _ = crate::output_window::paint_output(
            &ui.painter().with_clip_rect(rect),
            rect,
            egui::Color32::TRANSPARENT,
            Some(program),
            1.0,
        );
    }
}
