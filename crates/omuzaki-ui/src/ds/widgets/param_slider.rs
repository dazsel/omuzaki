//! `ParamSlider`: a slider with numeric entry and reset, the most-used control in the app (`design-system.md` 7.1).
//!
//! ```text
//!  ●  Width    ───────●──┆───   [ 0.62 ]  ↺
//! ```
//!
//! Anatomy: a "modified" dot, the label (38% of the row, 96 to 160 pt), the track (at least 80 pt), a mono numeric
//! field (56 x 22) and a reset button whose space is kept when hidden, so the row never shifts. The track fills from
//! the default, shows a tick where the default is, and has a 14 pt handle inside a full-height hit row.
//!
//! The widget reports gestures, not just values ([`ParamEvent`]): a drag is one open transaction that previews on
//! every move and commits when it ends; a typed entry, a reset and a key press each commit at once (P3).

use std::ops::RangeInclusive;

use egui::{
    Align, Align2, Area, Context, Frame, Id, Key, Modifiers, Order, Rect, Response, Sense, Stroke, StrokeKind,
    TextEdit, Ui, WidgetInfo, WidgetType, accesskit, pos2, vec2,
};

use super::ids;
use super::ime;
use super::keycap::Shortcut;
use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::expr::{self, ParseError};
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::text::{NumberBuf, decimals_for_range};
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Theme, dur, radius, size, space};
use crate::register_contrast;

/// The op that sets a parameter, for "Copy op call" (Ctrl+Shift+C) and the tooltip: the vocabulary an agent uses too (P8).
///
/// The slider does not know which op its value belongs to, so the host says. What is copied is an MCP tool call,
/// `{"name":"part_set_param","arguments":{"part":"hair","param":"width","value":0.62}}`, with the value as it is now and
/// the tool named as the MCP server names it ([`omuzaki_ops_api::tool_name`]).
#[derive(Clone, Copy, Debug)]
pub struct OpCall<'a> {
    /// The op's registered name (`part.set_param`).
    pub op: &'a str,
    /// A raw JSON fragment, copied as written (it is not escaped): the members of its arguments that say what to set,
    /// without braces and without the value:
    /// `"part":"hair","param":"width"`.
    pub target: &'a str,
}

impl OpCall<'_> {
    /// The call as JSON text, with `value` as the argument it sets.
    fn json(&self, value: f64) -> String {
        let comma = if self.target.is_empty() { "" } else { "," };
        let tool = omuzaki_ops_api::tool_name(self.op);
        format!("{{\"name\":\"{tool}\",\"arguments\":{{{}{comma}\"value\":{value}}}}}", self.target)
    }
}

/// An agent just set this value (P1, P4): who, when and why, so the row can say so.
#[derive(Clone, Copy, Debug)]
pub struct AgentTouch<'a> {
    /// The agent's name ("Designer").
    pub by: &'a str,
    /// Seconds since it set the value; drives the 900 ms glow.
    pub age_secs: f32,
    /// The prompt text or rule that caused it, for the tooltip.
    pub cause: Option<&'a str>,
    /// Identifies the change, so "seen" (hovered) is remembered per change rather than per row.
    pub stamp: u64,
}

/// What happened this frame, for the host to turn into a transaction (P3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamEvent {
    /// A drag began at this value: open a transaction.
    Begin(f64),
    /// The value moved during a drag: preview it.
    Preview(f64),
    /// The gesture finished: commit one transaction from `from` to `to`.
    Commit {
        /// The value before the gesture.
        from: f64,
        /// The value after it.
        to: f64,
        /// What produced it, for the transaction label.
        cause: CommitCause,
    },
    /// Esc during a drag: roll the open transaction back to this value.
    Rollback(f64),
    /// "Show in History" was chosen from the context menu: reveal this parameter's records in the History panel.
    ShowInHistory,
}

/// What kind of gesture committed a value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitCause {
    /// A drag ended.
    Drag,
    /// A click on the track jumped the handle.
    Click,
    /// A value was typed.
    Typed,
    /// The reset button, double-click on the label or Ctrl+Backspace.
    Reset,
    /// An arrow, Page or Home/End key.
    Keyboard,
    /// A number pasted from the clipboard, by the menu or Ctrl+V.
    Paste,
}

/// The result of showing a [`ParamSlider`].
#[derive(Debug)]
pub struct ParamOutput {
    /// The track's response: focus, hover, and `changed()` whenever the value changed.
    pub response: Response,
    /// The gesture events of this frame, in order (a drag's first frame is `Begin` then `Preview`).
    pub events: ParamEvents,
}

/// The events of one frame: at most three (begin, preview, commit), kept on the stack so showing a slider allocates
/// nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParamEvents {
    items: [Option<ParamEvent>; 3],
    len: usize,
}

impl ParamEvents {
    fn push(&mut self, event: ParamEvent) {
        if let Some(slot) = self.items.get_mut(self.len) {
            *slot = Some(event);
            self.len += 1;
        }
    }

    fn began(&self) -> bool {
        self.iter().any(|e| matches!(e, ParamEvent::Begin(_)))
    }

    /// The events, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &ParamEvent> {
        self.items[..self.len].iter().flatten()
    }

    /// The last event of the frame.
    pub fn last(&self) -> Option<&ParamEvent> {
        self.iter().last()
    }

    /// Whether nothing happened.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl IntoIterator for ParamEvents {
    type Item = ParamEvent;
    type IntoIter = std::iter::Flatten<std::array::IntoIter<Option<ParamEvent>, 3>>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter().flatten()
    }
}

/// A slider with numeric entry and reset, bound to `value`.
#[must_use = "a slider does nothing until it is shown"]
pub struct ParamSlider<'a> {
    label: &'a str,
    value: &'a mut f64,
    range: RangeInclusive<f64>,
    default: Option<f64>,
    step: Option<f64>,
    decimals: Option<usize>,
    unit: &'a str,
    variant_base: Option<f64>,
    agent: Option<AgentTouch<'a>>,
    mixed: bool,
    locked: Option<&'a str>,
    driven: Option<f64>,
    disabled: Option<&'a str>,
    op_call: Option<OpCall<'a>>,
    salt: Option<Id>,
}

/// What a slider's salt is namespaced by (see [`ids`]).
const KIND: &str = "ds.param_slider";

impl<'a> ParamSlider<'a> {
    /// A slider named `label` over `range`. The label is also the accessible name.
    pub fn new(label: &'a str, value: &'a mut f64, range: RangeInclusive<f64>) -> Self {
        a11y::require_label(label);
        debug_assert!(range.start() < range.end(), "a slider needs a non-empty range");
        Self {
            label,
            value,
            range,
            default: None,
            step: None,
            decimals: None,
            unit: "",
            variant_base: None,
            agent: None,
            mixed: false,
            locked: None,
            driven: None,
            disabled: None,
            op_call: None,
            salt: None,
        }
    }

