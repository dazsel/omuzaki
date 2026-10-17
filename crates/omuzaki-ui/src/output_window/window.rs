//! [`OutputWindow`]: the clean window itself, an egui viewport that shows the program frame and nothing else.

use std::time::Instant;

use egui::{Color32, Context, CursorIcon, Key, Pos2, Rect, TextureId, Ui, ViewportBuilder, ViewportId};

use super::plan::{OutputPlan, Shown};
use crate::aux_window::{Arrangement, PhysicalSize, PlacementBook, Transparency, resize_command};

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
}

impl OutputWindow {
    /// A closed window for `plan`, on a platform where a transparent surface is `transparency`.
    pub fn new(plan: OutputPlan, transparency: Transparency) -> Self {
        Self { plan, transparency, placement: PlacementBook::new(), status: OutputStatus::default(), fps_window: None }
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

    fn builder(&self, ctx: &Context) -> ViewportBuilder {
        let ppp = ctx.pixels_per_point();
        let monitor = ctx.input(|i| i.viewport().monitor_size).map(|size| PhysicalSize::of_points(size, ppp));
        let position = monitor
            .and_then(|monitor| self.placement.restore(&[Arrangement::new(monitor, ppp)]))
            .map(|(x, y)| physical_to_points(x, y, ppp));
        self.plan.viewport(ppp, self.shown(), position)
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
            if let Some(command) = resize_command(self.plan.size, measured, ppp) {
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
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(shown.fill)).show(ui, |ui| {
            let area = ui.max_rect();
            paint_output(ui.painter(), area, shown.fill, program);
            if self.plan.hide_cursor && ui.rect_contains_pointer(area) {
                ctx.set_cursor_icon(CursorIcon::None);
            }
        });
        self.presented();
        ctx.request_repaint_after(self.plan.frame_interval());
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

/// Paints the output: `fill` (the key, or transparent) over `area`, and the program frame over it, fitted and
/// centred. Nothing else, ever: no editor chrome, overlay or cursor is drawn into the output.
pub fn paint_output(painter: &egui::Painter, area: Rect, fill: Color32, program: Option<Program>) {
    painter.rect_filled(area, 0.0, fill);
    let Some(program) = program else { return };
    #[allow(clippy::cast_precision_loss)] // texture sides are far below 2^24
    let size = egui::vec2(program.size.width as f32, program.size.height as f32);
    if size.x <= 0.0 || size.y <= 0.0 {
        return;
    }
    let scale = (area.width() / size.x).min(area.height() / size.y);
    let shown = Rect::from_center_size(area.center(), size * scale);
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    painter.image(program.texture, shown, uv, Color32::WHITE);
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
}
