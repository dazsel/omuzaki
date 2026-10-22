//! The Live workspace's stage view (#305, #312): the character the streamer performs, drawn in the middle of the
//! Perform workspace and in the output window from one render.
//!
//! [`LiveViewport`] is the render side of the live slice. Each UI frame it takes the newest perform frame, runs the
//! stage on the real renderer ([`LiveScene`]), shows the finished picture to egui through a [`Presenter`] and publishes
//! it ([`publish`]) as the [`Program`] that the stage panel ([`StagePanel`]) and the output window both draw. Nothing
//! renders twice. It also records the latency the health strip shows: the tracker's median plus the age of the frame's
//! data when its picture was drawn ([`LatencyMeter`]).
//!
//! The character is the open project's ([`project_stage`]), built from its definition as the document holds it, with the
//! starter expressions on its face until it brings its own; [`kit_stage`] stages the kit default for tests and tools
//! with no document. Either is framed as a bust.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use eframe::egui_wgpu;
use egui::{Context, Id, Sense, Ui};
use omuzaki_char_api::{
    BodyTemplateId, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef,
    PartDef, PartId, PartKindId, RegistryBuilder,
};
use omuzaki_core::glam::UVec2;
use omuzaki_face_api::ExpressionSet;
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, Background, Image, Quality, RenderError, Renderer, Scene, TargetDesc, TargetId,
};
use omuzaki_stage::frame::face::FaceRig;
use omuzaki_stage::frame::{Stage, StageClock};
use omuzaki_stage::recipe::{LightRig, RecipeError};
use omuzaki_stage::scene::frame::frame as frame_camera;
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage::{StageError, UploadOptions};
use omuzaki_stage_api::{CameraDef, Framing};
use omuzaki_track_api::tracker::{LatencyStats, Tracker};

use super::latency::LatencyMeter;
use super::text::{Word, text};
use super::wiring::clock::ClockObserver;
use super::wiring::{FrameReader, LiveScene};
use crate::aux_window::PhysicalSize;
use crate::output_window::Program;

/// The light rig the stage is lit with: the viewer's studio light (`omuzaki_stage::recipe`).
const LIGHT_RIG: &str = "viewer-studio";

/// Why the stage view could not start or draw.
#[derive(Debug)]
#[non_exhaustive]
pub enum ViewportError {
    /// The kit character did not build.
    Character(BuildError),
    /// One of the kit's own ids is not a valid id: a bug here, not in the caller.
    KitId(String),
    /// The light rig did not load.
    Light(RecipeError),
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
            Self::Light(error) => write!(f, "the stage light did not load: {error}"),
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
            Self::Light(error) => Some(error),
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

/// The kit default character: the soft-sd body, head and hair, with the starter expressions on its face.
///
/// # Errors
/// [`ViewportError::Character`] if the kit's own parts don't build: a bug in the character crate, not in the caller.
pub fn kit_character() -> Result<BuiltCharacter, ViewportError> {
    omuzaki_char::link();
    let bad = |error: &dyn std::fmt::Display| ViewportError::KitId(error.to_string());
    let template = BodyTemplateId::new("soft-sd").map_err(|e| bad(&e))?;
    let mut def = CharacterDef::new("Soft SD", template);
    for kind in ["body", "head", "hair"] {
        let part = PartDef {
            kind: PartKindId::new(kind).map_err(|e| bad(&e))?,
            parent: None,
            enabled: true,
            layer: 0,
            attach: None,
            params: serde_json::json!({}),
        };
        def.parts.insert(PartId::new(kind).map_err(|e| bad(&e))?, part);
    }
    // The colours the kit's body, head and hair ask for: skin with its shade and line, and an outfit colour.
    def.palette = serde_json::from_value(serde_json::json!({
        "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80", "outfit": "#24392f",
    }))
    .map_err(|e| bad(&e))?;
    def.face.expressions = ExpressionSet::starter();
    let id = CharId::new("kit").map_err(|e| bad(&e))?;
    RegistryBuilder
        .build(&id, &def, &BuildOptions::default(), &BuildServices::default())
        .map_err(ViewportError::Character)
}

/// Character `id` as `def` describes it, built for the stage: a character with no expressions of its own performs with
/// the starter set, as the performer's triggers do ([`LiveApp`](super::wiring::LiveApp)). The document is not changed.
///
/// # Errors
/// [`ViewportError::Character`] when the definition doesn't build.
pub fn stage_character(id: &CharId, def: &CharacterDef) -> Result<BuiltCharacter, ViewportError> {
    let mut def = def.clone();
    if def.face.expressions.0.is_empty() {
        def.face.expressions = ExpressionSet::starter();
    }
    RegistryBuilder
        .build(id, &def, &BuildOptions::default(), &BuildServices::default())
        .map_err(ViewportError::Character)
}

/// The kit character on `renderer`: [`character_stage`] with [`kit_character`].
///
/// # Errors
/// [`ViewportError`]; nothing is left on the renderer then.
pub fn kit_stage(renderer: &mut dyn Renderer, size: UVec2) -> Result<Stage, ViewportError> {
    character_stage(renderer, kit_character()?, size)
}

/// Character `id` of the open project, as `def`, on `renderer`: [`character_stage`] with [`stage_character`].
///
/// # Errors
/// [`ViewportError`]; nothing is left on the renderer then.
pub fn project_stage(
    renderer: &mut dyn Renderer,
    id: &CharId,
    def: &CharacterDef,
    size: UVec2,
) -> Result<Stage, ViewportError> {
    character_stage(renderer, stage_character(id, def)?, size)
}

/// `built` on `renderer`, rendering into a target of `size`, lit and framed for the stage view, with its face attached
/// and the stage clock live.
///
/// # Errors
/// [`ViewportError`]; nothing is left on the renderer then.
pub fn character_stage(
    renderer: &mut dyn Renderer,
    built: BuiltCharacter,
    size: UVec2,
) -> Result<Stage, ViewportError> {
    let rig = LightRig::load(LIGHT_RIG).map_err(ViewportError::Light)?;
    let mut landmarks = Landmarks::measure(&built)?;
    // The stage's crown is the top of the head mesh, which leaves the hair out; the kit's hair stands above it, so the
    // bust shot would crop it. The head, for framing, reaches the highest point of the character.
    if let Some(head) = landmarks.head.as_mut() {
        head.max.y = head.max.y.max(landmarks.body.max.y);
    }
    #[allow(clippy::cast_precision_loss)] // a picture's sides are far below 2^24
    let aspect = size.x as f32 / size.y.max(1) as f32;
    let camera = frame_camera(Framing::Bust, &landmarks, &CameraDef::preset(Framing::Bust), aspect)?;
    let mut scene = Scene::new(camera);
    scene.lights = rig.to_lights().map_err(ViewportError::Light)?;
    scene.background = rig.background();
    scene.post = rig.post(false);
    let has_face = built.face.is_some();
    // Premultiplied, because egui blends its textures that way: a straight-alpha picture would fringe at every soft edge
    // once the backdrop is clear ([`LiveViewport::set_clean`]).
    let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Premultiplied })?;
    let stage =
        Stage::new(renderer, built, UploadOptions::default(), scene, target, Quality::live(), StageClock::live());
    let mut stage = match stage {
        Ok(stage) => stage,
        Err(error) => {
            release_target(renderer, target);
            return Err(error.into());
        }
    };
    if has_face && let Err(error) = stage.set_face(renderer, FaceRig::standard()) {
        if let Err(cleanup) = stage.release(renderer) {
            tracing::warn!(%cleanup, "releasing a stage that could not start failed too");
        }
        release_target(renderer, target);
        return Err(error.into());
    }
    Ok(stage)
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
}