    /// The default value: shows the tick, the reset button and the modified dot, and anchors the fill.
    pub const fn default_value(mut self, default: f64) -> Self {
        self.default = Some(default);
        self
    }

    /// The step for Ctrl-snapping and arrow keys (`x-ui.step`).
    pub const fn step(mut self, step: f64) -> Self {
        self.step = Some(step);
        self
    }

    /// Overrides the decimals shown (`x-ui.decimals`).
    pub const fn decimals(mut self, decimals: usize) -> Self {
        self.decimals = Some(decimals);
        self
    }

    /// The unit suffix (`x-ui.unit`), such as `"°"` or `"px"`.
    pub const fn unit(mut self, unit: &'a str) -> Self {
        self.unit = unit;
        self
    }

    /// Marks the value as overridden in the current variant; `base` is what the base character has.
    pub const fn variant_override(mut self, base: f64) -> Self {
        self.variant_base = Some(base);
        self
    }

    /// Marks the value as just set by an agent.
    pub const fn agent_set(mut self, touch: AgentTouch<'a>) -> Self {
        self.agent = Some(touch);
        self
    }

    /// Multi-selection with differing values: shows "—"; dragging sets them all.
    pub const fn mixed(mut self, mixed: bool) -> Self {
        self.mixed = mixed;
        self
    }

    /// Fenced: shows a lock and names who holds it. Not editable.
    pub const fn locked_by(mut self, owner: &'a str) -> Self {
        self.locked = Some(owner);
        self
    }

    /// A live input is driving this parameter; `normalised` (0 to 1) is drawn as a meter over the track.
    pub const fn driven(mut self, normalised: f64) -> Self {
        self.driven = Some(normalised);
        self
    }

    /// Disables the slider, saying why.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// The op that sets this value: adds "Copy op call" to the context menu, Ctrl+Shift+C, and the op name to the
    /// tooltip of the reset button's row.
    pub const fn op_call(mut self, call: OpCall<'a>) -> Self {
        self.op_call = Some(call);
        self
    }

    /// Keys the slider's state (keyboard focus, a drag or a typed entry in progress, the open menu and error bubble)
    /// by `salt` instead of by its position in the `Ui`, so reordering the rows of an inspector keeps a half-typed
    /// number with its parameter. Use a value that names the parameter, unique among the sliders of one `Ui`; the
    /// label is no good, since it is translated and two rows can share one.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(Id::new(salt));
        self
    }

    /// Shows the slider and reports its gesture. Prefer this to `ui.add` when the host keeps transactions.
    pub fn show(self, ui: &mut Ui) -> ParamOutput {
        Show::new(self, ui).run(ui)
    }
}

impl egui::Widget for ParamSlider<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui).response
    }
}

// ----------------------------------------------------------------------------
// Value maths: kept free of egui so it can be tested exactly.

/// Pure conversions between a value, a normalised position and a pointer coordinate.
mod math {
    /// The position of `value` in `min..=max` as 0 to 1.
    pub fn normalise(value: f64, min: f64, max: f64) -> f64 {
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    }

    /// The value at normalised position `t`.
    pub fn denormalise(t: f64, min: f64, max: f64) -> f64 {
        min + t.clamp(0.0, 1.0) * (max - min)
    }

    /// `value` rounded to the nearest multiple of `step` counted from `min`.
    pub fn snap(value: f64, step: f64, min: f64) -> f64 {
        if step <= 0.0 { value } else { min + ((value - min) / step).round() * step }
    }

    /// Rounds away binary noise (0.1 + 0.2) to `decimals` places so a nudged value prints and compares cleanly.
    pub fn tidy(value: f64, decimals: usize) -> f64 {
        let scale = 10f64.powi(i32::try_from(decimals.min(12)).unwrap_or(12));
        (value * scale).round() / scale
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn normalise_round_trips() {
            for v in [0.0, 0.25, 0.62, 1.0] {
                assert!((denormalise(normalise(v, 0.0, 1.0), 0.0, 1.0) - v).abs() < 1e-12);
            }
            assert!((normalise(15.0, 10.0, 20.0) - 0.5).abs() < 1e-12);
            assert!((normalise(-5.0, 0.0, 1.0)).abs() < 1e-12, "outside the range clamps");
        }

        #[test]
        fn snap_counts_from_the_minimum() {
            assert!((snap(0.62, 0.05, 0.0) - 0.60).abs() < 1e-12);
            assert!((snap(13.0, 5.0, 10.0) - 15.0).abs() < 1e-12);
            assert!((snap(0.62, 0.0, 0.0) - 0.62).abs() < 1e-12, "no step means no snapping");
        }

        #[test]
        fn tidy_removes_binary_noise() {
            assert!((tidy(0.1 + 0.2, 3) - 0.3).abs() < f64::EPSILON);
        }
    }
}

// ----------------------------------------------------------------------------
// Layout

/// Widths of the parts of a row, from its total width (`shell.md` section 4 gives the 288 pt minimum).
struct Columns {
    dot: Rect,
    label: Rect,
    track: Rect,
    field: Rect,
    reset: Rect,
}

fn columns(row: Rect) -> Columns {
    const GUTTER: f32 = 12.0;
    let h = row.height();
    let field_size = size::NUMERIC_FIELD;
    let reset_w = size::MIN_TARGET;
    let usable = row.width() - GUTTER;
    let label_w = (usable * size::LABEL_COLUMN).clamp(size::LABEL_COLUMN_MIN, size::LABEL_COLUMN_MAX);
    // The label column gives way first when the row is narrow (shell.md section 2): the track keeps its minimum.
    let fixed = field_size.x + reset_w + 3.0 * space::S6;
    let label_w = label_w.min((usable - fixed - size::TRACK_MIN).max(48.0));
    let at = |x: f32, w: f32| Rect::from_min_size(pos2(row.left() + x, row.top()), vec2(w, h));
    let label = at(GUTTER, label_w);
    let reset = at(row.width() - reset_w, reset_w);
    let field = Rect::from_center_size(pos2(reset.left() - space::S4 - field_size.x / 2.0, row.center().y), field_size);
    let track =
        Rect::from_min_max(pos2(label.right() + space::S6, row.top()), pos2(field.left() - space::S6, row.bottom()));
    Columns { dot: at(0.0, GUTTER), label, track, field, reset }
}

// ----------------------------------------------------------------------------
// Behaviour

