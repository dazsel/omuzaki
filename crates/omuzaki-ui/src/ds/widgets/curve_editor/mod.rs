//! `CurveEditor` (`design-system.md` 7.5): edits a response curve for input mapping (PF-22), exaggeration (FE-06) and
//! expression intensity (FE-03), and is the base of the Shots curve editor (SH-02).
//!
//! ```text
//!  [ S-curve ▾ ]
//!  1 ┤·····················╭──
//!    │               ╭─────╯ ○
//!    │          ○───╯
//!  0 ┼─○────────────────────────
//!    0          0.5          1
//!   Input      Output     Tangent
//!   [0.30]     [0.20]     [0.97]
//! ```
//!
//! The plot is a 4 by 4 grid with labels, a dotted diagonal for reference, the curve as one polyline of 128 segments
//! drawn from the 256-entry table, control points with 10 pt handles inside 24 pt hit targets, a faint copy of the
//! curve as it was when the drag began, and (for tracker curves) a live marker at the current input. The maths is in
//! [`Curve`]; this file is the drawing and the interaction.
//!
//! **Pointer:** drag a point (Shift keeps the axis it started along, Ctrl snaps to the grid, Esc puts it back); double-click
//! the plot to add a point on the curve; drag a point off the plot to remove it. **Keyboard** (WCAG 2.5.7): Tab and
//! Shift+Tab choose a point; the arrows nudge it by 1% of the axis (Shift 10%); Insert adds a point after it; Delete
//! removes it; Enter types its numbers in the readout, which also does everything the pointer does without a drag. The
//! plot is a group whose children are the points, each a slider that answers `Increment`, `Decrement` and `SetValue` on
//! its output, with a text summary of the curve ("Curve with 4 points. Input 0.5 gives 0.62.").
//!
//! A drag reports [`CurveEvent::Preview`] on every move and one [`CurveEvent::Commit`] at the end; a nudge, an
//! insertion, a removal, a typed number and a preset each commit at once. Esc reports [`CurveEvent::Rollback`].

mod curve;

use egui::{
    Align2, EventFilter, Id, Key, Modifiers, PointerButton, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, UiBuilder,
    Vec2, Widget, WidgetInfo, WidgetType, accesskit, pos2, vec2,
};

pub use curve::{Curve, CurveError, CurvePoint, LUT_SIZE, MAX_POINTS, MIN_POINTS, Preset};

use super::dropdown::{Choice, Dropdown};
use super::ids;
use super::number_field::NumberField;
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::text::NumberBuf;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Theme, space};
use crate::register_contrast;

/// What a curve editor's salt is namespaced by (see [`ids`]).
const KIND: &str = "ds.curve_editor";

/// What changed a curve, for the transaction label (P3).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurveCause {
    /// A point was dragged.
    Drag,
    /// An arrow key or a screen reader moved a point.
    Keyboard,
    /// A point was added.
    Added,
    /// A point was removed.
    Removed,
    /// A number typed in the readout.
    Readout,
    /// A preset replaced the curve.
    Preset,
}

/// What happened to the curve this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum CurveEvent {
    /// A drag moved a point: apply the curve live.
    Preview,
    /// A gesture finished: commit one transaction from `from` (the curve before it) to the curve now.
    Commit {
        /// The curve as it was before the gesture.
        from: Curve,
        /// What produced the change.
        cause: CurveCause,
    },
    /// Esc during a drag: the curve was put back as it was when the drag began.
    Rollback,
}

/// The result of showing a [`CurveEditor`].
#[derive(Debug)]
pub struct CurveOutput {
    /// The plot as a group; `changed()` whenever the curve changed this frame.
    pub response: Response,
    /// What happened to the curve, if anything.
    pub event: Option<CurveEvent>,
}

/// A curve editor bound to `curve`.
#[must_use = "a curve editor does nothing until it is shown"]
pub struct CurveEditor<'a> {
    label: &'a str,
    curve: &'a mut Curve,
    plot_height: f32,
    live: Option<f32>,
    disabled: Option<&'a str>,
    /// Why the tangent can't be edited, for a host whose format has no tangents.
    no_tangents: Option<&'a str>,
    salt: Option<Id>,
}

