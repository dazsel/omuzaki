//! [`OutputWindow`]: the clean window itself, an egui viewport that shows the program frame and nothing else.

use std::cell::OnceCell;
use std::time::Instant;

use egui::{
    Color32, Context, CursorIcon, Key, Pos2, Rect, TextureId, Ui, ViewportBuilder, ViewportCommand, ViewportId,
};

use omuzaki_stream_api::window::ClickThrough;

use super::hit_test::{AlphaMask, DesktopProbe, PixelHits, os_probe};
use super::plan::{OutputPlan, Shown};
use crate::aux_window::{Arrangement, PhysicalSize, PlacementBook, Transparency, fit_commands};

/// The viewport's id: one output window per app.
pub const VIEWPORT: &str = "omuzaki-output";

/// What the window shows: the program frame's texture, or the test card until the viewport hands one over.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Program {
    /// The texture the program frame was rendered into (shared with the viewport: a copy of nothing, a second render
    /// of nothing).
    pub texture: TextureId,
    /// Its size in pixels.
    pub size: PhysicalSize,
}

/// What the window reports back each frame: what the Live workspace shows next to the toggle.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct OutputStatus {
    /// Whether the window is open.
    pub open: bool,
    /// The client area the OS gave it, in physical pixels (once it has drawn).
    pub measured: Option<PhysicalSize>,
    /// Whether it is minimised: a minimised window can't be captured, and the UI says so.
    pub minimised: bool,
    /// Whether it fell back to the key from a transparent plan.
    pub fallback: bool,
    /// Frames presented since it opened.
    pub presented: u64,
    /// Presented frames per second over the last second.
    pub fps: f32,
}

/// The output window: its plan, its state, and where it was.
#[derive(Debug)]
pub struct OutputWindow {
    plan: OutputPlan,
    transparency: Transparency,
    placement: PlacementBook,
    status: OutputStatus,
    /// Start of the current one-second fps window and the frames presented in it.
    fps_window: Option<(Instant, u64)>,
    /// The scale the viewport's size and bounds were last planned at: the main viewport's when the window opened, then
    /// the output viewport's own once it has drawn. The two differ when the window sits on another monitor.
    planned_at: Option<f32>,
    /// Per-pixel click-through on the desktop.
    hits: PixelHits,
    /// The desktop pointer and keys: opened the first time a desktop character needs it (`None` inside once tried and
    /// unavailable).
    probe: OnceCell<Option<Box<dyn DesktopProbe>>>,
    /// Whether the way-back chord went down since the app last asked.
    way_back: bool,
}

impl OutputWindow {
    /// A closed window for `plan`, on a platform where a transparent surface is `transparency`.
    pub fn new(plan: OutputPlan, transparency: Transparency) -> Self {
        Self {
            plan,
            transparency,
            placement: PlacementBook::new(),
            status: OutputStatus::default(),
            fps_window: None,
            planned_at: None,
            hits: PixelHits::default(),
            probe: OnceCell::new(),
            way_back: false,
        }
    }

    /// The window reading the desktop through `probe` instead of the OS's (tests).
    #[must_use]
    pub fn with_probe(mut self, probe: Box<dyn DesktopProbe>) -> Self {
        self.probe = OnceCell::from(Some(probe));
        self
    }

    /// Whether the way-back chord was pressed anywhere on the desktop since the last call, while every click passes
    /// through the character.
    pub fn take_way_back(&mut self) -> bool {
        std::mem::take(&mut self.way_back)
    }

    /// Whether the per-pixel click-through wants a fresh mask of the program frame now: the pointer is over a desktop
    /// character and the last mask is a quarter of a second old.
    pub fn wants_mask(&self) -> bool {
        self.status.open && self.per_pixel() && self.hits.wants_mask(Instant::now())
    }

    /// Takes the alpha of the program frame just presented, for the per-pixel click-through.
    pub fn set_mask(&mut self, frame: &omuzaki_render_api::Image) {
        if let Some(mask) = AlphaMask::from_image(frame) {
            self.hits.set_mask(mask, Instant::now());
        }
    }

    fn per_pixel(&self) -> bool {
        self.plan.desktop.is_some_and(|d| d.click_through == ClickThrough::Transparent)
    }