/// The render side of the live slice.
pub struct LiveViewport {
    renderer: WgpuRenderer,
    scene: LiveScene,
    presenter: Box<dyn Presenter>,
    size: PhysicalSize,
    program: Option<Program>,
    /// The backdrop the stage was made with, drawn while the output window is closed.
    studio: Background,
    clock: Option<ClockObserver>,
    tracker: Option<SharedTracker>,
    meter: Arc<LatencyMeter>,
    /// The tracker's frames seen at the last sample: a camera that restarted starts its numbers over.
    last_frames: u64,
}

impl LiveViewport {
    /// The view of `stage` (made by [`kit_stage`] on `renderer` at `size`), fed from `reader` and shown through `presenter`.
    /// `clock` is the perform thread's clock, read to date a frame's arrival; `tracker` supplies the first half of the
    /// latency. Without either the latency stays unknown.
    pub fn new(
        renderer: WgpuRenderer,
        stage: Stage,
        size: UVec2,
        reader: FrameReader,
        presenter: Box<dyn Presenter>,
        clock: Option<ClockObserver>,
        tracker: Option<SharedTracker>,
    ) -> Self {
        let size = PhysicalSize::new(size.x, size.y);
        let studio = stage.scene().background;
        Self {
            renderer,
            scene: LiveScene::new(stage, reader),
            presenter,
            size,
            program: None,
            studio,
            clock,
            tracker,
            meter: LatencyMeter::shared(),
            last_frames: 0,
        }
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
        self.scene.stage_mut().scene_mut().background = if clean { Background::Transparent } else { self.studio };
    }

    /// Draws the newest perform frame, if one arrived, and returns the program frame: the newest picture, or `None`
    /// before the first. `dt` is the measured time since the last call. Publishes the program for the panels.
    ///
    /// # Errors
    /// [`ViewportError`]; the previous picture stays on screen and the next call tries again.
    pub fn frame(&mut self, ctx: &Context, dt: f32) -> Result<Option<Program>, ViewportError> {
        if self.scene.frame(&mut self.renderer, dt)?.is_some() {
            let texture = self.presenter.present(ctx, &mut self.renderer, self.scene.stage().target())?;
            self.program = Some(Program { texture, size: self.size });
            self.measure();
        }
        publish(ctx, self.program);
        Ok(self.program)
    }

    /// One latency sample for the frame just drawn, when its data came from a tracked face.
    fn measure(&mut self) {
        let Some(frame) = self.scene.last_frame() else { return };
        if frame.tracking != omuzaki_stage::perform::Tracking::Tracking {
            return;
        }
        let (Some(arrived), Some(clock)) = (frame.arrived, self.clock.as_ref()) else { return };
        let age = (clock().0 - arrived.0).max(0.0);
        let stats = self.tracker.as_ref().map(Tracker::latency).unwrap_or_default();
        if stats.frames < self.last_frames {
            self.meter.reset();
        }
        self.last_frames = stats.frames;
        self.meter.record(stats.p50 + Duration::from_secs_f64(age));
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
        crate::output_window::paint_output(
            &ui.painter().with_clip_rect(rect),
            rect,
            egui::Color32::TRANSPARENT,
            Some(program),
        );
    }
}