/// Room left of the plot for the output labels, and above and right of it.
const MARGIN_LEFT: f32 = 32.0;
const MARGIN_TOP: f32 = 6.0;
const MARGIN_RIGHT: f32 = 10.0;
/// Room under the plot for the input labels.
const MARGIN_BOTTOM: f32 = 16.0;
/// A point's handle: 10 pt across, in a 24 pt target (WCAG 2.5.8).
const HANDLE_RADIUS: f32 = 5.0;
const HIT: f32 = 24.0;
/// How far outside the plot a dragged point must be released to be removed.
const DRAG_OFF: f32 = 20.0;
/// Height of a readout: its caption and its field.
const READOUT_CAPTION: f32 = 14.0;
/// Height of a readout field.
const FIELD_HEIGHT: f32 = 22.0;
/// Arrow-key nudge as a fraction of an axis; Shift multiplies it by ten.
const NUDGE: f32 = 0.01;

impl<'a> CurveEditor<'a> {
    /// An editor named `label` (the accessible name) for `curve`.
    pub fn new(label: &'a str, curve: &'a mut Curve) -> Self {
        a11y::require_label(label);
        Self { label, curve, plot_height: 160.0, live: None, disabled: None, no_tangents: None, salt: None }
    }

    /// The plot's height; 160 pt by default. Its width is the room it is given.
    pub const fn plot_height(mut self, height: f32) -> Self {
        self.plot_height = height;
        self
    }

    /// The current input of a live signal (a tracker channel): a vertical line with a dot riding the curve. The host
    /// repaints at up to 60 Hz while it moves; the editor never asks for frames itself.
    pub const fn live_input(mut self, x: Option<f32>) -> Self {
        self.live = x;
        self
    }

    /// Disables the editor, saying why.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Turns the tangent field off, saying why: for a host that stores points only and would lose a tangent.
    pub const fn without_tangents(mut self, reason: &'a str) -> Self {
        self.no_tangents = Some(reason);
        self
    }

    /// Keys the editor by `salt` instead of by position.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(Id::new(salt));
        self
    }

    /// Shows the editor and reports what happened to the curve.
    pub fn show(self, ui: &mut Ui) -> CurveOutput {
        Show::new(self, ui).run(ui)
    }
}

impl Widget for CurveEditor<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui).response
    }
}

/// Whether `key` went down this frame as a new press, and not as the auto-repeat of a key that is held: a held Insert
/// or Delete adds or removes one point, not a point per repeat. The key is consumed either way.
fn fresh(input: &mut egui::InputState, key: Key) -> bool {
    let pressed = input
        .events
        .iter()
        .any(|e| matches!(e, egui::Event::Key { key: k, pressed: true, repeat: false, .. } if *k == key));
    input.consume_key(Modifiers::NONE, key) && pressed
}

/// What a key asks of the focused point.
enum Act {
    Nudge(f32, f32),
    Insert,
    Delete,
    Edit,
}

/// A drag in progress, and the curve as it was when it began.
#[derive(Clone)]
struct Gesture {
    from: Curve,
    index: usize,
    cancelled: bool,
}

/// What the editor remembers between frames.
#[derive(Clone, Default)]
struct State {
    selected: usize,
    gesture: Option<Gesture>,
}

/// The plot's rectangle and the maps between the curve's axes and the screen.
#[derive(Clone, Copy)]
struct Plot {
    rect: Rect,
    x: (f32, f32),
    y: (f32, f32),
}

impl Plot {
    fn screen_at(&self, x: f32, y: f32) -> Pos2 {
        pos2(
            self.rect.left() + (x - self.x.0) / (self.x.1 - self.x.0) * self.rect.width(),
            self.rect.bottom() - (y - self.y.0) / (self.y.1 - self.y.0) * self.rect.height(),
        )
    }

    fn data_at(&self, p: Pos2) -> (f32, f32) {
        (
            self.x.0 + (p.x - self.rect.left()) / self.rect.width() * (self.x.1 - self.x.0),
            self.y.0 + (self.rect.bottom() - p.y) / self.rect.height() * (self.y.1 - self.y.0),
        )
    }
}

struct Show<'a> {
    e: CurveEditor<'a>,
    ds: std::sync::Arc<Ds>,
    enabled: bool,
    id: Id,
}