    fn probe(&self) -> Option<&dyn DesktopProbe> {
        self.probe.get_or_init(os_probe).as_deref()
    }

    /// The plan.
    pub fn plan(&self) -> &OutputPlan {
        &self.plan
    }

    /// Replaces the plan; an open window follows it on its next frame.
    pub fn set_plan(&mut self, plan: OutputPlan) {
        self.plan = plan;
    }

    /// The backdrop the window shows.
    pub fn shown(&self) -> Shown {
        self.plan.shown(self.transparency)
    }

    /// The last status.
    pub fn status(&self) -> OutputStatus {
        OutputStatus { fallback: self.shown().fallback, ..self.status }
    }

    /// Opens or closes the window.
    pub fn set_open(&mut self, open: bool) {
        if open != self.status.open {
            self.status = OutputStatus { open, ..OutputStatus::default() };
            self.fps_window = None;
            self.planned_at = None;
        }
    }

    /// Where it was, per monitor arrangement (for the settings to save).
    pub fn placement(&self) -> &PlacementBook {
        &self.placement
    }

    /// Restores remembered positions (from the settings).
    pub fn set_placement(&mut self, placement: PlacementBook) {
        self.placement = placement;
    }

    /// Draws the window for this frame of the app, if it is open. Call once per frame from the main viewport.
    pub fn show(&mut self, ctx: &Context, program: Option<Program>) {
        if !self.status.open {
            return;
        }
        // The way back from full click-through: the window takes no keys then, so the chord is read from the desktop.
        if self.plan.desktop.is_some_and(|d| d.click_through == ClickThrough::Full) {
            let held = self.probe().is_some_and(DesktopProbe::way_back_held);
            self.way_back |= self.hits.way_back(held);
        }
        if ctx.embed_viewports() {
            // A backend without OS windows (a test harness, the web) would draw it as a panel inside the editor, which
            // is exactly what the output must never be: it stays closed there.
            self.set_open(false);
            return;
        }
        let builder = self.builder(ctx);
        ctx.show_viewport_immediate(ViewportId::from_hash_of(VIEWPORT), builder, |ui, _class| self.frame(ui, program));
        // The main viewport repaints at least as often as the window presents, so a covered window keeps its cadence.
        ctx.request_repaint_after(self.plan.frame_interval());
    }

    /// The viewport the plan asks for. The position is read at the main viewport's scale (the arrangement it was saved
    /// under); the size and bounds are planned at the scale the output viewport has been seen at, else the main one.
    fn builder(&mut self, ctx: &Context) -> ViewportBuilder {
        let ppp = ctx.pixels_per_point();
        let monitor = ctx.input(|i| i.viewport().monitor_size).map(|size| PhysicalSize::of_points(size, ppp));
        let position = monitor
            .and_then(|monitor| self.placement.restore(&[Arrangement::new(monitor, ppp)]))
            .map(|(x, y)| physical_to_points(x, y, ppp));
        let planned = *self.planned_at.get_or_insert(ppp);
        self.plan.viewport(planned, self.shown(), position)
    }

    /// What brings the output viewport back to the plan, given the physical size it measures and the scale `ppp` it is
    /// drawn at now: its size and both bounds, at that scale. `None` while it already is the planned size and its
    /// bounds are planned at this scale. Moving to a monitor of another scale needs it even when the pixels happen to
    /// come out right, because the bounds are in points.
    fn refit(&mut self, measured: PhysicalSize, ppp: f32) -> Option<[ViewportCommand; 3]> {
        let settled = measured == self.plan.size && self.planned_at.is_some_and(|at| at.to_bits() == ppp.to_bits());
        if settled {
            return None;
        }
        let commands = fit_commands(self.plan.size, ppp);
        if commands.is_some() {
            self.planned_at = Some(ppp);
        }
        commands
    }

