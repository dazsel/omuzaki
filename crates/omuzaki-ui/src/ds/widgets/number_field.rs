//! `NumberField` (`design-system.md` 7.1, 7.5): a mono, right-aligned number in a bordered box that you type into.
//!
//! The readout of a curve point (input, output, tangent) and any place a value stands alone, without a track. It is
//! the number half of a [`ParamSlider`](super::ParamSlider) as its own control: a click, Enter or F2 starts typing
//! (everything selected); Enter or leaving the field commits; Esc puts the old value back; the text is read like a
//! slider's (a unit, a percent, simple arithmetic, `expr::parse`) and bad text keeps the field open with the message
//! in its description and its tooltip, nothing shaking.
//!
//! One Tab stop. Up and Down nudge by the step (Shift ×10), Page Up and Page Down move ten steps, Ctrl+Backspace
//! returns to the default if one was given. AccessKit sees a `SpinButton` with its range and step that answers
//! `SetValue`, `Increment` and `Decrement` focused or not. At rest the number is formatted into a stack buffer and
//! drawn from the text cache, so a field that is not being edited allocates nothing.

use std::ops::RangeInclusive;

use egui::{
    Align, Align2, EventFilter, Frame, Id, Key, Modifiers, Rect, Response, Sense, Stroke, StrokeKind, TextEdit, Ui,
    UiBuilder, Widget, WidgetInfo, WidgetType, accesskit, vec2,
};

use super::ime;
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::expr::{self, ParseError};
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::text::{NumberBuf, decimals_for_range};
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Theme, radius, size, space};
use crate::register_contrast;

/// What the field remembers while it is being typed into.
#[derive(Clone, Default)]
struct Edit {
    text: String,
    error: Option<ParseError>,
}

/// A number you can type, nudge and reset.
#[must_use = "a number field does nothing until it is shown"]
pub struct NumberField<'a> {
    label: &'a str,
    value: &'a mut f64,
    range: RangeInclusive<f64>,
    decimals: Option<usize>,
    step: Option<f64>,
    unit: &'a str,
    width: f32,
    default: Option<f64>,
    modified: bool,
    disabled: Option<&'a str>,
    salt: Option<Id>,
}

impl<'a> NumberField<'a> {
    /// A field named `label` (the accessible name) editing `value` within `range`.
    pub fn new(label: &'a str, value: &'a mut f64, range: RangeInclusive<f64>) -> Self {
        a11y::require_label(label);
        debug_assert!(range.start() < range.end(), "a number field needs a non-empty range");
        Self {
            label,
            value,
            range,
            decimals: None,
            step: None,
            unit: "",
            width: size::NUMERIC_FIELD.x,
            default: None,
            modified: false,
            disabled: None,
            salt: None,
        }
    }

    /// Overrides the decimals shown.
    pub const fn decimals(mut self, decimals: usize) -> Self {
        self.decimals = Some(decimals);
        self
    }

    /// The step of Up and Down.
    pub const fn step(mut self, step: f64) -> Self {
        self.step = Some(step);
        self
    }

    /// A unit that is shown after the number and may be typed with it.
    pub const fn unit(mut self, unit: &'a str) -> Self {
        self.unit = unit;
        self
    }

    /// The field's width; 56 pt by default.
    pub const fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// The value Ctrl+Backspace returns to. Without one, Ctrl+Backspace does nothing.
    pub const fn default_value(mut self, default: f64) -> Self {
        self.default = Some(default);
        self
    }

    /// Marks the value as set by hand rather than derived: an accent dot at the field's corner.
    pub const fn modified(mut self, modified: bool) -> Self {
        self.modified = modified;
        self
    }