impl<'a> Show<'a> {
    fn new(e: CurveEditor<'a>, ui: &mut Ui) -> Self {
        let ds = ds(ui.ctx());
        let enabled = ui.is_enabled() && e.disabled.is_none();
        let id = e.salt.map_or_else(|| ui.next_auto_id().with("curve"), |salt| ids::salted(ui, KIND, salt));
        Self { e, ds, enabled, id }
    }

    fn run(mut self, ui: &mut Ui) -> CurveOutput {
        let ctx = ui.ctx().clone();
        let id = self.id;
        let mut state: State = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
        state.selected = state.selected.min(self.e.curve.points().len() - 1);
        let before = (state.selected, state.gesture.as_ref().map(|g| g.cancelled));
        let mut event = None;
        let mut changed = false;

        // The preset row, then the plot, then the readout of the selected point: top to bottom, so Tab follows the eye.
        if let Some(from) = self.presets(ui) {
            event = Some(CurveEvent::Commit { from, cause: CurveCause::Preset });
            changed = true;
        }
        let (plot_event, mut response) = self.plot(ui, &mut state, &mut changed);
        event = plot_event.or(event);
        if let Some(from) = self.readout(ui, &state) {
            event = Some(CurveEvent::Commit { from, cause: CurveCause::Readout });
            changed = true;
        }

        // Written back only when it changed: the gesture's copy of the curve is not cloned every frame of a drag.
        if (state.selected, state.gesture.as_ref().map(|g| g.cancelled)) != before {
            ctx.data_mut(|d| d.insert_temp(id, state));
        }
        if changed {
            response.mark_changed();
        }
        CurveOutput { response, event }
    }