    /// One frame inside the output viewport.
    fn frame(&mut self, ui: &mut Ui, program: Option<Program>) {
        let ctx = ui.ctx().clone();
        let (rect, ppp, info) = ctx.input(|i| (i.viewport().inner_rect, i.pixels_per_point(), i.viewport().clone()));
        if info.close_requested() || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.set_open(false);
            return;
        }
        self.status.minimised = info.minimized.unwrap_or(false);
        if let Some(rect) = rect {
            let measured = PhysicalSize::of_points(rect.size(), ppp);
            self.status.measured = Some(measured);
            for command in self.refit(measured, ppp).into_iter().flatten() {
                ctx.send_viewport_cmd(command);
            }
        }
        if let (Some(outer), Some(monitor)) = (info.outer_rect, info.monitor_size) {
            let corner = outer.min * ppp;
            #[allow(clippy::cast_possible_truncation)] // desktop coordinates fit i32
            let (x, y) = (corner.x.round() as i32, corner.y.round() as i32);
            self.placement.remember(Arrangement::new(PhysicalSize::of_points(monitor, ppp), ppp), x, y);
        }
        let shown = self.shown();
        let (opacity, drags) = (self.plan.opacity(), self.plan.drags());
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(shown.fill)).show(ui, |ui| {
            let area = ui.max_rect();
            let character = paint_output(ui.painter(), area, shown.fill, program, opacity);
            if self.plan.hide_cursor && ui.rect_contains_pointer(area) {
                ctx.set_cursor_icon(CursorIcon::None);
            }
            // On the desktop the character is the handle: a press on it moves the window.
            let pressed = ctx.input(|i| i.pointer.primary_pressed().then(|| i.pointer.interact_pos()).flatten());
            if drags && pressed.is_some_and(|at| character.unwrap_or(area).contains(at)) {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            if let Some(passing) = self.hit_test(rect, character, ppp) {
                ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(passing));
            }
        });
        self.presented();
        ctx.request_repaint_after(self.plan.frame_interval());
    }

    /// This frame's per-pixel click-through, for the window at `inner` (desktop points) with the frame drawn at
    /// `character` (window points): whether the window must pass clicks through from now, when that changes. Outside
    /// per-pixel mode it puts back the plan's whole-window state if the window was left in another one.
    fn hit_test(&mut self, inner: Option<Rect>, character: Option<Rect>, ppp: f32) -> Option<bool> {
        if !self.per_pixel() {
            let full = self.plan.desktop.is_some_and(|d| d.click_through == ClickThrough::Full);
            return self.hits.settle(full);
        }
        let inner = inner?;
        let cursor = self.probe()?.cursor();
        let window = Rect::from_min_size((inner.min.to_vec2() * ppp).to_pos2(), inner.size() * ppp);
        let frame = character.map(|c| Rect::from_min_size(window.min + c.min.to_vec2() * ppp, c.size() * ppp));
        self.hits.update(cursor, window, frame)
    }

    fn presented(&mut self) {
        self.status.presented += 1;
        let now = Instant::now();
        let (start, at_start) = *self.fps_window.get_or_insert((now, self.status.presented));
        let elapsed = now.duration_since(start).as_secs_f32();
        if elapsed >= 1.0 {
            #[allow(clippy::cast_precision_loss)] // frame counts per second are small
            let frames = (self.status.presented - at_start) as f32;
            self.status.fps = frames / elapsed;
            self.fps_window = Some((now, self.status.presented));
        }
    }
}

#[allow(clippy::cast_precision_loss)] // desktop coordinates are far below 2^24
fn physical_to_points(x: i32, y: i32, ppp: f32) -> Pos2 {
    Pos2::new(x as f32 / ppp, y as f32 / ppp)
}