    /// Disables the field, saying why.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Keys the field by `salt` instead of by position.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(Id::new(salt));
        self
    }

    /// Shows the field and reports whether the value changed and whether it was reset.
    pub fn show(self, ui: &mut Ui) -> NumberOutput {
        Show::new(self, ui).run(ui, None)
    }

    /// Shows the field in `rect` as the widget `id`, wherever the layout's cursor is: for a composite that places its
    /// own parts (the curve editor's readout). The caller makes `id` unique and stable.
    pub fn show_at(self, ui: &mut Ui, id: Id, rect: Rect) -> NumberOutput {
        Show::new(self, ui).run(ui, Some((id, rect)))
    }

    /// Starts typing in the field shown with `id` on its next frame (the curve editor's Enter on a point). Takes the
    /// keyboard focus there.
    pub fn request_edit(ctx: &egui::Context, id: Id) {
        ctx.data_mut(|d| d.insert_temp(id.with("start"), true));
        ctx.memory_mut(|m| m.request_focus(id));
    }
}

impl Widget for NumberField<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui).response
    }
}

/// What a [`NumberField`] reports.
#[derive(Debug)]
pub struct NumberOutput {
    /// The field; `changed()` when the value changed.
    pub response: Response,
    /// Ctrl+Backspace returned it to its default this frame.
    pub reset: bool,
}

struct Show<'a> {
    f: NumberField<'a>,
    ds: std::sync::Arc<Ds>,
    enabled: bool,
    min: f64,
    max: f64,
    decimals: usize,
}

impl<'a> Show<'a> {
    fn new(f: NumberField<'a>, ui: &Ui) -> Self {
        let (min, max) = (*f.range.start(), *f.range.end());
        let decimals = f.decimals.unwrap_or_else(|| decimals_for_range(min, max));
        Self { enabled: ui.is_enabled() && f.disabled.is_none(), ds: ds(ui.ctx()), f, min, max, decimals }
    }

    fn step(&self) -> f64 {
        self.f.step.unwrap_or_else(|| 10f64.powi(-i32::try_from(self.decimals).unwrap_or(2)))
    }

    fn set(&mut self, value: f64) -> bool {
        let value = value.clamp(self.min, self.max);
        let changed = (value - *self.f.value).abs() > f64::EPSILON;
        *self.f.value = value;
        changed
    }

    fn run(mut self, ui: &mut Ui, at: Option<(Id, Rect)>) -> NumberOutput {
        let ctx = ui.ctx().clone();
        let (rect, id) = if let Some((id, rect)) = at {
            (rect, id)
        } else {
            let (rect, outer) = ui.allocate_exact_size(vec2(self.f.width, size::NUMERIC_FIELD.y), Sense::hover());
            (rect, self.f.salt.map_or(outer.id.with("number"), |salt| ui.make_persistent_id(salt)))
        };
        let sense = if self.enabled { Sense::click() } else { Sense::focusable_noninteractive() };
        let mut response = ui.interact(rect, id, sense);
        let rect = paint::snap(&ctx, rect);
        let edit_id = id.with("edit");
        let mut edit: Option<Edit> = ctx.data(|d| d.get_temp(edit_id));
        // Typing that began this frame: the Enter that started it is still in this frame's input and must not end it.
        let fresh = edit.is_none();
        let mut changed = false;
        let mut reset = false;

        if self.enabled {
            changed |= self.assistive(&ctx, id);
            if edit.is_none() {
                let keys = self.keyboard(&ctx, &response);
                changed |= keys.changed;
                reset = keys.reset;
                let asked = ctx.data_mut(|d| d.remove_temp::<bool>(id.with("start"))).unwrap_or(false);
                if response.clicked() || keys.start || asked {
                    edit = Some(self.begin(&ctx, id));
                }
            }
        }
        let was_editing = edit.is_some();
        if let Some(mut open) = edit.take() {
            match self.typing(ui, &ctx, rect, id, &mut open, fresh) {
                Typing::Commit(value) => changed |= self.set(value),
                Typing::Cancel => {}
                Typing::Keep => edit = Some(open),
            }
            ctx.data_mut(|d| {
                if let Some(open) = &edit {
                    d.insert_temp(edit_id, open.clone());
                } else {
                    d.remove::<Edit>(edit_id);
                }
            });
            if let Some(error) = edit.as_ref().and_then(|e| e.error) {
                self.error_bubble(&ctx, rect, id, error);
            }
        } else {
            self.paint_resting(ui, rect, &response, id);
        }
        if was_editing && edit.is_none() {
            // Typing is over: the keyboard is back on the field.
            response.request_focus();
        }
        if changed {
            response.mark_changed();
        }
        self.accessibility(&response, edit.as_ref().and_then(|e| e.error));
        NumberOutput { response, reset }
    }