    /// The preset dropdown. Returns the curve as it was when a preset replaced it.
    fn presets(&mut self, ui: &mut Ui) -> Option<Curve> {
        let ds = std::sync::Arc::clone(&self.ds);
        let names = [
            ds.text(Msg::CurveLinear),
            ds.text(Msg::CurveEaseIn),
            ds.text(Msg::CurveEaseOut),
            ds.text(Msg::CurveSCurve),
            ds.text(Msg::CurveDeadZone),
            ds.text(Msg::CurveInverted),
            ds.text(Msg::CurveStep),
        ];
        let choices: [Choice<'_, Option<Preset>>; 7] =
            std::array::from_fn(|i| Choice::new(Some(Preset::ALL[i]), &names[i]));
        let custom = ds.text(Msg::CurveCustom);
        let mut chosen = self.e.curve.preset();
        let preset_label = ds.text(Msg::CurvePreset);
        let mut dropdown =
            Dropdown::new(&preset_label, &mut chosen, &choices).placeholder(&custom).id_salt(self.id.with("presets"));
        if !self.enabled {
            dropdown = dropdown.disabled(self.e.disabled.unwrap_or_default());
        }
        let picked = ui.add(dropdown).changed();
        let preset = chosen.filter(|_| picked)?;
        let from = self.e.curve.clone();
        self.e.curve.apply_preset(preset);
        Some(from)
    }

    /// The plot: grid, curve, points and the pointer and keyboard that edit them.
    fn plot(&mut self, ui: &mut Ui, state: &mut State, changed: &mut bool) -> (Option<CurveEvent>, Response) {
        let ds = std::sync::Arc::clone(&self.ds);
        let ctx = ui.ctx().clone();
        let width = ui.available_width();
        let (outer, _) =
            ui.allocate_exact_size(vec2(width, MARGIN_TOP + self.e.plot_height + MARGIN_BOTTOM), Sense::hover());
        let rect = Rect::from_min_max(
            pos2(outer.left() + MARGIN_LEFT, outer.top() + MARGIN_TOP),
            pos2(outer.right() - MARGIN_RIGHT, outer.top() + MARGIN_TOP + self.e.plot_height),
        );
        let plot = Plot { rect: paint::snap(&ctx, rect), x: self.e.curve.domain(), y: self.e.curve.range() };
        let group = self.id.with("plot");
        let a11y_on = ctx.accesskit_node_builder(group, |node| node.set_role(accesskit::Role::Group)).is_some();

        if ui.is_rect_visible(outer) {
            self.paint_plot(ui, &ds, &plot, state);
        }

        let mut event = None;
        // Points are children of the plot's group node only while a screen reader is listening: making the child `Ui`
        // costs an allocation, and the plot must not cost any per frame.
        let mut child;
        let ui = if a11y_on {
            child = ui.new_child(UiBuilder::new().id_salt(group).max_rect(outer).accessibility_parent(group));
            &mut child
        } else {
            ui
        };
        // Clickable (a double-click adds a point) but not a Tab stop: the points are.
        let plot_response = ui.interact(plot.rect, group, Sense::CLICK);
        if self.enabled
            && plot_response.double_clicked()
            && let Some(pos) = plot_response.interact_pointer_pos()
        {
            let (x, _) = plot.data_at(pos);
            let from = self.e.curve.clone();
            if let Some(i) = self.e.curve.insert(x) {
                state.selected = i;
                ctx.memory_mut(|m| m.request_focus(self.point_id(i)));
                event = Some(CurveEvent::Commit { from, cause: CurveCause::Added });
                *changed = true;
            }
        }
        // The number of points is read again each time round: a removal (Delete, or a drag off the plot) shortens it.
        let mut i = 0;
        while i < self.e.curve.points().len() {
            if let Some(e) = self.point(ui, &ds, &plot, state, i, changed) {
                event = Some(e);
            }
            i += 1;
        }
        self.accessibility(&plot_response);
        (event, plot_response)
    }

    fn point_id(&self, i: usize) -> Id {
        self.id.with(("point", i))
    }

    /// Everything about point `i`: its hit target, drag, keys, drawing and node.
    fn point(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        plot: &Plot,
        state: &mut State,
        i: usize,
        changed: &mut bool,
    ) -> Option<CurveEvent> {
        let ctx = ui.ctx().clone();
        let t = &ds.theme;
        let count = self.e.curve.points().len();
        let p = *self.e.curve.points().get(i)?;
        let centre = plot.screen_at(p.x, p.y);
        let sense = if self.enabled { Sense::click_and_drag() } else { Sense::focusable_noninteractive() };
        let response = ui.interact(Rect::from_center_size(centre, Vec2::splat(HIT)), self.point_id(i), sense);
        let mut event = None;

        if response.has_focus() || response.clicked() || response.drag_started() {
            state.selected = i;
        }
        if self.enabled {
            event = self.drag(&ctx, &response, plot, state, i, changed);
            if response.has_focus() {
                event = self.keys(&ctx, state, i, changed).or(event);
            }
            event = self.assistive(&ctx, i, changed).or(event);
        }
        // The point may have moved this frame: draw it where it is now.
        let p = self.e.curve.points().get(i).copied().unwrap_or(p);
        let centre = plot.screen_at(p.x, p.y);
        let removing = state.gesture.as_ref().is_some_and(|g| g.index == i && !g.cancelled)
            && response.dragged()
            && self.outside(plot, &response);
        if ui.is_rect_visible(Rect::from_center_size(centre, Vec2::splat(HIT))) {
            let selected = state.selected == i;
            let painter = ui.painter();
            let hot = paint::hot(&response);
            let fill = if selected { t.accent } else { t.bg2 };
            let ring_ink = if removing { t.warn } else { t.accent };
            painter.circle_filled(centre, HANDLE_RADIUS + if hot { 1.0 } else { 0.0 }, fill);
            painter.circle_stroke(centre, HANDLE_RADIUS + if hot { 1.0 } else { 0.0 }, Stroke::new(2.0, ring_ink));
            if response.has_focus() && crate::ds::focus::visible(&ctx) {
                paint::draw_ring(
                    painter,
                    t,
                    Rect::from_center_size(centre, Vec2::splat(2.0 * HANDLE_RADIUS + 2.0)),
                    8,
                    RingPlacement::Outside,
                );
            }
        }
        self.describe_point(&response, i, count, self.e.curve.points().get(i).copied().unwrap_or(p));
        event
    }

    /// Whether the pointer is far enough off the plot that releasing here removes the point.
    fn outside(&self, plot: &Plot, response: &Response) -> bool {
        response.interact_pointer_pos().is_some_and(|pos| !plot.rect.expand(DRAG_OFF).contains(pos))
            && !self.e.curve.points().is_empty()
    }

    /// Dragging a point: moves it while the pointer is on the plot, removes it if released far off, Esc undoes it.
    fn drag(
        &mut self,
        ctx: &egui::Context,
        response: &Response,
        plot: &Plot,
        state: &mut State,
        i: usize,
        changed: &mut bool,
    ) -> Option<CurveEvent> {
        if response.drag_started_by(PointerButton::Primary) {
            state.gesture = Some(Gesture { from: self.e.curve.clone(), index: i, cancelled: false });
            ctx.memory_mut(|m| m.request_focus(response.id));
        }
        let gesture = state.gesture.as_mut().filter(|g| g.index == i)?;
        if gesture.cancelled {
            if response.drag_stopped() {
                state.gesture = None;
            }
            return None;
        }
        if ctx.input(|input| input.key_pressed(Key::Escape)) {
            *self.e.curve = gesture.from.clone();
            gesture.cancelled = true;
            *changed = true;
            return Some(CurveEvent::Rollback);
        }
        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
        {
            if plot.rect.expand(DRAG_OFF).contains(pos) {
                let (mut x, mut y) = plot.data_at(pos);
                let start = gesture.from.points()[i];
                let (shift, ctrl) = ctx.input(|input| (input.modifiers.shift, input.modifiers.command));
                if shift {
                    // Keep the axis the drag started along: the one it has moved further on, in screen terms.
                    let (dx, dy) = ((x - start.x) / (plot.x.1 - plot.x.0), (y - start.y) / (plot.y.1 - plot.y.0));
                    if dx.abs() >= dy.abs() { y = start.y } else { x = start.x }
                }
                if ctrl {
                    x = snap(x, plot.x);
                    y = snap(y, plot.y);
                }
                if self.e.curve.move_point(i, x, y) {
                    *changed = true;
                    return Some(CurveEvent::Preview);
                }
            }
            return None;
        }
        if response.drag_stopped() {
            let finished = state.gesture.take()?;
            let released_off =
                response.interact_pointer_pos().is_some_and(|pos| !plot.rect.expand(DRAG_OFF).contains(pos));
            if released_off && self.e.curve.remove(i) {
                state.selected = state.selected.saturating_sub(usize::from(state.selected >= i));
                *changed = true;
                return Some(CurveEvent::Commit { from: finished.from, cause: CurveCause::Removed });
            }
            if finished.from != *self.e.curve {
                return Some(CurveEvent::Commit { from: finished.from, cause: CurveCause::Drag });
            }
        }
        None
    }

    /// The keys of the focused point: nudge, insert, remove, edit the numbers.
    fn keys(&mut self, ctx: &egui::Context, state: &mut State, i: usize, changed: &mut bool) -> Option<CurveEvent> {
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                self.point_id(i),
                EventFilter { horizontal_arrows: true, vertical_arrows: true, ..EventFilter::default() },
            );
        });
        let act = ctx.input_mut(|input| {
            let k = if input.modifiers.shift { 10.0 * NUDGE } else { NUDGE };
            if input.key_pressed(Key::ArrowLeft) {
                Some(Act::Nudge(-k, 0.0))
            } else if input.key_pressed(Key::ArrowRight) {
                Some(Act::Nudge(k, 0.0))
            } else if input.key_pressed(Key::ArrowUp) {
                Some(Act::Nudge(0.0, k))
            } else if input.key_pressed(Key::ArrowDown) {
                Some(Act::Nudge(0.0, -k))
            } else if fresh(input, Key::Insert) {
                Some(Act::Insert)
            } else if fresh(input, Key::Delete) {
                Some(Act::Delete)
            } else if input.consume_key(Modifiers::NONE, Key::Enter) {
                Some(Act::Edit)
            } else {
                None
            }
        })?;
        let (id, readout) = (self.id, self.readout_id(1));
        let point_id = |k: usize| id.with(("point", k));
        let curve = &mut *self.e.curve;
        let from = curve.clone();
        let (x_span, y_span) = (curve.domain().1 - curve.domain().0, curve.range().1 - curve.range().0);
        let p = curve.points()[i];
        match act {
            Act::Nudge(dx, dy) => {
                curve.move_point(i, p.x + dx * x_span, p.y + dy * y_span).then_some(CurveCause::Keyboard)
            }
            Act::Insert => {
                // Between this point and the next, or the one before when this is the last: a point on the curve.
                let (a, b) = if i + 1 < curve.points().len() { (i, i + 1) } else { (i - 1, i) };
                let mid = f32::midpoint(curve.points()[a].x, curve.points()[b].x);
                curve.insert(mid).map(|new| {
                    state.selected = new;
                    ctx.memory_mut(|m| m.request_focus(point_id(new)));
                    CurveCause::Added
                })
            }
            Act::Delete => curve.remove(i).then(|| {
                state.selected = i - 1;
                ctx.memory_mut(|m| m.request_focus(point_id(i - 1)));
                CurveCause::Removed
            }),
            Act::Edit => {
                NumberField::request_edit(ctx, readout);
                None
            }
        }
        .map(|cause| {
            *changed = true;
            CurveEvent::Commit { from, cause }
        })
    }

    /// What a screen reader asks of point `i`: its output stepped or set, focused or not.
    fn assistive(&mut self, ctx: &egui::Context, i: usize, changed: &mut bool) -> Option<CurveEvent> {
        let id = self.point_id(i);
        let (steps, set) = ctx.input(|input| {
            let count = |action| input.num_accesskit_action_requests(id, action) as f32;
            let set =
                input.accesskit_action_requests(id, accesskit::Action::SetValue).last().and_then(|r| match &r.data {
                    Some(accesskit::ActionData::NumericValue(v)) => Some(*v as f32),
                    Some(accesskit::ActionData::Value(text)) => text.trim().parse::<f32>().ok(),
                    _ => None,
                });
            (count(accesskit::Action::Increment) - count(accesskit::Action::Decrement), set)
        });
        if steps == 0.0 && set.is_none() {
            return None;
        }
        let curve = &mut *self.e.curve;
        let from = curve.clone();
        let p = curve.points()[i];
        let y = set.filter(|v| v.is_finite()).unwrap_or(p.y + steps * NUDGE * (curve.range().1 - curve.range().0));
        curve.move_point(i, p.x, y).then(|| {
            *changed = true;
            CurveEvent::Commit { from, cause: CurveCause::Keyboard }
        })
    }

    fn readout_id(&self, which: usize) -> Id {
        self.id.with(("readout", which))
    }

    /// The numbers of the selected point: input, output and tangent, in a row under the plot. Returns the curve as it
    /// was when a typed number changed it.
    fn readout(&mut self, ui: &mut Ui, state: &State) -> Option<Curve> {
        let ds = std::sync::Arc::clone(&self.ds);
        let t = &ds.theme;
        let width = ui.available_width();
        let (row, _) = ui.allocate_exact_size(vec2(width, READOUT_CAPTION + FIELD_HEIGHT), Sense::hover());
        let i = state.selected;
        let p = self.e.curve.points()[i];
        let (x_range, y_range) = (self.e.curve.domain(), self.e.curve.range());
        let ends = i == 0 || i + 1 == self.e.curve.points().len();
        let slope = self.e.curve.slopes()[i];
        let auto = self.e.curve.automatic_slope(i);
        let column = (width - 2.0 * space::S8) / 3.0;
        let at = |k: f32| {
            Rect::from_min_size(
                pos2(row.left() + k * (column + space::S8), row.top() + READOUT_CAPTION),
                vec2(column, FIELD_HEIGHT),
            )
        };

        let mut values = [f64::from(p.x), f64::from(p.y), f64::from(slope)];
        let mut edited = None;
        for (k, (msg, range, decimals)) in [
            (Msg::CurveInput, (f64::from(x_range.0), f64::from(x_range.1)), 3),
            (Msg::CurveOutput, (f64::from(y_range.0), f64::from(y_range.1)), 3),
            (Msg::CurveTangent, (-1000.0, 1000.0), 2),
        ]
        .into_iter()
        .enumerate()
        {
            let name = ds.text(msg);
            #[allow(clippy::cast_precision_loss, reason = "a column index")]
            let rect = at(k as f32);
            paint::text_in(
                ui.painter(),
                Rect::from_min_size(rect.min - vec2(0.0, READOUT_CAPTION), vec2(rect.width(), READOUT_CAPTION)),
                Align2::LEFT_CENTER,
                &name,
                &ds.text.micro,
                t.text3,
            );
            let mut field = NumberField::new(&name, &mut values[k], range.0..=range.1)
                .decimals(decimals)
                .width(column)
                .step(if k == 2 { 0.1 } else { 0.01 });
            let pinned = ds.text(Msg::CurveEndPinned);
            if let Some(reason) = self.e.disabled {
                field = field.disabled(reason);
            } else if k == 0 && ends {
                field = field.disabled(&pinned);
            }
            if k == 2 {
                if let Some(reason) = self.e.no_tangents {
                    field = field.disabled(reason);
                }
                field = field.default_value(f64::from(auto)).modified(p.tangent.is_some());
            }
            let out = field.show_at(ui, self.readout_id(k), rect);
            if out.response.changed() || out.reset {
                edited = Some((k, out.reset));
            }
        }
        let (k, reset) = edited?;
        let from = self.e.curve.clone();
        #[allow(clippy::cast_possible_truncation, reason = "back to the curve's f32")]
        let moved = match k {
            0 => self.e.curve.move_point(i, values[0] as f32, p.y),
            1 => self.e.curve.move_point(i, p.x, values[1] as f32),
            _ if reset => self.e.curve.set_tangent(i, None),
            _ => self.e.curve.set_tangent(i, Some(values[2] as f32)),
        };
        moved.then_some(from)
    }

    /// Frame, grid with labels, diagonal, original curve, curve, live marker.
    fn paint_plot(&self, ui: &Ui, ds: &Ds, plot: &Plot, state: &State) {
        let t = &ds.theme;
        let painter = ui.painter();
        let curve = &*self.e.curve;
        painter.rect_filled(plot.rect, 2, t.bg2);
        // The 4 by 4 grid and the labels at its ends and middle.
        for k in 0..=4u8 {
            let f = f32::from(k) / 4.0;
            let (gx, gy) = (plot.rect.left() + f * plot.rect.width(), plot.rect.bottom() - f * plot.rect.height());
            let hairline = Stroke::new(1.0, t.line);
            painter.line_segment([pos2(gx, plot.rect.top()), pos2(gx, plot.rect.bottom())], hairline);
            painter.line_segment([pos2(plot.rect.left(), gy), pos2(plot.rect.right(), gy)], hairline);
            if k % 2 == 0 {
                let vx = plot.x.0 + f * (plot.x.1 - plot.x.0);
                let vy = plot.y.0 + f * (plot.y.1 - plot.y.0);
                let (nx, ny) = (NumberBuf::new(f64::from(vx), 1), NumberBuf::new(f64::from(vy), 1));
                // At the corner the two zero labels would meet: the input one starts at the corner and runs right, the
                // output one sits above the baseline.
                let (x_anchor, y_anchor) = if k == 0 {
                    (Align2::LEFT_CENTER, Align2::RIGHT_BOTTOM)
                } else {
                    (Align2::CENTER_CENTER, Align2::RIGHT_CENTER)
                };
                paint::text(
                    painter,
                    pos2(gx, plot.rect.bottom() + 12.0),
                    x_anchor,
                    nx.as_str(),
                    &ds.text.micro,
                    t.text3,
                );
                paint::text(painter, pos2(plot.rect.left() - 7.0, gy), y_anchor, ny.as_str(), &ds.text.micro, t.text3);
            }
        }
        painter.rect_stroke(plot.rect, 2, Stroke::new(t.control_stroke, t.border_control), egui::StrokeKind::Inside);
        // The diagonal, dotted: where input equals output.
        let (start, end) = (plot.screen_at(plot.x.0, plot.y.0), plot.screen_at(plot.x.1, plot.y.1));
        let length = (end - start).length();
        let dir = (end - start) / length.max(1.0);
        let mut along = 0.0;
        while along + 3.0 < length {
            painter.line_segment([start + dir * along, start + dir * (along + 3.0)], Stroke::new(1.0, t.text3));
            along += 8.0;
        }
        // The curve as it was when the drag began, and the curve.
        if let Some(gesture) = state.gesture.as_ref().filter(|g| !g.cancelled) {
            polyline(painter, plot, gesture.from.lut(), Stroke::new(1.5, t.text3));
        }
        polyline(painter, plot, curve.lut(), Stroke::new(2.0, t.accent));
        if let Some(x) = self.e.live {
            let input = x.clamp(plot.x.0, plot.x.1);
            let (top, bottom) = (plot.screen_at(input, plot.y.1), plot.screen_at(input, plot.y.0));
            painter.line_segment([top, bottom], Stroke::new(1.0, t.text2));
            let dot = plot.screen_at(input, curve.sample(input));
            painter.circle_filled(dot, 4.5, t.bg2);
            painter.circle_filled(dot, 3.0, t.text);
        }
    }

    /// The plot's group node: its summary, with the live input (or the middle of the domain) as the sample.
    fn accessibility(&self, response: &Response) {
        let ds = &*self.ds;
        let curve = &*self.e.curve;
        let (label, enabled) = (self.e.label, self.enabled);
        let x = self.e.live.unwrap_or_else(|| f32::midpoint(curve.domain().0, curve.domain().1));
        a11y::describe(
            response,
            || {
                let (count, input, output) = (
                    curve.points().len().to_string(),
                    NumberBuf::new(f64::from(x), 2).as_str().to_owned(),
                    NumberBuf::new(f64::from(curve.sample(x)), 2).as_str().to_owned(),
                );
                WidgetInfo::labeled(
                    WidgetType::Other,
                    enabled,
                    ds.text_with(
                        Msg::CurveSummary,
                        &[("label", label), ("count", &count), ("x", &input), ("y", &output)],
                    ),
                )
            },
            |node| node.set_role(accesskit::Role::Group),
        );
    }

    /// A point's node: a slider on its output, with both numbers as its text.
    fn describe_point(&self, response: &Response, i: usize, count: usize, p: CurvePoint) {
        let ds = &*self.ds;
        let (label, enabled) = (self.e.label, self.enabled);
        let (y_min, y_max) = self.e.curve.range();
        a11y::describe(
            response,
            || {
                let (index, total) = ((i + 1).to_string(), count.to_string());
                WidgetInfo::labeled(
                    WidgetType::Slider,
                    enabled,
                    ds.text_with(Msg::CurvePoint, &[("label", label), ("index", &index), ("count", &total)]),
                )
            },
            |node| {
                node.set_role(accesskit::Role::Slider);
                // Tab visits the points by input value, left to right, though a rising curve climbs the screen.
                node.set_class_name(a11y::ORDERED_BY_DATA);
                node.set_numeric_value(f64::from(p.y));
                node.set_min_numeric_value(f64::from(y_min));
                node.set_max_numeric_value(f64::from(y_max));
                node.set_numeric_value_step(f64::from(NUDGE * (y_max - y_min)));
                node.set_value(ds.text_with(
                    Msg::CurvePointValue,
                    &[
                        ("x", NumberBuf::new(f64::from(p.x), 2).as_str()),
                        ("y", NumberBuf::new(f64::from(p.y), 2).as_str()),
                    ],
                ));
                for action in [accesskit::Action::SetValue, accesskit::Action::Increment, accesskit::Action::Decrement]
                {
                    node.add_action(action);
                }
            },
        );
    }
}