/// Paints the output: `fill` (the key, or transparent) over `area`, and the program frame over it at `opacity`
/// (0 to 1), fitted and centred; returns where the frame went. Nothing else, ever: no editor chrome, overlay or cursor
/// is drawn into the output.
pub fn paint_output(
    painter: &egui::Painter,
    area: Rect,
    fill: Color32,
    program: Option<Program>,
    opacity: f32,
) -> Option<Rect> {
    painter.rect_filled(area, 0.0, fill);
    let program = program?;
    #[allow(clippy::cast_precision_loss)] // texture sides are far below 2^24
    let size = egui::vec2(program.size.width as f32, program.size.height as f32);
    if size.x <= 0.0 || size.y <= 0.0 {
        return None;
    }
    let scale = (area.width() / size.x).min(area.height() / size.y);
    let shown = Rect::from_center_size(area.center(), size * scale);
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    // The frame is premultiplied, so a white tint of this alpha fades colour and coverage together.
    painter.image(program.texture, shown, uv, Color32::WHITE.gamma_multiply(opacity.clamp(0.0, 1.0)));
    Some(shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_window::plan::{DEFAULT_KEY, OutputBackdrop};

    #[test]
    fn a_closed_window_reports_closed_and_opening_resets_its_counts() {
        let mut window = OutputWindow::new(OutputPlan::default(), Transparency::Fallback);
        assert!(!window.status().open);
        window.status.presented = 9;
        window.set_open(true);
        assert_eq!(window.status(), OutputStatus { open: true, ..OutputStatus::default() });
        window.set_open(true);
        assert!(window.status().open, "opening an open window changes nothing");
    }

    #[test]
    fn a_minimised_window_is_published_as_minimised_for_the_ops() {
        use omuzaki_stream_api::window::WindowHost as _;
        let host = crate::output_window::OutputHost::new(Transparency::Fallback, PhysicalSize::FULL_HD);
        let mut window = OutputWindow::new(OutputPlan::default(), Transparency::Fallback);
        window.set_open(true);
        window.status.minimised = true;
        host.publish(&window, 1.0);
        let omuzaki_stream_api::window::WindowState::Open { minimised, .. } = host.state() else {
            panic!("the window is open")
        };
        assert!(minimised, "status says it can't be captured");
    }

    #[test]
    fn the_status_says_fallback_for_a_transparent_plan_that_is_not_verified() {
        let plan =
            OutputPlan { backdrop: OutputBackdrop::Transparent { fallback: DEFAULT_KEY }, ..OutputPlan::default() };
        assert!(OutputWindow::new(plan.clone(), Transparency::Fallback).status().fallback);
        assert!(!OutputWindow::new(plan, Transparency::Verified).status().fallback);
        assert!(!OutputWindow::new(OutputPlan::default(), Transparency::Fallback).status().fallback);
    }

    #[test]
    fn moving_to_a_monitor_of_another_scale_refits_the_size_and_both_bounds_at_that_scale() {
        let ctx = Context::default();
        let mut window = OutputWindow::new(OutputPlan::default(), Transparency::Fallback);
        window.set_open(true);
        // Opened from the main viewport at 100 %: the viewport is planned in its points.
        let opened = window.builder(&ctx);
        assert_eq!(opened.min_inner_size, Some(egui::vec2(1920.0, 1080.0)));
        assert_eq!(window.refit(PhysicalSize::FULL_HD, 1.0), None, "right size, right scale: nothing to do");

        // It lands on a 150 % monitor, where those points are 2880 x 1620 pixels. All three are corrected, at 150 %.
        let at_150 = egui::vec2(1280.0, 720.0);
        let moved = window.refit(PhysicalSize::new(2880, 1620), 1.5);
        assert_eq!(
            moved,
            Some([
                ViewportCommand::InnerSize(at_150),
                ViewportCommand::MinInnerSize(at_150),
                ViewportCommand::MaxInnerSize(at_150),
            ])
        );
        assert_eq!(window.refit(PhysicalSize::FULL_HD, 1.5), None, "settled at the new scale");
        // The viewport egui is handed each frame follows, so it can't pull the bounds back to the main viewport's.
        let builder = window.builder(&ctx);
        assert_eq!(
            (builder.inner_size, builder.min_inner_size, builder.max_inner_size),
            (Some(at_150), Some(at_150), Some(at_150))
        );

        // Back on the 100 % monitor the OS may keep 1920 x 1080 pixels; the bounds are still 150 %'s points.
        let back = window.refit(PhysicalSize::FULL_HD, 1.0).expect("the stale bounds are corrected");
        assert!(back.iter().all(|command| matches!(
            command,
            ViewportCommand::InnerSize(v) | ViewportCommand::MinInnerSize(v) | ViewportCommand::MaxInnerSize(v)
                if *v == egui::vec2(1920.0, 1080.0)
        )));
    }

    #[test]
    fn a_scale_that_is_not_a_number_is_not_planned_at() {
        let mut window = OutputWindow::new(OutputPlan::default(), Transparency::Fallback);
        assert_eq!(window.refit(PhysicalSize::new(10, 10), 0.0), None);
        assert_eq!(window.planned_at, None);
    }
}