/// One drag gesture, kept in the context between frames.
#[derive(Clone, Copy, Default)]
struct DragState {
    /// The value when the gesture began: what Esc restores and what the commit is "from".
    start_value: f64,
    /// Pointer x and value at the last (re)anchor, for relative and fine dragging.
    anchor_x: f32,
    anchor_value: f64,
    /// Shift is held: the pointer moves the value a tenth as far.
    fine: bool,
    /// The handle was grabbed, so the value follows pointer movement instead of jumping to the pointer.
    relative: bool,
    /// Esc was pressed: ignore the rest of the gesture.
    cancelled: bool,
}

/// What was chosen from the context menu.
#[derive(Clone, Copy)]
enum MenuItem {
    Reset,
    Copy,
    Paste,
    CopyOpCall,
    ShowInHistory,
}

/// What a key press on the focused track asks for.
enum Act {
    Move(f64),
    Absolute(f64),
    Reset,
    Edit,
    Copy,
    CopyOpCall,
    Paste(String),
}

/// Frames a "Paste value" from the menu waits for the clipboard's answer before giving up, so a paste that never
/// arrives (a host that cannot read the clipboard) is not taken by a Ctrl+V pressed much later somewhere else.
const PASTE_WAIT_FRAMES: u64 = 12;

#[derive(Clone, Default)]
struct Edit {
    buffer: String,
    error: Option<ParseError>,
}

struct Show<'a> {
    p: ParamSlider<'a>,
    ds: std::sync::Arc<Ds>,
    ctx: Context,
    /// Whether the slider can be operated.
    enabled: bool,
    /// Whether it is drawn as live. A slider disabled by its own flag (`disabled`, `locked_by`) draws dimmed; one in a
    /// disabled `Ui` is faded by egui, so it must not dim itself a second time.
    looks_enabled: bool,
    row: Rect,
    id: Id,
    min: f64,
    max: f64,
    decimals: usize,
}

impl<'a> Show<'a> {
    fn new(p: ParamSlider<'a>, ui: &Ui) -> Self {
        let ds = ds(ui.ctx());
        let (min, max) = (*p.range.start(), *p.range.end());
        let decimals = p.decimals.unwrap_or_else(|| decimals_for_range(min, max));
        let looks_enabled = p.disabled.is_none() && p.locked.is_none();
        let enabled = ui.is_enabled() && looks_enabled;
        let height = ds.metrics.control_row;
        let row = Rect::from_min_size(ui.available_rect_before_wrap().min, vec2(ui.available_width(), height));
        Self { p, ctx: ui.ctx().clone(), ds, enabled, looks_enabled, row, id: Id::NULL, min, max, decimals }
    }

    fn is_modified(&self) -> bool {
        self.p
            .default
            .is_some_and(|d| (*self.p.value - d).abs() > 0.5 * 10f64.powi(-i32::try_from(self.decimals).unwrap_or(3)))
    }

    fn step(&self) -> f64 {
        self.p.step.unwrap_or_else(|| 10f64.powi(-i32::try_from(self.decimals).unwrap_or(2)))
    }

    fn set(&mut self, value: f64) -> bool {
        let value = value.clamp(self.min, self.max);
        let changed = (value - *self.p.value).abs() > f64::EPSILON;
        *self.p.value = value;
        changed
    }

    fn run(mut self, ui: &mut Ui) -> ParamOutput {
        // Every other id of the slider (the track, the field, the drag, the menu) is derived from this one.
        let (_, row_response) = ids::allocate(ui, self.row.size(), Sense::hover(), KIND, self.p.salt);
        self.id = row_response.id;
        let cols = columns(self.row);
        let mut events = ParamEvents::default();
        let mut changed = false;

        // The track is the focusable, draggable part; the rest are small separate hit areas.
        let track_sense = if self.enabled { Sense::click_and_drag() } else { Sense::focusable_noninteractive() };
        let track = ui.interact(cols.track, self.id.with("track"), track_sense);
        let editing = self.is_editing();

        if self.enabled {
            changed |= self.pointer(&track, &cols, &mut events);
            if !editing {
                changed |= self.keyboard(&track, &mut events);
            }
            changed |= self.assistive(&track, &mut events);
            changed |= self.awaited_paste(&mut events);
        }
        let modified = self.is_modified();
        let reset_response = self.reset_button(ui, &cols, modified);
        if reset_response.clicked() && self.enabled {
            changed |= self.reset(&mut events);
        }
        // The label is not a widget: it is no Tab stop and no screen-reader node (the track already carries the
        // name), yet a double-click on it still resets, as the spec says.
        let label_hot = self.enabled && ui.rect_contains_pointer(cols.label);
        if label_hot && self.ctx.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
            changed |= self.reset(&mut events);
        }
        self.paint(ui, &cols, &track, label_hot, modified);
        changed |= self.numeric_field(ui, &cols, &track, editing, &mut events);