/// `v` on the nearest of the five grid lines of `axis`.
fn snap(v: f32, axis: (f32, f32)) -> f32 {
    let span = axis.1 - axis.0;
    axis.0 + ((v - axis.0) / span * 4.0).round() / 4.0 * span
}

/// The table drawn as one polyline of 128 segments, as separate line segments so nothing allocates.
fn polyline(painter: &egui::Painter, plot: &Plot, lut: &[f32; LUT_SIZE], stroke: Stroke) {
    let at = |i: usize| {
        let x = plot.x.0 + (plot.x.1 - plot.x.0) * i as f32 / (LUT_SIZE - 1) as f32;
        plot.screen_at(x, lut[i])
    };
    for i in (0..LUT_SIZE - 1).step_by(2) {
        painter.add(Shape::line_segment([at(i), at((i + 2).min(LUT_SIZE - 1))], stroke));
    }
}

register_contrast!("curve_editor", pairs);

/// The curve, the handles, the labels and the grid on the plot's surface.
fn pairs(table: &mut Table<'_>) {
    let t: Theme = *table.theme();
    table.add("curve", t.accent, t.bg2, Rule::NonText);
    table.add("handle ring", t.accent, t.bg2, Rule::NonText);
    table.add("selected handle", t.accent, t.bg2, Rule::NonText);
    table.add("removal ring", t.warn, t.bg2, Rule::NonText);
    table.add("axis labels", t.text3, t.bg2, Rule::Text);
    table.add("original curve and diagonal", t.text3, t.bg2, Rule::NonText);
    table.add("live marker line", t.text2, t.bg2, Rule::NonText);
    table.add("plot border", t.border_control, t.bg2, Rule::NonText);
    table.add("readout caption", t.text3, t.bg1, Rule::Text);
}
