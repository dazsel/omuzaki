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
//!
//! **Selecting:** a click picks the part under the pointer ([`pick`]) and selects it, Shift+click adds or removes it,
//! and a click on nothing clears the selection. The selection is drawn as a box round each selected part, as posed.
//!
//! **Placing:** the selected part, when it moves as one piece (an accessory, not hair or a garment), carries move,
//! rotate and scale handles ([`gizmo`]; W, E and R pick which) in its middle. A drag writes its attachment with
//! `part.set_attach`, the op an agent calls, inside one transaction: the whole drag is one undo step, Escape puts it
//! back, and Ctrl snaps. A part placed by its kind alone is pinned first (`part.set_attach` with `pin`), in the same
//! transaction, so it moves from where it is.

pub mod camera;
pub mod gizmo;
pub mod gpu;
pub mod pick;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui_wgpu;
use egui::{Key, Modifiers, PointerButton, Rect, Sense, Ui, WidgetInfo, WidgetType};
use omuzaki_char_api::{
    Attach, AttachOffset, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, PartId,
    Quality,
};
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_ops_api::TxId;
use omuzaki_stage::ops::render::job::{DEFAULT_LIGHT_RIG, still_light};
use serde_json::json;

use self::camera::{Bounds, OrbitCamera, View};
use self::gizmo::{Drag, Handle, Screen, Tool};
use self::gpu::{Callback, Gpu, Job, SharedJob};
use super::edit::EditFocus;
use super::io::{DocWatch, Io};
use super::text::{self, say};
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::widgets::{Button, EmptyState, Segment, Segmented};
use crate::shell::history::{Preview, Viewing};

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
    /// The past state History shows, read in place of the document while it is set.
    preview: Option<Preview>,
    /// The step whose state the last build showed, by `seq`; `None` for the document as it is.
    built_preview: Option<u64>,
    /// Where the render was drawn last frame, for what a host draws over it (the Theatre's highlight and notes).
    rect: Option<Rect>,
    /// Each part's attachment from the last build; `None` for a part its kind places by itself.
    attaches: BTreeMap<PartId, Option<Attach>>,
    /// What the handles do.
    tool: Tool,
    /// The handle under the pointer.
    hot: Option<Handle>,
    /// The drag under way.
    placing: Option<Placing>,
    /// Why the last drag couldn't place the part.
    refused: Option<String>,
}