    /// Starts typing: the text is the number as shown, all of it selected when the field is typed into.
    fn begin(&self, ctx: &egui::Context, id: Id) -> Edit {
        let text = NumberBuf::new(*self.f.value, self.decimals).as_str().to_owned();
        ctx.data_mut(|d| d.insert_temp(id.with("select_all"), true));
        Edit { text, error: None }
    }

    /// The keys of the focused field at rest: nudge, page, reset, start typing.
    fn keyboard(&mut self, ctx: &egui::Context, response: &Response) -> Keys {
        let mut keys = Keys::default();
        if !response.has_focus() {
            return keys;
        }
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(response.id, EventFilter { vertical_arrows: true, ..EventFilter::default() });
        });
        let step = self.step();
        let act = ctx.input_mut(|i| {
            let unit = if i.modifiers.shift { 10.0 * step } else { step };
            if i.consume_key(Modifiers::CTRL, Key::Backspace) {
                Some(Act::Reset)
            } else if i.key_pressed(Key::ArrowUp) {
                Some(Act::Nudge(unit))
            } else if i.key_pressed(Key::ArrowDown) {
                Some(Act::Nudge(-unit))
            } else if i.key_pressed(Key::PageUp) {
                Some(Act::Nudge(10.0 * step))
            } else if i.key_pressed(Key::PageDown) {
                Some(Act::Nudge(-10.0 * step))
            } else if i.key_pressed(Key::Enter) || i.key_pressed(Key::F2) {
                Some(Act::Start)
            } else {
                None
            }
        });
        match act {
            Some(Act::Reset) => {
                if let Some(default) = self.f.default {
                    keys.changed = self.set(default);
                    keys.reset = true;
                }
            }
            Some(Act::Nudge(delta)) => {
                // Round away binary noise (0.1 + 0.2) so a nudged value prints and compares cleanly.
                let scale = 10f64.powi(i32::try_from(self.decimals.clamp(3, 12)).unwrap_or(3));
                keys.changed = self.set(((*self.f.value + delta) * scale).round() / scale);
            }
            Some(Act::Start) => keys.start = true,
            None => {}
        }
        keys
    }

    /// What a screen reader asks for, focused or not.
    fn assistive(&mut self, ctx: &egui::Context, id: Id) -> bool {
        let (steps, set) = ctx.input(|i| {
            let count = |action| i.num_accesskit_action_requests(id, action) as f64;
            let set = i.accesskit_action_requests(id, accesskit::Action::SetValue).last().and_then(|r| match &r.data {
                Some(accesskit::ActionData::NumericValue(v)) => Some(*v),
                Some(accesskit::ActionData::Value(text)) => expr::parse(text, self.f.unit, self.min, self.max).ok(),
                _ => None,
            });
            (count(accesskit::Action::Increment) - count(accesskit::Action::Decrement), set)
        });
        if let Some(value) = set.filter(|v| v.is_finite()) {
            self.set(value)
        } else if steps != 0.0 {
            self.set(*self.f.value + steps * self.step())
        } else {
            false
        }
    }

    /// The number and its unit in the bordered box, as drawn when nobody is typing.
    fn paint_resting(&self, ui: &Ui, rect: Rect, response: &Response, id: Id) {
        let ds = &*self.ds;
        let t = &ds.theme;
        let painter = ui.painter();
        let looks_enabled = self.f.disabled.is_none();
        let focused = response.has_focus();
        let hover = paint::fade(ui.ctx(), ds, id.with("hover"), self.enabled && paint::hot(response));
        painter.rect_filled(rect, radius::CONTROL, paint::dim(looks_enabled, t.bg2));
        // The focus border is the focus indicator, so it is never dimmed, even on a field that is disabled.
        let (width, border) = if focused {
            (2.0, t.accent)
        } else {
            (t.control_stroke, paint::dim(looks_enabled, paint::lerp(t.border_control, t.text3, hover * 0.6)))
        };
        painter.rect_stroke(rect, radius::CONTROL, Stroke::new(width, border), StrokeKind::Inside);
        let mut text = NumberBuf::new(*self.f.value, self.decimals);
        let area = Rect::from_min_max(
            egui::pos2(rect.left() + space::S6, rect.top()),
            egui::pos2(rect.right() - space::S6, rect.bottom()),
        );
        let ink = paint::ink(t, looks_enabled, if self.modified_or_hot(hover) { t.text } else { t.text2 });
        // The unit follows the number in the same string: it is a symbol, not a word.
        let _ = std::fmt::Write::write_str(&mut text, self.f.unit);
        paint::text_in(painter, area, Align2::RIGHT_CENTER, text.as_str(), &ds.text.mono, ink);
        if self.f.modified {
            painter.circle_filled(
                egui::pos2(rect.left() + 4.0, rect.top() + 4.0),
                2.5,
                paint::dim(looks_enabled, t.accent),
            );
        }
    }

    /// Value text is `text` once it differs from the default or the pointer is on it, `text.2` otherwise.
    fn modified_or_hot(&self, hover: f32) -> bool {
        self.f.modified || hover > 0.5
    }

    /// The text field while typing. Returns what the frame decided.
    fn typing(&self, ui: &mut Ui, ctx: &egui::Context, rect: Rect, id: Id, edit: &mut Edit, fresh: bool) -> Typing {
        let ds = &*self.ds;
        let t = &ds.theme;
        let text_id = id.with("text");
        let select_all = ctx.data_mut(|d| d.remove_temp::<bool>(id.with("select_all"))).unwrap_or(false);
        // Enter and Esc belong to the IME while it composes; the preedit is drawn beside the text, never in it.
        let ime = ime::begin(ctx, text_id, &edit.text);
        ui.painter().rect_filled(rect, radius::CONTROL, t.bg2);
        // A child `Ui` that does not move the parent's cursor, centred whatever the parent lays out.
        let child =
            &mut ui.new_child(UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(Align::Center)));
        let mut composing_layout = ime.layouter(ds.text.mono.clone(), t.text, Align::RIGHT);
        let mut field_edit = TextEdit::singleline(&mut edit.text)
            .id(text_id)
            .frame(Frame::NONE)
            .font(ds.text.mono.clone())
            .text_color(t.text)
            .horizontal_align(Align::RIGHT)
            .vertical_align(Align::Center)
            .margin(egui::Margin::symmetric(6, 0))
            .desired_width(rect.width())
            // Esc is ours: it puts the old value back, so the TextEdit must not surrender focus itself.
            .event_filter(EventFilter { escape: true, ..EventFilter::default() });
        if ime.composing() {
            field_edit = field_edit.layouter(&mut composing_layout);
        }
        let output = field_edit.show(child);
        ime.finish(ctx, text_id, &edit.text);
        let field = output.response.response;
        let border = if edit.error.is_some() { t.danger } else { t.accent };
        ui.painter().rect_stroke(rect, radius::CONTROL, Stroke::new(2.0, border), StrokeKind::Inside);
        if edit.error.is_some() {
            // The border is `danger` here, so thickening it would be the only sign of focus: add the ring.
            paint::draw_ring(ui.painter(), t, rect, radius::CONTROL, RingPlacement::Outside);
        }
        if select_all {
            field.request_focus();
            if let Some(mut state) = TextEdit::load_state(ctx, text_id) {
                let end = edit.text.chars().count();
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(end),
                )));
                TextEdit::store_state(ctx, text_id, state);
            }
        }
        ctx.accesskit_node_builder(text_id, |node| {
            node.set_label(ds.text_with(Msg::ParamValue, &[("label", self.f.label)]));
        });
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            return Typing::Cancel;
        }
        // A press, not its auto-repeat: Enter held to start typing must not also commit.
        let enter = !fresh
            && ctx.input(|i| {
                i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Key { key: Key::Enter, pressed: true, repeat: false, .. }))
            });
        // egui's `TextEdit` surrenders the focus on any Enter event, an auto-repeat included. Enter held to start typing
        // must not end it that way: a repeat that is not also a fresh press takes the focus back and changes nothing.
        let repeated = ctx.input(|i| {
            i.events.iter().any(|e| matches!(e, egui::Event::Key { key: Key::Enter, pressed: true, repeat: true, .. }))
        });
        if field.lost_focus() && repeated && !enter {
            field.request_focus();
            return Typing::Keep;
        }
        if (field.lost_focus() && !fresh) || enter {
            return match expr::parse(&edit.text, self.f.unit, self.min, self.max) {
                Ok(value) => Typing::Commit(value),
                Err(error) => {
                    // Keep the field open with the message; nothing shakes (7.1).
                    edit.error = Some(error);
                    field.request_focus();
                    Typing::Keep
                }
            };
        }
        Typing::Keep
    }

    /// The message under the field while its text is not a number.
    fn error_bubble(&self, ctx: &egui::Context, field: Rect, id: Id, error: ParseError) {
        let ds = &*self.ds;
        let t = &ds.theme;
        let message = self.error_text(Some(error));
        egui::Area::new(id.with("error"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(field.right(), field.bottom() + space::S4))
            .pivot(Align2::RIGHT_TOP)
            .show(ctx, |ui| {
                paint::overlay_frame(ds).stroke(Stroke::new(1.0, t.danger)).inner_margin(egui::Margin::same(8)).show(
                    ui,
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(crate::ds::icons::WARNING)
                                    .font(ds.text.icon(14.0, true))
                                    .color(t.danger),
                            );
                            ui.label(egui::RichText::new(message).font(ds.text.small.clone()).color(t.text));
                        });
                    },
                );
            });
    }

    /// "Enter a number from 0 to 1" for text that is not a number.
    fn error_text(&self, error: Option<ParseError>) -> String {
        let ds = &*self.ds;
        match error {
            Some(ParseError::DivideByZero) => ds.text(Msg::ParamDivideByZero).into_owned(),
            _ => ds.text_with(
                Msg::ParamRangeError,
                &[
                    ("min", NumberBuf::new(self.min, self.decimals).as_str()),
                    ("max", NumberBuf::new(self.max, self.decimals).as_str()),
                ],
            ),
        }
    }

    fn accessibility(&self, response: &Response, error: Option<ParseError>) {
        let (value, enabled) = (*self.f.value, self.enabled);
        let (min, max, step, decimals) = (self.min, self.max, self.step(), self.decimals);
        let reason = self.f.disabled;
        a11y::describe(
            response,
            || WidgetInfo::labeled(WidgetType::DragValue, enabled, self.f.label),
            |node| {
                node.set_role(accesskit::Role::SpinButton);
                node.set_numeric_value(value);
                node.set_min_numeric_value(min);
                node.set_max_numeric_value(max);
                node.set_numeric_value_step(step);
                node.set_value(format!("{}{}", NumberBuf::new(value, decimals).as_str(), self.f.unit));
                for action in [accesskit::Action::SetValue, accesskit::Action::Increment, accesskit::Action::Decrement]
                {
                    node.add_action(action);
                }
                if let Some(reason) = reason {
                    node.set_description(reason);
                } else if error.is_some() {
                    node.set_description(self.error_text(error));
                }
            },
        );
    }
}

/// What typing decided this frame.
enum Typing {
    Commit(f64),
    Cancel,
    Keep,
}

/// What a key asks of the focused field at rest.
enum Act {
    Reset,
    Nudge(f64),
    Start,
}

/// What the keys did this frame.
#[derive(Default)]
struct Keys {
    changed: bool,
    reset: bool,
    start: bool,
}

register_contrast!("number_field", pairs);

/// The number on the field and its border on the surfaces it sits on.
fn pairs(table: &mut Table<'_>) {
    let t: Theme = *table.theme();
    let surfaces = [("bg1", t.bg1), ("bg2", t.bg2)];
    table.add("number at rest", t.text2, t.bg2, Rule::Text);
    table.add("number, set by hand or hovered", t.text, t.bg2, Rule::Text);
    table.add("typed number", t.text, t.bg2, Rule::Text);
    table.add_on("focus border", t.accent, &surfaces, Rule::NonText);
    table.add_on("error border", t.danger, &surfaces, Rule::NonText);
    table.add("modified dot", t.accent, t.bg2, Rule::NonText);
}
