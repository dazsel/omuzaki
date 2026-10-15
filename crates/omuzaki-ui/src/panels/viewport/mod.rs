//! The viewport: the stage's render of the open character with an orbit camera (`shell.md` section 5).
//!
//! The character is built from the document when it changes ([`DocWatch`]) by the app's own builder, and staged on the
//! app's wgpu device ([`gpu`]): a render is a still from the viewport's camera, so it matches `render.preview`'s path
//! (`omuzaki_stage::ops::render::still`). It renders only when the camera, the size or the character changed; an idle
//! viewport asks egui for no frame, and a camera drag (whose pointer moves wake egui) renders every frame
//! (`design-system.md` section 9 rule 1).
//!
//! **Camera:** orbit is middle drag or Alt+left drag, pan is Shift+middle drag, zoom is the wheel or a pinch. With the
//! viewport focused, 1 to 4 are Front, ¾, Side and Back, F frames the selection and Home frames the character; the
//! toolbar has a button for each.

pub mod camera;
pub mod gpu;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui_wgpu;
use egui::{Key, Modifiers, PointerButton, Rect, Sense, Ui, WidgetInfo, WidgetType};
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, PartId, Quality};
use omuzaki_core::glam::UVec2;
use omuzaki_stage::ops::render::job::{DEFAULT_LIGHT_RIG, still_light};

use self::camera::{Bounds, OrbitCamera, View};
use self::gpu::{Callback, Gpu, Job, SharedJob};
use super::edit::EditFocus;
use super::io::{DocWatch, Io};
use super::text::{self, say};
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::widgets::{Button, EmptyState};

/// The viewport's id, as the shell registers it.
pub const ID: &str = "viewport";
/// Points of wheel scroll that zoom by a factor of e.
const WHEEL_PER_E: f32 = 500.0;

/// What the app gives the viewport: its wgpu device (egui's) and the character builder with its services.
#[derive(Clone)]
pub struct ViewportServices {
    /// The device egui draws with; `None` when the app runs without wgpu, and the viewport says so.
    pub gpu: Option<egui_wgpu::RenderState>,
    /// Builds characters for the stage.
    pub builder: Arc<dyn CharacterBuilder>,
    /// What builds read (assets, a rasteriser).
    pub build: BuildServices,
}

/// The viewport panel.
pub struct Viewport {
    focus: EditFocus,
    services: ViewportServices,
    watch: DocWatch,
    character: Option<CharId>,
    camera: OrbitCamera,
    /// The camera and size of the last frame that asked for a render.
    drawn: Option<(OrbitCamera, UVec2)>,
    /// The character's box, and each part's, from the last build.
    bounds: Option<Bounds>,
    parts: BTreeMap<PartId, Bounds>,
    job: SharedJob,
    /// Why the GPU side couldn't start, or the character couldn't build.
    problem: Option<String>,
}

impl std::fmt::Debug for Viewport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewport").field("camera", &self.camera).field("problem", &self.problem).finish_non_exhaustive()
    }
}

impl Viewport {
    /// A viewport over `focus`, drawing with `services`.
    pub fn new(focus: EditFocus, services: ViewportServices) -> Self {
        Self {
            focus,
            services,
            watch: DocWatch::default(),
            character: None,
            camera: OrbitCamera::default(),
            drawn: None,
            bounds: None,
            parts: BTreeMap::new(),
            job: Arc::new(Mutex::new(Job::default())),
            problem: None,
        }
    }

    /// Whether the panel can work: there is a GPU and a character is open.
    pub fn available(&self) -> bool {
        self.services.gpu.is_some() && self.focus.character().is_some()
    }

    /// The camera.
    pub fn camera(&self) -> OrbitCamera {
        self.camera
    }

    /// Sets the camera (a test, or a saved view).
    pub fn set_camera(&mut self, camera: OrbitCamera) {
        self.camera = camera;
    }

    /// How many frames rendered the stage (frames that only showed the last picture don't count).
    pub fn renders(&self) -> u64 {
        self.job.lock().unwrap_or_else(PoisonError::into_inner).renders
    }

    /// Why the viewport can't show the character, if it can't.
    pub fn problem(&self) -> Option<String> {
        let job = self.job.lock().unwrap_or_else(PoisonError::into_inner);
        self.problem.clone().or_else(|| job.error.clone())
    }