/// A handle drag under way: its transaction and what it writes.
struct Placing {
    drag: Drag,
    tx: TxId,
    character: CharId,
    part: PartId,
    /// The attachment as it was when the drag started (after the pin).
    attach: Attach,
    /// The offset last written.
    written: AttachOffset,
    /// Whether the transaction holds a change (the pin, or a written offset).
    changed: bool,
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
            preview: None,
            built_preview: None,
            rect: None,
            attaches: BTreeMap::new(),
            tool: Tool::default(),
            hot: None,
            placing: None,
            refused: None,
        }
    }

    /// What the handles do.
    pub const fn tool(&self) -> Tool {
        self.tool
    }

    /// Sets what the handles do (the toolbar, W, E and R).
    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
    }

    /// Why the last handle drag couldn't place the part, if it couldn't.
    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    /// The handles of the selected part as they land in the view on the last frame, if it has them.
    pub fn handles(&self) -> Option<Screen> {
        let rect = self.rect?;
        Screen::of(&self.camera, [rect.width(), rect.height()], self.handle_origin()?)
    }

    /// Where the render was drawn on the last frame.
    pub const fn rect(&self) -> Option<Rect> {
        self.rect
    }

    /// Part `part`'s box from the last build (bind pose), if it has a mesh.
    pub fn part_bounds(&self, part: &PartId) -> Option<Bounds> {
        self.parts.get(part).copied()
    }

    /// Every part's box from the last build.
    pub fn parts(&self) -> impl Iterator<Item = (&PartId, Bounds)> {
        self.parts.iter().map(|(id, b)| (id, *b))
    }

    /// Centres `bounds` and fits it, keeping the angles: the Theatre's follow mode.
    pub fn frame(&mut self, bounds: Bounds) {
        self.camera.frame(bounds);
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
        self.viewing_line(ui, &ds);
        // What is left of the panel, and only what shows of it: the render is the size of what is seen.
        let rect = ui.available_rect_before_wrap().intersect(ui.clip_rect());
        self.rect = Some(rect);
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let name = say(&ds, text::VIEWPORT, &[]);
        a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Other, true, name.as_ref()), |_| {});
        self.place(ui, &response, io, &ds);
        self.input(ui, &response);
        if self.focus.take_frame_request() {
            self.frame_selection();
        }
        self.submit(ui, rect);
        self.draw_selection(ui, rect, &ds);
        if let Some(screen) = self.handles() {
            let hot = self.placing.as_ref().map(|p| p.drag.handle).or(self.hot);
            let painter = ui.painter().with_clip_rect(rect);
            screen.draw(&painter, self.tool, hot, rect.min.to_vec2());
        }
        let problem = self.problem().map(|p| say(&ds, text::UNBUILT, &[("reason", &p)]).into_owned());
        if let Some(text) = problem.or_else(|| self.refused.clone()) {
            let at = rect.left_bottom() + egui::vec2(8.0, -24.0);
            ui.painter().text(at, egui::Align2::LEFT_TOP, text, ds.text.small.clone(), ds.theme.danger);
        }
    }

    /// Shows the past state `preview` holds in place of the document while it holds one (History's preview).
    pub fn set_preview(&mut self, preview: Option<Preview>) {
        self.preview = preview;
    }

    /// Builds the character again when the document or the open character changed, and hands it to the GPU side.
    ///
    /// A change shows on the next frame, as `shell.md` section 5 asks: there is no throttle, so a slider dragged in
    /// the inspector restages once per frame (a build and a settle; see `a_preview_shows_on_the_next_frame` for its
    /// cost) and never later than the frame after the edit.
    fn rebuild(&mut self, character: &CharId, io: Io<'_>) {
        let other = self.character.as_ref() != Some(character);
        let viewing = self.preview.as_ref().and_then(Preview::seq);
        if !self.watch.stale(io.events) && !other && viewing == self.built_preview {
            return;
        }
        self.watch.built();
        self.built_preview = viewing;
        let def = match &self.preview {
            Some(preview) if viewing.is_some() => preview.with(|v| past_character(v, character)),
            _ => io.character(character).map_err(|e| e.message),
        };
        let built = def.and_then(|def| {
            let def = def.ok_or_else(|| format!("there is no character '{character}'"))?;
            self.attaches = def.parts.iter().map(|(id, part)| (id.clone(), part.attach.clone())).collect();
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
            // The views, then the framing: two groups, so the eye finds each.
            ui.separator();
            if ui.add(Button::new(&say(ds, text::FRAME_SELECTION, &[])).ghost().shortcut_hint(Key::F)).clicked() {
                self.frame_selection();
            }
            if ui.add(Button::new(&say(ds, text::FRAME_CHARACTER, &[])).ghost().shortcut_hint(Key::Home)).clicked() {
                self.frame_character();
            }
            ui.separator();
            let (moving, rotating, scaling) =
                (say(ds, text::MOVE, &[]), say(ds, text::ROTATE, &[]), say(ds, text::SCALE, &[]));
            let segments = [
                Segment::new(Tool::Move, &moving),
                Segment::new(Tool::Rotate, &rotating),
                Segment::new(Tool::Scale, &scaling),
            ];
            ui.add(Segmented::new(&say(ds, text::HANDLES, &[]), &mut self.tool, &segments));
        });
    }

    /// While a past state is on show: what it is, and the way back.
    fn viewing_line(&mut self, ui: &mut Ui, ds: &crate::ds::Ds) {
        let Some(preview) = self.preview.clone() else { return };
        let Some(label) = preview.with(|v| v.map(|v| v.label.clone())) else { return };
        ui.horizontal_wrapped(|ui| {
            let line = say(ds, text::VIEWING_PAST, &[("label", &label)]);
            ui.add(egui::Label::new(egui::RichText::new(line.as_ref()).color(ds.theme.accent)).truncate());
            if ui.add(Button::new(&say(ds, text::BACK_TO_NOW, &[])).icon(icons::ARROW_U_UP_LEFT)).clicked() {
                preview.back();
            }
        });
    }

    /// The part under the point `at` of the view (from its top left), if any.
    pub fn part_at(&self, at: egui::Vec2) -> Option<pick::Hit> {
        let rect = self.rect?;
        let pick = self.job.lock().unwrap_or_else(PoisonError::into_inner).pick.clone()?;
        let (origin, direction) = self.camera.ray([at.x, at.y], [rect.width(), rect.height()]);
        pick.hit(origin, direction)
    }

    /// A click at `at`: selects the part there, or with `add` adds or removes it; a click on nothing clears.
    fn click(&mut self, at: egui::Vec2, add: bool) {
        match (self.part_at(at), add) {
            (Some(hit), true) => self.focus.toggle(&hit.part),
            (Some(hit), false) => self.focus.select(hit.part),
            (None, true) => {}
            (None, false) => self.focus.clear(),
        }
    }

    /// A box round each selected part, as posed, so the person sees what a click selected.
    fn draw_selection(&self, ui: &Ui, rect: Rect, ds: &crate::ds::Ds) {
        let Some(pick) = self.job.lock().unwrap_or_else(PoisonError::into_inner).pick.clone() else { return };
        let size = [rect.width(), rect.height()];
        for part in self.focus.selected() {
            let Some([x0, y0, x1, y1]) = pick.bounds(&part).and_then(|b| self.camera.project_bounds(b, size)) else {
                continue;
            };
            let boxed = Rect::from_min_max(rect.min + egui::vec2(x0, y0), rect.min + egui::vec2(x1, y1)).expand(3.0);
            ui.painter().with_clip_rect(rect).rect_stroke(
                boxed,
                4.0,
                egui::Stroke::new(1.5, ds.theme.accent),
                egui::StrokeKind::Outside,
            );
        }
    }

    /// The camera gestures and keys (`shell.md` section 5).
    fn input(&mut self, ui: &Ui, response: &egui::Response) {
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        let (alt, shift) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift));
        if response.clicked_by(PointerButton::Primary)
            && !alt
            && self.hot.is_none()
            && let Some(at) = response.interact_pointer_pos()
        {
            self.click(at - response.rect.min, shift);
        }
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
            for (tool, key) in [(Tool::Move, Key::W), (Tool::Rotate, Key::E), (Tool::Scale, Key::R)] {
                if pressed(key) {
                    self.tool = tool;
                }
            }
        }
    }

    /// Where the selected part's handles sit, in the character's space: the middle of its posed box. `None` for a part
    /// bent by several bones or simulated (hair, a garment, the body), which has no place to drag, unless an attachment
    /// already places it.
    fn handle_origin(&self) -> Option<Vec3> {
        let part = self.focus.primary()?;
        let pick = self.job.lock().unwrap_or_else(PoisonError::into_inner).pick.clone()?;
        let attached = self.attaches.get(&part).is_some_and(Option::is_some);
        if !attached && !pick.rigid(&part) {
            return None;
        }
        pick.bounds(&part).map(Bounds::centre)
    }

    /// The handles: which is under the pointer, and a drag of one, written through `part.set_attach`.
    fn place(&mut self, ui: &Ui, response: &egui::Response, io: Io<'_>, ds: &crate::ds::Ds) {
        let local = |p: egui::Pos2| p - response.rect.min.to_vec2();
        let (alt, ctrl) = ui.input(|i| (i.modifiers.alt, i.modifiers.command || i.modifiers.ctrl));
        if self.placing.is_none() {
            let screen = self.handles();
            self.hot = match (screen, response.hover_pos().or_else(|| response.interact_pointer_pos())) {
                (Some(screen), Some(pointer)) if !alt => screen.handle_at(self.tool, local(pointer)),
                _ => None,
            };
            if response.drag_started_by(PointerButton::Primary)
                && let (Some(handle), Some(screen), Some(pointer)) =
                    (self.hot, self.handles(), response.interact_pointer_pos())
            {
                self.start(io, ds, handle, screen, local(pointer));
            }
            return;
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            self.finish(io, false);
            return;
        }
        if response.dragged_by(PointerButton::Primary)
            && let Some(pointer) = response.interact_pointer_pos()
        {
            self.drag_to(io, local(pointer), ctrl);
        }
        if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            self.finish(io, true);
        }
    }

    /// Takes `handle`: opens the drag's transaction and pins the part first when its kind places it.
    fn start(&mut self, io: Io<'_>, ds: &crate::ds::Ds, handle: Handle, screen: Screen, start: egui::Pos2) {
        self.refused = None;
        let (Some(character), Some(part)) = (self.character.clone(), self.focus.primary()) else { return };
        let label = match self.tool {
            Tool::Move => text::MOVE_PART,
            Tool::Rotate => text::ROTATE_PART,
            Tool::Scale => text::SCALE_PART,
        };
        let tx = io.runner.begin(&say(ds, label, &[("part", part.as_str())]), io.actor);
        let pinned = match self.attaches.get(&part).cloned().flatten() {
            Some(attach) => Ok((attach, false)),
            None => io
                .run("part.set_attach", json!({"character": character, "part": part, "pin": true}), Some(tx))
                .and_then(|outcome| {
                    let attach = outcome.plan.output.and_then(|o| o.get("attach").cloned()).unwrap_or_default();
                    serde_json::from_value::<Attach>(attach)
                        .map_err(|e| omuzaki_ops_api::OpError::internal(format!("the pin answered no attachment: {e}")))
                })
                .map(|attach| (attach, true)),
        };
        let pick = self.job.lock().unwrap_or_else(PoisonError::into_inner).pick.clone();
        let target = match (&pinned, pick) {
            (Ok((attach, _)), Some(pick)) => pick.target_frame(attach).ok_or_else(|| {
                omuzaki_ops_api::OpError::invalid("attach", "the viewport can't find what the part is fixed to")
            }),
            (Ok(_), None) => Err(omuzaki_ops_api::OpError::internal("the character isn't staged yet")),
            (Err(e), _) => Err(e.clone()),
        };
        match (pinned, target) {
            (Ok((attach, changed)), Ok(target)) => {
                let from = *attach.offset();
                let drag = Drag { handle, start, screen, from, target };
                self.placing = Some(Placing { drag, tx, character, part, attach, written: from, changed });
            }
            (_, Err(e)) | (Err(e), _) => {
                // Nothing of the drag is kept: the pin, if it ran, goes with the transaction.
                if let Err(rollback) = io.runner.rollback(tx, io.actor) {
                    tracing::warn!("the refused drag's transaction didn't roll back: {}", rollback.message);
                }
                self.refused = Some(say(ds, text::CANT_PLACE, &[("reason", &e.message)]).into_owned());
            }
        }
    }

    /// The pointer at `pointer` (in the view): writes the offset the drag gives, when it changed.
    fn drag_to(&mut self, io: Io<'_>, pointer: egui::Pos2, snap: bool) {
        let Some(placing) = self.placing.as_mut() else { return };
        let offset = placing.drag.offset(pointer, snap);
        if offset == placing.written {
            return;
        }
        let attach = gizmo::with_offset(&placing.attach, offset);
        let args = json!({"character": placing.character, "part": placing.part, "attach": attach});
        match io.run("part.set_attach", args, Some(placing.tx)) {
            Ok(_) => {
                placing.written = offset;
                placing.changed = true;
            }
            Err(e) => self.refused = Some(e.message),
        }
    }

    /// Ends the drag: `keep` commits it as one undo step (or drops it when it changed nothing), else rolls it back.
    fn finish(&mut self, io: Io<'_>, keep: bool) {
        let Some(placing) = self.placing.take() else { return };
        let result = if keep && placing.changed {
            io.runner.commit(placing.tx, io.actor)
        } else {
            io.runner.rollback(placing.tx, io.actor)
        };
        if let Err(e) = result {
            self.refused = Some(e.message);
        }
        self.watch.force();
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

/// Character `id` as `viewing` holds it.
fn past_character(viewing: Option<&Viewing>, id: &CharId) -> Result<Option<CharacterDef>, String> {
    let Some(viewing) = viewing else { return Ok(None) };
    let Some(node) = omuzaki_doc_api::lookup(&viewing.doc, &omuzaki_char_api::paths::character(id)) else {
        return Ok(None);
    };
    CharacterDef::from_node(node).map(Some).map_err(|e| e.to_string())
}

/// The viewport as the shell shows it, over the shell's [`EditFocus`] and [`ViewportServices`]. It makes its
/// [`Viewport`] on the first frame the services are there, so a shell that shares them after the panel opened works.
struct ViewportPanel {
    focus: EditFocus,
    viewport: Option<Viewport>,
}

impl omuzaki_ui_api::panel::Panel for ViewportPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &omuzaki_ui_api::panel::PanelCtx<'_>) {
        if self.viewport.is_none() {
            self.viewport = ctx.view::<ViewportServices>().map(|s| Viewport::new(self.focus.clone(), s.clone()));
        }
        if let Some(viewport) = &mut self.viewport {
            viewport.set_preview(ctx.view::<Preview>().cloned());
            viewport.ui(ui, Io::from_ctx(ctx));
        } else {
            let ds = ds(ui.ctx());
            EmptyState::new(icons::CUBE, &say(&ds, text::NO_GPU, &[]), &say(&ds, text::NO_GPU_HOW, &[])).show(ui);
        }
    }

    /// Without a GPU the panel is still available, and says why it can't draw.
    fn available(&self, _ctx: &omuzaki_ui_api::panel::PanelCtx<'_>) -> bool {
        self.focus.character().is_some()
    }
}

omuzaki_ui_api::register_panel! {
    id: "viewport", title: "Viewport", icon: icons::CUBE,
    region: omuzaki_ui_api::registry::Region::Center, min_size: [480.0, 320.0], order: 10,
    kind: omuzaki_ui_api::registry::PanelKind::Primary,
    empty: omuzaki_ui_api::panel::EmptyState::new(text::VIEWPORT_EMPTY, text::VIEWPORT_EMPTY_HOW),
    build: |ctx| Box::new(ViewportPanel { focus: EditFocus::from_ctx(ctx), viewport: None }),
}