        let mut response = track.clone();
        if changed {
            response.mark_changed();
        }
        self.context_menu(&response, &mut events, modified);
        self.accessibility(&response, modified);
        ParamOutput { response, events }
    }

    // ---- pointer ---------------------------------------------------------

    fn x_range(track: Rect) -> (f32, f32) {
        let r = size::HANDLE / 2.0 + 1.0;
        (track.left() + r, track.right() - r)
    }

    fn value_at(&self, track: Rect, x: f32, snap: bool) -> f64 {
        let (lo, hi) = Self::x_range(track);
        let t = f64::from(((x - lo) / (hi - lo)).clamp(0.0, 1.0));
        let raw = math::denormalise(t, self.min, self.max);
        if snap { math::snap(raw, self.step(), self.min) } else { raw }
    }

    fn x_of(&self, track: Rect, value: f64) -> f32 {
        let (lo, hi) = Self::x_range(track);
        #[allow(clippy::cast_possible_truncation, reason = "a 0..1 fraction of an on-screen width")]
        let t = math::normalise(value, self.min, self.max) as f32;
        lo + t * (hi - lo)
    }

    /// The value a drag should have with the pointer at `x`.
    fn drag_target(&self, state: &DragState, track: Rect, x: f32, snap: bool) -> f64 {
        let (lo, hi) = Self::x_range(track);
        let per_point = (self.max - self.min) / f64::from(hi - lo);
        let moved = f64::from(x - state.anchor_x) * per_point;
        let raw = if state.fine {
            state.anchor_value + moved * 0.1
        } else if state.relative {
            state.anchor_value + moved
        } else {
            self.value_at(track, x, false)
        };
        let raw = raw.clamp(self.min, self.max);
        if snap { math::snap(raw, self.step(), self.min) } else { raw }
    }

    fn on_handle(&self, track: Rect, x: f32) -> bool {
        (x - self.x_of(track, *self.p.value)).abs() <= size::HANDLE / 2.0 + 3.0
    }

    fn pointer(&mut self, track: &Response, cols: &Columns, events: &mut ParamEvents) -> bool {
        let drag_id = self.id.with("drag");
        let mut changed = false;
        let mods = self.ctx.input(|i| i.modifiers);
        let press_x = self.ctx.input(|i| i.pointer.press_origin()).map_or(f32::NAN, |p| p.x);

        // A press and release without moving is a click: the handle jumps there (WCAG 2.5.7).
        if track.clicked()
            && let Some(pos) = track.interact_pointer_pos()
        {
            let before = *self.p.value;
            changed |= self.set(self.value_at(cols.track, pos.x, mods.ctrl));
            if changed {
                events.push(ParamEvent::Commit { from: before, to: *self.p.value, cause: CommitCause::Click });
            }
            track.request_focus();
        }

        if track.drag_started() && press_x.is_finite() {
            let start = *self.p.value;
            let mut state = DragState {
                start_value: start,
                anchor_x: press_x,
                anchor_value: start,
                fine: mods.shift,
                relative: mods.shift || self.on_handle(cols.track, press_x),
                cancelled: false,
            };
            events.push(ParamEvent::Begin(start));
            if !state.relative {
                // Grabbing the track away from the handle jumps there; grabbing the handle does not.
                changed |= self.set(self.value_at(cols.track, press_x, mods.ctrl));
                state.anchor_value = *self.p.value;
            }
            self.ctx.data_mut(|d| d.insert_temp(drag_id, state));
            track.request_focus();
        }

        // Esc during a drag restores the start value. egui itself ends the drag on Esc (so `drag_stopped` can fire in
        // the same frame), which is why this runs before the drag and the stop are handled.
        if self.ctx.input(|i| i.key_pressed(Key::Escape))
            && let Some(mut state) = self.ctx.data(|d| d.get_temp::<DragState>(drag_id))
            && !state.cancelled
        {
            state.cancelled = true;
            changed |= self.set(state.start_value);
            events.push(ParamEvent::Rollback(state.start_value));
            self.ctx.data_mut(|d| d.insert_temp(drag_id, state));
        }

        if track.dragged()
            && let (Some(mut state), Some(pos)) =
                (self.ctx.data(|d| d.get_temp::<DragState>(drag_id)), track.interact_pointer_pos())
        {
            if !state.cancelled {
                if state.fine != mods.shift {
                    // Switching fine mode re-anchors, so the handle never jumps.
                    state.fine = mods.shift;
                    state.relative = true;
                    state.anchor_x = pos.x;
                    state.anchor_value = *self.p.value;
                }
                let target = self.drag_target(&state, cols.track, pos.x, mods.ctrl);
                if self.set(target) || events.began() {
                    changed = true;
                    events.push(ParamEvent::Preview(*self.p.value));
                }
            }
            self.ctx.data_mut(|d| d.insert_temp(drag_id, state));
        }

        if track.drag_stopped()
            && let Some(state) = self.ctx.data_mut(|d| d.remove_temp::<DragState>(drag_id))
            && !state.cancelled
        {
            events.push(if (*self.p.value - state.start_value).abs() > f64::EPSILON {
                ParamEvent::Commit { from: state.start_value, to: *self.p.value, cause: CommitCause::Drag }
            } else {
                // Dragged and came back: nothing to commit, so close the transaction.
                ParamEvent::Rollback(state.start_value)
            });
        }
        changed
    }

    // ---- keyboard ----------------------------------------------------------

    fn keyboard(&mut self, track: &Response, events: &mut ParamEvents) -> bool {
        if !track.has_focus() {
            return false;
        }
        self.ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                track.id,
                egui::EventFilter { horizontal_arrows: true, ..egui::EventFilter::default() },
            );
        });
        let step = self.step();
        let span = self.max - self.min;
        let before = *self.p.value;
        let act = self.ctx.input_mut(|i| {
            let shift = i.modifiers.shift;
            let unit = if shift { 10.0 * step } else { step };
            // The shown shortcut (Command: ⌘ on macOS, Ctrl elsewhere); a host that reports only a bare Ctrl is taken too.
            if i.consume_key(Modifiers::COMMAND, Key::Backspace) || i.consume_key(Modifiers::CTRL, Key::Backspace) {
                Some(Act::Reset)
            } else if self.p.op_call.is_some() && i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::C) {
                Some(Act::CopyOpCall)
            } else if i.events.iter().any(|e| matches!(e, egui::Event::Copy)) {
                Some(Act::Copy)
            } else if let Some(text) = i.events.iter().find_map(|e| match e {
                egui::Event::Paste(text) => Some(text.clone()),
                _ => None,
            }) {
                Some(Act::Paste(text))
            } else if i.key_pressed(Key::ArrowRight) {
                Some(Act::Move(unit))
            } else if i.key_pressed(Key::ArrowLeft) {
                Some(Act::Move(-unit))
            } else if i.key_pressed(Key::PageUp) {
                Some(Act::Move(0.1 * span))
            } else if i.key_pressed(Key::PageDown) {
                Some(Act::Move(-0.1 * span))
            } else if i.key_pressed(Key::Home) {
                Some(Act::Absolute(self.min))
            } else if i.key_pressed(Key::End) {
                Some(Act::Absolute(self.max))
            } else if i.key_pressed(Key::Enter) || i.key_pressed(Key::F2) {
                Some(Act::Edit)
            } else {
                None
            }
        });
        let changed = match act {
            Some(Act::Move(delta)) => self.set(math::tidy(before + delta, self.decimals.max(3))),
            Some(Act::Absolute(v)) => self.set(v),
            Some(Act::Reset) => return self.reset(events),
            Some(Act::Edit) => {
                self.start_editing();
                false
            }
            Some(Act::Copy) => {
                self.copy_value();
                false
            }
            Some(Act::CopyOpCall) => {
                self.copy_op_call();
                false
            }
            Some(Act::Paste(text)) => return self.paste(&text, events),
            None => false,
        };
        if changed {
            events.push(ParamEvent::Commit { from: before, to: *self.p.value, cause: CommitCause::Keyboard });
        }
        changed
    }

    /// What a screen reader asks for: Increment and Decrement behave like arrow presses and `SetValue` like a typed
    /// entry. They act whether or not the slider has focus, because assistive technology addresses a node directly.
    fn assistive(&mut self, track: &Response, events: &mut ParamEvents) -> bool {
        let (steps, set) = self.ctx.input(|i| {
            let count = |action| i.num_accesskit_action_requests(track.id, action) as f64;
            let set = i.accesskit_action_requests(track.id, accesskit::Action::SetValue).last().and_then(|request| {
                match &request.data {
                    Some(accesskit::ActionData::NumericValue(v)) => Some(*v),
                    Some(accesskit::ActionData::Value(text)) => expr::parse(text, self.p.unit, self.min, self.max).ok(),
                    _ => None,
                }
            });
            (count(accesskit::Action::Increment) - count(accesskit::Action::Decrement), set)
        });
        let before = *self.p.value;
        let (changed, cause) = if let Some(value) = set.filter(|v| v.is_finite()) {
            (self.set(value), CommitCause::Typed)
        } else if steps != 0.0 {
            (self.set(math::tidy(before + steps * self.step(), self.decimals.max(3))), CommitCause::Keyboard)
        } else {
            return false;
        };
        if changed {
            events.push(ParamEvent::Commit { from: before, to: *self.p.value, cause });
        }
        changed
    }

    fn reset(&mut self, events: &mut ParamEvents) -> bool {
        let Some(default) = self.p.default else { return false };
        let before = *self.p.value;
        let changed = self.set(default);
        if changed || self.p.mixed {
            events.push(ParamEvent::Commit { from: before, to: *self.p.value, cause: CommitCause::Reset });
        }
        changed
    }

    // ---- reset button ------------------------------------------------------

    fn reset_button(&self, ui: &mut Ui, cols: &Columns, modified: bool) -> Response {
        let t = &self.ds.theme;
        let visible = modified && self.p.locked.is_none();
        // Clickable but not focusable: the slider is one Tab stop and Ctrl+Backspace resets from the keyboard.
        let sense = if visible && self.enabled { Sense::CLICK } else { Sense::hover() };
        let r = ui.interact(cols.reset, self.id.with("reset"), sense);
        let painter = ui.painter();
        let centre = cols.reset.center();
        if let Some(owner) = self.p.locked {
            paint::icon(painter, &self.ds, centre, icons::LOCK, 14.0, t.text2, true);
            let reason = self.ds.text_with(Msg::ParamHeldBy, &[("owner", owner)]);
            return r.tip(Tip::new(&self.ds.text(Msg::ParamLocked)).detail(&reason));
        } else if visible {
            let hover = paint::fade(&self.ctx, &self.ds, self.id.with("reset"), self.enabled && paint::hot(&r));
            let box_rect = paint::square(centre, 22.0);
            if hover > 0.0 {
                painter.rect_filled(box_rect, radius::CONTROL, t.bg3.gamma_multiply(hover));
            }
            let ink = paint::lerp(t.text3, t.text, hover);
            paint::icon(
                painter,
                &self.ds,
                centre,
                icons::ARROW_COUNTER_CLOCKWISE,
                14.0,
                paint::dim(self.looks_enabled, ink),
                false,
            );
            paint::focus_ring(painter, t, &r, box_rect, radius::CONTROL, RingPlacement::Outside);
        }
        if visible {
            a11y::describe(
                &r,
                || {
                    WidgetInfo::labeled(
                        WidgetType::Button,
                        self.enabled,
                        self.ds.text_with(Msg::ParamReset, &[("label", self.p.label)]),
                    )
                },
                |_| {},
            );
            // Building the tip's text allocates, so it is only done while the pointer is over the button.
            if r.hovered() {
                let default_text = NumberBuf::new(self.p.default.unwrap_or(0.0), self.decimals);
                let value = format!("{}{}", default_text.as_str(), self.p.unit);
                let tip_text = self.ds.text_with(Msg::ParamResetTo, &[("value", &value)]);
                return r.tip(Tip::new(&tip_text).shortcut(Shortcut::command(Key::Backspace)));
            }
        }
        r
    }

    // ---- painting ------------------------------------------------------------

    fn paint(&self, ui: &Ui, cols: &Columns, track: &Response, label_hot: bool, modified: bool) {
        let ds = &*self.ds;
        let t = &ds.theme;
        let painter = ui.painter();
        let enabled = self.looks_enabled;
        let cy = self.row.center().y;
        let hot = paint::hot(track) || label_hot;

        // Agent glow behind the whole row, fading over 900 ms (a static outline for 2 s under reduce motion).
        let mut agent_fill = 0.0;
        let mut agent_tick = false;
        if let Some(touch) = self.p.agent {
            let reduce = ds.settings.reduce_motion;
            let glow = if reduce {
                if touch.age_secs < 2.0 { 1.0 } else { 0.0 }
            } else {
                (1.0 - touch.age_secs / dur::AGENT_GLOW).clamp(0.0, 1.0)
            };
            if glow > 0.0 {
                agent_fill = glow;
                if reduce {
                    painter.rect_stroke(
                        self.row.shrink2(vec2(0.0, 1.0)),
                        radius::CONTROL,
                        Stroke::new(1.5, t.agent),
                        StrokeKind::Inside,
                    );
                    self.ctx.request_repaint_after_secs((2.0 - touch.age_secs).max(0.0));
                } else {
                    painter.rect_filled(
                        self.row.shrink2(vec2(0.0, 1.0)),
                        radius::CONTROL,
                        t.agent_glow.gamma_multiply(glow),
                    );
                    self.ctx.request_repaint();
                }
            } else {
                let seen_id = self.id.with("seen");
                let seen = self.ctx.data(|d| d.get_temp::<u64>(seen_id)) == Some(touch.stamp);
                if hot && !seen {
                    self.ctx.data_mut(|d| d.insert_temp(seen_id, touch.stamp));
                }
                agent_tick = !seen && !hot;
            }
        }

        // Modified dot.
        if modified && self.p.locked.is_none() {
            painter.circle_filled(pos2(cols.dot.left() + 4.0, cy), 3.0, paint::dim(enabled, t.accent));
        }

        // Label, with the variant badge at the end of the label column.
        let badge_w = if self.p.variant_base.is_some() { 16.0 } else { 0.0 };
        let label_rect = Rect::from_min_max(cols.label.min, pos2(cols.label.max.x - badge_w, cols.label.max.y));
        let label_ink = paint::ink(t, enabled, if hot { t.text } else { t.text2 });
        let drawn = paint::text_in(painter, label_rect, Align2::LEFT_CENTER, self.p.label, &ds.text.body, label_ink);
        if agent_tick {
            painter.rect_filled(
                Rect::from_min_size(pos2(drawn.left(), drawn.bottom() + 0.0), vec2(14.0, 2.0)),
                1,
                t.agent,
            );
        }
        if let Some(_base) = self.p.variant_base {
            let badge = Rect::from_center_size(pos2(cols.label.max.x - 8.0, cy), vec2(14.0, 14.0));
            painter.rect_filled(badge, 3, t.selection);
            painter.rect_stroke(badge, 3, Stroke::new(1.0, t.accent), StrokeKind::Inside);
            // `text` on the selection tint: `accent.text` there was 4.43:1 in light, just under the 4.5:1 for text.
            paint::text(
                painter,
                badge.center(),
                Align2::CENTER_CENTER,
                &ds.text(Msg::ParamVariantBadge),
                &ds.text.micro,
                t.text,
            );
        }

        self.paint_track(painter, cols.track, track, modified, agent_fill);
    }

    fn paint_track(&self, painter: &egui::Painter, area: Rect, response: &Response, modified: bool, agent_fill: f32) {
        let ds = &*self.ds;
        let t = &ds.theme;
        let enabled = self.looks_enabled;
        let cy = area.center().y;
        let (lo, hi) = Self::x_range(area);
        let rail = Rect::from_min_max(
            pos2(lo - 6.0, cy - size::TRACK_HEIGHT / 2.0),
            pos2(hi + 6.0, cy + size::TRACK_HEIGHT / 2.0),
        );
        painter.rect_filled(rail, 2, paint::dim(enabled, t.bg4));
        if t.control_stroke > 1.0 {
            painter.rect_stroke(rail, 2, Stroke::new(1.0, t.border_control), StrokeKind::Outside);
        }

        let value_x = if self.p.mixed {
            self.x_of(area, self.min + (self.max - self.min) / 2.0)
        } else {
            self.x_of(area, *self.p.value)
        };
        // The fill runs from the default; with no default it runs from the left end.
        let origin_x = self.p.default.map_or(lo - 6.0, |d| self.x_of(area, d));
        if !self.p.mixed {
            let fill =
                Rect::from_min_max(pos2(origin_x.min(value_x), rail.top()), pos2(origin_x.max(value_x), rail.bottom()));
            let base = if modified || self.p.default.is_none() { t.accent } else { t.border_control };
            let colour = paint::lerp(base, t.agent, agent_fill);
            painter.rect_filled(fill, 2, paint::dim(enabled, colour));
        }
        if let Some(d) = self.p.default {
            let x = self.x_of(area, d);
            painter
                .line_segment([pos2(x, cy - 5.0), pos2(x, cy + 5.0)], Stroke::new(1.5, paint::dim(enabled, t.text3)));
        }
        if let Some(driven) = self.p.driven {
            #[allow(clippy::cast_possible_truncation, reason = "a 0..1 fraction of an on-screen width")]
            let x = lo + (hi - lo) * (driven.clamp(0.0, 1.0) as f32);
            let meter = Rect::from_min_max(pos2(lo - 6.0, cy - 1.0), pos2(x, cy + 1.0));
            painter.rect_filled(meter, 1, t.text.gamma_multiply(0.8));
        }

        // Handle: a 14 pt disc inside a 2 pt ring. Hollow at rest, ringed in the accent once changed.
        let dragging = response.dragged();
        let hot = paint::hot(response);
        let grow = paint::fade(&self.ctx, ds, self.id.with("handle"), self.enabled && (hot || dragging));
        let radius = size::HANDLE / 2.0 + grow * 1.0;
        let ring = if dragging || modified { t.accent } else { paint::lerp(t.text2, t.text, grow) };
        painter.circle_filled(pos2(value_x, cy), radius, paint::dim(enabled, t.bg1));
        painter.circle_stroke(pos2(value_x, cy), radius - 1.0, Stroke::new(2.0, paint::dim(enabled, ring)));
        if self.p.mixed {
            painter.line_segment(
                [pos2(value_x - 3.0, cy), pos2(value_x + 3.0, cy)],
                Stroke::new(1.5, paint::dim(enabled, t.text2)),
            );
        }
        paint::focus_ring(
            painter,
            t,
            response,
            Rect::from_center_size(pos2(value_x, cy), vec2(size::HANDLE, size::HANDLE)),
            8,
            RingPlacement::Outside,
        );
    }

    // ---- numeric entry ---------------------------------------------------------

    fn edit_id(&self) -> Id {
        self.id.with("edit")
    }

    fn is_editing(&self) -> bool {
        self.ctx.data(|d| d.get_temp::<bool>(self.edit_id())).unwrap_or(false)
    }

    fn start_editing(&mut self) {
        let text =
            if self.p.mixed { String::new() } else { NumberBuf::new(*self.p.value, self.decimals).as_str().to_owned() };
        self.ctx.data_mut(|d| {
            d.insert_temp(self.edit_id(), true);
            d.insert_temp(self.id.with("buf"), Edit { buffer: text, error: None });
        });
        self.ctx.data_mut(|d| d.insert_temp(self.id.with("select_all"), true));
    }

    fn stop_editing(&self) {
        self.ctx.data_mut(|d| {
            d.insert_temp(self.edit_id(), false);
            d.remove::<Edit>(self.id.with("buf"));
        });
    }

    /// The numeric field: typing when `was_editing`, otherwise the resting number. `was_editing` is the state at the
    /// start of the frame: the Enter that started the typing is still in this frame's input and must not also finish it.
    fn numeric_field(
        &mut self,
        ui: &mut Ui,
        cols: &Columns,
        track: &Response,
        was_editing: bool,
        events: &mut ParamEvents,
    ) -> bool {
        let ds = std::sync::Arc::clone(&self.ds);
        let t = &ds.theme;
        let rect = paint::snap(&self.ctx, cols.field);
        let field_id = self.id.with("field");
        let mut changed = false;

        if self.is_editing() {
            let mut edit = self
                .ctx
                .data_mut(|d| d.remove_temp::<Edit>(self.id.with("buf")))
                .unwrap_or(Edit { buffer: String::new(), error: None });
            let select_all = self.ctx.data_mut(|d| d.remove_temp::<bool>(self.id.with("select_all"))).unwrap_or(false);
            // Enter and Esc belong to the IME while it composes; the preedit is drawn beside the buffer, never in it.
            let ime = ime::begin(&self.ctx, field_id, &edit.buffer);
            // The field is a part of the row, which was allocated already: a child `Ui`, not a scope that would pull
            // the cursor up to the field's bottom edge and let the next row overlap this one.
            let field = {
                let ui = &mut ids::editor_ui(ui, rect);
                ui.visuals_mut().selection.stroke =
                    Stroke::new(2.0, if edit.error.is_some() { t.danger } else { t.accent });
                let mut composing_layout = ime.layouter(ds.text.mono.clone(), t.text, Align::RIGHT);
                let mut field_edit = TextEdit::singleline(&mut edit.buffer)
                    .id(field_id)
                    .font(ds.text.mono.clone())
                    .text_color(t.text)
                    .horizontal_align(Align::RIGHT)
                    .vertical_align(Align::Center)
                    .margin(egui::Margin::symmetric(6, 2))
                    .desired_width(rect.width())
                    .min_size(rect.size())
                    // Esc is ours (it cancels the edit, unless the IME is composing): egui must not surrender focus on it.
                    // The arrows move the caret while typing, never the keyboard to another widget.
                    .event_filter(egui::EventFilter {
                        escape: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..egui::EventFilter::default()
                    });
                if ime.composing() {
                    field_edit = field_edit.layouter(&mut composing_layout);
                }
                let output = field_edit.show(ui);
                if select_all {
                    output.response.request_focus();
                    if let Some(mut state) = TextEdit::load_state(ui.ctx(), field_id) {
                        let end = edit.buffer.chars().count();
                        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(end),
                        )));
                        TextEdit::store_state(ui.ctx(), field_id, state);
                    }
                }
                output.response
            };
            ime.finish(&self.ctx, field_id, &edit.buffer);
            let name = ds.text_with(Msg::ParamValue, &[("label", self.p.label)]);
            self.ctx.accesskit_node_builder(field.id, |node| node.set_label(name));
            let escape = self.ctx.input(|i| i.key_pressed(Key::Escape));
            let enter = was_editing && self.ctx.input(|i| i.key_pressed(Key::Enter));
            if escape {
                self.stop_editing();
                track.request_focus();
            } else if field.lost_focus() || enter {
                match expr::parse(&edit.buffer, self.p.unit, self.min, self.max) {
                    Ok(v) => {
                        let before = *self.p.value;
                        changed = self.set(v);
                        if changed || self.p.mixed {
                            events.push(ParamEvent::Commit {
                                from: before,
                                to: *self.p.value,
                                cause: CommitCause::Typed,
                            });
                        }
                        self.stop_editing();
                        if enter {
                            track.request_focus();
                        }
                    }
                    Err(ParseError::Empty) if self.p.mixed => {
                        self.stop_editing();
                    }
                    Err(e) => {
                        // Keep the field open with the message; nothing shakes (7.1).
                        edit.error = Some(e);
                        field.request_focus();
                    }
                }
            }
            if self.is_editing() {
                if let Some(error) = edit.error {
                    self.error_bubble(&ds, rect, error);
                }
                self.ctx.data_mut(|d| d.insert_temp(self.id.with("buf"), edit));
            }
            return changed;
        }

        // Resting field: a click starts typing. It takes the auto id the editor's child `Ui` takes, so the widgets after
        // this row are called the same whether or not the number is being typed.
        ids::skip_editor_id(ui);
        if self.resting_field(ui, &ds, rect, field_id).clicked() && self.enabled {
            self.start_editing();
        }
        false
    }

    /// The number and its unit in a bordered box, as drawn when not being edited.
    fn resting_field(&self, ui: &mut Ui, ds: &Ds, rect: Rect, field_id: Id) -> Response {
        let t = &ds.theme;
        // Clickable but not focusable: Enter or F2 on the track starts typing, so the slider stays one Tab stop.
        let sense = if self.enabled { Sense::CLICK } else { Sense::hover() };
        let r = ui.interact(rect, field_id, sense);
        let hover = paint::fade(&self.ctx, ds, field_id, self.enabled && paint::hot(&r));
        let painter = ui.painter();
        painter.rect_filled(rect, radius::CONTROL, paint::dim(self.looks_enabled, t.bg2));
        let border = paint::lerp(t.border_control, t.text3, hover);
        painter.rect_stroke(
            rect,
            radius::CONTROL,
            Stroke::new(t.control_stroke, paint::dim(self.looks_enabled, border)),
            StrokeKind::Inside,
        );
        let ink = paint::ink(
            t,
            self.looks_enabled,
            if self.is_modified() || self.p.default.is_none() { t.text } else { t.text2 },
        );
        let inner = rect.shrink2(vec2(space::S6, 0.0));
        if self.p.mixed {
            paint::text_in(painter, inner, Align2::RIGHT_CENTER, "—", &ds.text.mono, ink);
        } else {
            let number = NumberBuf::new(*self.p.value, self.decimals);
            let unit_w = if self.p.unit.is_empty() {
                0.0
            } else {
                paint::text_width(&self.ctx, self.p.unit, &ds.text.small) + 1.0
            };
            let number_rect = Rect::from_min_max(inner.min, pos2(inner.max.x - unit_w, inner.max.y));
            paint::text_in(painter, number_rect, Align2::RIGHT_CENTER, number.as_str(), &ds.text.mono, ink);
            if unit_w > 0.0 {
                paint::text(
                    painter,
                    pos2(inner.max.x, inner.center().y + 0.5),
                    Align2::RIGHT_CENTER,
                    self.p.unit,
                    &ds.text.small,
                    paint::ink(t, self.looks_enabled, t.text3),
                );
            }
        }
        paint::focus_ring(painter, t, &r, rect, radius::CONTROL, RingPlacement::Outside);
        a11y::describe(
            &r,
            || {
                WidgetInfo::labeled(
                    WidgetType::Button,
                    self.enabled,
                    ds.text_with(Msg::ParamEdit, &[("label", self.p.label)]),
                )
            },
            |_| {},
        );
        r
    }

    fn error_bubble(&self, ds: &Ds, field: Rect, error: ParseError) {
        let t = &ds.theme;
        let message = match error {
            ParseError::Empty | ParseError::NotANumber => ds.text_with(
                Msg::ParamRangeError,
                &[
                    ("min", NumberBuf::new(self.min, self.decimals).as_str()),
                    ("max", NumberBuf::new(self.max, self.decimals).as_str()),
                ],
            ),
            ParseError::DivideByZero => ds.text(Msg::ParamDivideByZero).into_owned(),
        };
        Area::new(self.id.with("error"))
            .order(Order::Foreground)
            .fixed_pos(pos2(field.right(), field.bottom() + 4.0))
            .pivot(Align2::RIGHT_TOP)
            .show(&self.ctx, |ui| {
                Frame::popup(ui.style()).stroke(Stroke::new(1.0, t.danger)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(icons::WARNING).font(ds.text.icon(14.0, true)).color(t.danger));
                        ui.label(egui::RichText::new(message).font(ds.text.small.clone()).color(t.text));
                    });
                });
            });
    }

    // ---- menu and accessibility -----------------------------------------------------

    fn context_menu(&mut self, response: &Response, events: &mut ParamEvents, modified: bool) {
        if !self.enabled {
            return;
        }
        // Building the popup allocates (even `context_menu_opened` does), so it is only touched when it is opening or
        // was open last frame.
        let open_id = self.id.with("menu_open");
        let was_open = self.ctx.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
        if !response.secondary_clicked() && !was_open {
            return;
        }
        let ds = std::sync::Arc::clone(&self.ds);
        let label = |msg| ds.text(msg);
        let (reset_label, copy_label, paste_label, history_label) = (
            label(Msg::ParamResetDefault),
            label(Msg::ParamCopyValue),
            label(Msg::ParamPasteValue),
            label(Msg::ParamShowHistory),
        );
        let op_label = self.p.op_call.is_some().then(|| label(Msg::ParamCopyOpCall));
        let mut chosen = None;
        let shown = response
            .context_menu(|ui| {
                ui.set_min_width(200.0);
                let mut item = |ui: &mut Ui, enabled: bool, text: &str, what: MenuItem| {
                    if ui.add_enabled(enabled, egui::Button::new(text)).clicked() {
                        chosen = Some(what);
                        ui.close();
                    }
                };
                item(ui, modified, &reset_label, MenuItem::Reset);
                item(ui, true, &copy_label, MenuItem::Copy);
                item(ui, true, &paste_label, MenuItem::Paste);
                if let Some(text) = &op_label {
                    item(ui, true, text, MenuItem::CopyOpCall);
                }
                item(ui, true, &history_label, MenuItem::ShowInHistory);
            })
            .is_some();
        if shown != was_open {
            self.ctx.data_mut(|d| d.insert_temp(open_id, shown));
        }
        match chosen {
            Some(MenuItem::Reset) => {
                self.reset(events);
            }
            Some(MenuItem::Copy) => self.copy_value(),
            Some(MenuItem::Paste) => {
                // The clipboard answers a frame or two later, as `Event::Paste` (see `awaited_paste`).
                self.ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                let now = self.ctx.cumulative_frame_nr();
                self.ctx.data_mut(|d| d.insert_temp(self.id.with("paste_wait"), now));
            }
            Some(MenuItem::CopyOpCall) => self.copy_op_call(),
            Some(MenuItem::ShowInHistory) => events.push(ParamEvent::ShowInHistory),
            None => {}
        }
    }

    fn copy_value(&self) {
        self.ctx.copy_text(NumberBuf::new(*self.p.value, self.decimals).as_str().to_owned());
    }

    fn copy_op_call(&self) {
        if let Some(call) = &self.p.op_call {
            self.ctx.copy_text(call.json(*self.p.value));
        }
    }

    /// Sets the value from pasted text: a number, with a unit, a percent or simple arithmetic, read like typed text.
    /// Text that is not a number is ignored, since the clipboard can hold anything.
    fn paste(&mut self, text: &str, events: &mut ParamEvents) -> bool {
        let Ok(value) = expr::parse(text.trim(), self.p.unit, self.min, self.max) else { return false };
        let before = *self.p.value;
        let changed = self.set(value);
        if changed {
            events.push(ParamEvent::Commit { from: before, to: *self.p.value, cause: CommitCause::Paste });
        }
        changed
    }

    /// The clipboard's answer to "Paste value" from the menu. A focused track takes its own Ctrl+V; this is for the
    /// menu, where the pointer, not the keyboard, asked.
    fn awaited_paste(&mut self, events: &mut ParamEvents) -> bool {
        let key = self.id.with("paste_wait");
        let Some(asked_in) = self.ctx.data(|d| d.get_temp::<u64>(key)) else { return false };
        let text = self.ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Paste(text) => Some(text.clone()),
                _ => None,
            })
        });
        if text.is_some() || self.ctx.cumulative_frame_nr() > asked_in + PASTE_WAIT_FRAMES {
            self.ctx.data_mut(|d| d.remove::<u64>(key));
        }
        text.is_some_and(|text| self.paste(&text, events))
    }

    fn accessibility(&self, response: &Response, modified: bool) {
        let value = *self.p.value;
        let enabled = self.enabled;
        let decimals = self.decimals;
        let range = (self.min, self.max);
        let step = self.step();
        let ds = &*self.ds;
        a11y::describe(
            response,
            || WidgetInfo::slider(enabled, value, self.p.label),
            |node| {
                node.set_role(accesskit::Role::Slider);
                node.set_min_numeric_value(range.0);
                node.set_max_numeric_value(range.1);
                node.set_numeric_value_step(step);
                node.add_action(accesskit::Action::SetValue);
                node.add_action(accesskit::Action::Increment);
                node.add_action(accesskit::Action::Decrement);
                // "0.62°, default 0.50, modified": the parts a person would want read, in the table's list style.
                let mut parts: Vec<String> = Vec::with_capacity(4);
                if self.p.mixed {
                    parts.push(ds.text(Msg::ParamMixed).into_owned());
                } else {
                    parts.push(format!("{}{}", NumberBuf::new(value, decimals).as_str(), self.p.unit));
                    if let Some(d) = self.p.default {
                        parts.push(ds.text_with(Msg::ParamDefault, &[("value", NumberBuf::new(d, decimals).as_str())]));
                        parts.push(
                            ds.text(if modified { Msg::ParamModified } else { Msg::ParamAtDefault }).into_owned(),
                        );
                    }
                }
                if let Some(owner) = self.p.locked {
                    parts.push(ds.text_with(Msg::ParamLockedBy, &[("owner", owner)]));
                }
                node.set_value(parts.join(&ds.text(Msg::ListSeparator)));
                if let Some(reason) = self.p.disabled {
                    node.set_description(reason);
                }
            },
        );
    }
}

register_contrast!("param_slider", pairs);

/// The variant badge, and the fill, handle ring and tick on the rail.
fn pairs(table: &mut Table<'_>) {
    let t: Theme = *table.theme();
    table.add("variant badge letter on its tint", t.text, t.selection, Rule::Text);
    table.add("variant badge outline on its tint", t.accent, t.selection, Rule::NonText);
    table.add("modified fill on the rail", t.accent, t.bg4, Rule::NonText);
    table.add("default tick on the rail", t.text3, t.bg4, Rule::NonText);
    table.add("handle ring (modified) on the handle", t.accent, t.bg1, Rule::NonText);
    table.add("handle ring (at rest) on the handle", t.text2, t.bg1, Rule::NonText);
    table.add("agent fill on the rail", t.agent, t.bg4, Rule::NonText);
}

#[cfg(test)]
mod tests;