    /// Draws the viewport for one frame.
    pub fn ui(&mut self, ui: &mut Ui, io: Io<'_>) {
        let ds = ds(ui.ctx());
        let Some(state) = self.services.gpu.clone() else {
            EmptyState::new(icons::CUBE, &say(&ds, text::NO_GPU, &[]), &say(&ds, text::NO_GPU_HOW, &[])).show(ui);
            return;
        };
        let Some(character) = self.focus.character() else {
            EmptyState::new(icons::CUBE, &say(&ds, text::NO_CHARACTER, &[]), &say(&ds, text::OPEN_CHARACTER, &[]))
                .show(ui);
            return;
        };
        let light = || still_light(DEFAULT_LIGHT_RIG);
        if let Err(e) =
            Gpu::install(&state, || light().unwrap_or_else(|_| fallback_light()), omuzaki_render_api::Quality::live())
        {
            self.problem = Some(e);
        }
        self.rebuild(&character, io);
        self.toolbar(ui, &ds);
        // What is left of the panel, and only what shows of it: the render is the size of what is seen.
        let rect = ui.available_rect_before_wrap().intersect(ui.clip_rect());
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let name = say(&ds, text::VIEWPORT, &[]);
        a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Other, true, name.as_ref()), |_| {});
        self.input(ui, &response);
        if self.focus.take_frame_request() {
            self.frame_selection();
        }
        self.submit(ui, rect);
        if let Some(problem) = self.problem() {
            let at = rect.left_bottom() + egui::vec2(8.0, -24.0);
            let text = say(&ds, text::UNBUILT, &[("reason", &problem)]);
            ui.painter().text(at, egui::Align2::LEFT_TOP, text, ds.text.small.clone(), ds.theme.danger);
        }
    }

    /// Builds the character again when the document or the open character changed, and hands it to the GPU side.
    fn rebuild(&mut self, character: &CharId, io: Io<'_>) {
        let other = self.character.as_ref() != Some(character);
        if !self.watch.stale(io.events) && !other {
            return;
        }
        self.watch.built();
        let built = io.character(character).map_err(|e| e.message).and_then(|def| {
            let def = def.ok_or_else(|| format!("there is no character '{character}'"))?;
            let options = BuildOptions { variant: None, quality: Quality::Live, rig_export: false };
            self.services.builder.build(character, &def, &options, &self.services.build).map_err(|e| e.to_string())
        });
        match built {
            Ok(built) => {
                self.measure(&built);
                self.problem = None;
                self.job.lock().unwrap_or_else(PoisonError::into_inner).built = Some(built);
                if other {
                    self.character = Some(character.clone());
                    self.frame_character();
                }
            }
            Err(e) => self.problem = Some(e),
        }
    }

    /// The boxes the framing keys use, from the built meshes (bind pose).
    fn measure(&mut self, built: &BuiltCharacter) {
        self.parts.clear();
        for mesh in &built.meshes {
            if let Some(b) = Bounds::of(mesh.mesh.geometry.positions.iter().copied()) {
                let part = self.parts.entry(mesh.part.clone()).or_insert(b);
                *part = part.union(b);
            }
        }
        self.bounds = self.parts.values().copied().reduce(Bounds::union);
    }

    /// Home: the whole character.
    fn frame_character(&mut self) {
        if let Some(bounds) = self.bounds {
            self.camera.frame(bounds);
        }
    }

    /// F: the selected parts, or the character when none of them has a mesh.
    fn frame_selection(&mut self) {
        let selected = self.focus.selected().iter().filter_map(|p| self.parts.get(p).copied()).reduce(Bounds::union);
        match selected {
            Some(bounds) => self.camera.frame(bounds),
            None => self.frame_character(),
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, ds: &crate::ds::Ds) {
        // Wrapped, so a narrow dock grows the toolbar down rather than the panel sideways.
        ui.horizontal_wrapped(|ui| {
            for (view, label, key) in [
                (View::Front, text::FRONT, Key::Num1),
                (View::ThreeQuarter, text::THREE_QUARTER, Key::Num2),
                (View::Side, text::SIDE, Key::Num3),
                (View::Back, text::BACK, Key::Num4),
            ] {
                if ui.add(Button::new(&say(ds, label, &[])).ghost().shortcut_hint(key)).clicked() {
                    self.camera.look_from(view);
                }
            }
            if ui.add(Button::new(&say(ds, text::FRAME_SELECTION, &[])).ghost().shortcut_hint(Key::F)).clicked() {
                self.frame_selection();
            }
            if ui.add(Button::new(&say(ds, text::FRAME_CHARACTER, &[])).ghost().shortcut_hint(Key::Home)).clicked() {
                self.frame_character();
            }
        });
    }

    /// The camera gestures and keys (`shell.md` section 5).
    fn input(&mut self, ui: &Ui, response: &egui::Response) {
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        let (alt, shift) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift));
        let delta = response.drag_delta();
        if response.dragged_by(PointerButton::Middle) && shift {
            self.camera.pan(delta.x, delta.y, response.rect.height());
        } else if response.dragged_by(PointerButton::Middle) || (alt && response.dragged_by(PointerButton::Primary)) {
            self.camera.orbit(delta.x, delta.y);
        }
        if response.hovered() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            if scroll != 0.0 {
                self.camera.zoom((-scroll / WHEEL_PER_E).exp());
            }
            if (pinch - 1.0).abs() > f32::EPSILON {
                self.camera.zoom(1.0 / pinch);
            }
        }
        if response.has_focus() {
            let pressed = |key| ui.input_mut(|i| i.consume_key(Modifiers::NONE, key));
            for (view, key) in View::ALL.into_iter().zip([Key::Num1, Key::Num2, Key::Num3, Key::Num4]) {
                if pressed(key) {
                    self.camera.look_from(view);
                }
            }
            if pressed(Key::F) {
                self.frame_selection();
            }
            if pressed(Key::Home) {
                self.frame_character();
            }
        }
    }

    /// Hands this frame's job to the callback and adds the callback to the panel.
    fn submit(&mut self, ui: &Ui, rect: Rect) {
        let ppp = ui.ctx().pixels_per_point();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a panel's size in pixels, rounded")]
        let size =
            UVec2::new((rect.width() * ppp).round().max(1.0) as u32, (rect.height() * ppp).round().max(1.0) as u32);
        {
            let mut job = self.job.lock().unwrap_or_else(PoisonError::into_inner);
            if self.drawn != Some((self.camera, size)) {
                job.render = true;
                self.drawn = Some((self.camera, size));
            }
            job.camera = self.camera.camera();
            job.size = size;
        }
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, Callback { job: Arc::clone(&self.job) }));
    }
}

/// The light a still uses when the named rig can't load (it ships with the binary, so this is a broken build): the
/// renderer's default scene light.
fn fallback_light() -> omuzaki_stage::ops::render::still::StillLight {
    let scene = omuzaki_render_api::Scene::new(omuzaki_render_api::Camera::default());
    omuzaki_stage::ops::render::still::StillLight::new(scene.lights, scene.background, scene.post)
}
