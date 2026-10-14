//! Toggle: the switch and the checkbox (`design-system.md` section 7.2).
//!
//! A switch (34 x 18) is for settings that apply at once; a checkbox (16) is for lists and forms that need Apply, and
//! for tri-state parents such as the visibility of a group. State is carried by position and by a check mark inside
//! the knob, never by colour alone (P9). The whole label is the hit area.

use egui::emath::easing;
use egui::{
    Align2, Id, Response, Sense, Stroke, StrokeKind, Ui, Widget, WidgetInfo, WidgetType, accesskit, pos2, vec2,
};

use super::ids;
use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::paint::{self, RingPlacement};
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{dur, mix, radius, size, space};
use crate::register_contrast;

/// The three states of a parent checkbox.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TriState {
    /// Nothing below is on.
    #[default]
    Off,
    /// Everything below is on.
    On,
    /// Some of what is below is on.
    Mixed,
}

impl TriState {
    /// The state after a click: mixed and off turn on, on turns off.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::On => Self::Off,
            Self::Off | Self::Mixed => Self::On,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Shape {
    Switch,
    Checkbox,
}

enum Value<'a> {
    Bool(&'a mut bool),
    Tri(&'a mut TriState),
}

impl Value<'_> {
    fn get(&self) -> TriState {
        match self {
            Self::Bool(b) if **b => TriState::On,
            Self::Bool(_) => TriState::Off,
            Self::Tri(t) => **t,
        }
    }

    fn click(&mut self) {
        match self {
            Self::Bool(b) => **b = !**b,
            Self::Tri(t) => **t = t.toggled(),
        }
    }
}

/// A switch or a checkbox with its label.
#[must_use = "a toggle does nothing until it is added to a Ui"]
pub struct Toggle<'a> {
    label: &'a str,
    value: Value<'a>,
    shape: Shape,
    disabled: Option<&'a str>,
    hide_label: bool,
    salt: Option<Id>,
}

impl<'a> Toggle<'a> {
    /// A switch for a setting that applies at once.
    pub fn switch(label: &'a str, value: &'a mut bool) -> Self {
        a11y::require_label(label);
        Self { label, value: Value::Bool(value), shape: Shape::Switch, disabled: None, hide_label: false, salt: None }
    }

    /// A checkbox for a list or form.
    pub fn checkbox(label: &'a str, value: &'a mut bool) -> Self {
        a11y::require_label(label);
        Self { label, value: Value::Bool(value), shape: Shape::Checkbox, disabled: None, hide_label: false, salt: None }
    }

    /// A tri-state checkbox for a parent whose children are not all alike.
    pub fn tri(label: &'a str, value: &'a mut TriState) -> Self {
        a11y::require_label(label);
        Self { label, value: Value::Tri(value), shape: Shape::Checkbox, disabled: None, hide_label: false, salt: None }
    }

    /// Draws no visible label, for a row whose own label sits beside it; the accessible label stays.
    pub const fn label_hidden(mut self) -> Self {
        self.hide_label = true;
        self
    }

    /// Disables the toggle, saying why (shown as the accessible description).
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Keys the toggle's state (keyboard focus, the slide animation) by `salt` instead of by its position in the
    /// `Ui`, so reordering the rows of a list keeps the focus with its toggle. Use a value that names the thing the
    /// toggle sets, unique among the toggles of one `Ui`.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(Id::new(salt));
        self
    }
}

impl Widget for Toggle<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let ds = ds(ui.ctx());
        let (t, m) = (&ds.theme, &ds.metrics);
        let ctx = ui.ctx().clone();
        let enabled = ui.is_enabled() && self.disabled.is_none();
        // Only the toggle's own `disabled(reason)` draws the disabled look: a disabled `Ui` is faded by egui.
        let looks_enabled = self.disabled.is_none();
        let control = match self.shape {
            Shape::Switch => size::SWITCH,
            Shape::Checkbox => vec2(size::CHECKBOX, size::CHECKBOX),
        };
        // The label gets the room left beside the control; a longer one ends in an ellipsis and the tooltip says it all.
        let gap = if self.hide_label { 0.0 } else { space::S8 };
        let label_want = if self.hide_label { 0.0 } else { paint::text_width(&ctx, self.label, &ds.text.body) };
        let label_w = label_want.min((paint::fit_width(ui) - control.x - gap).max(MIN_LABEL));
        let truncated = label_want > label_w;
        let width = control.x + gap + label_w;
        let sense = if enabled { Sense::click() } else { Sense::focusable_noninteractive() };
        let (row, mut response) = ids::allocate(ui, vec2(width, m.control_row.max(control.y)), sense, KIND, self.salt);
        if response.clicked() && enabled {
            self.value.click();
            response.mark_changed();
        }

        let state = self.value.get();
        let on = state != TriState::Off;
        if ui.is_rect_visible(row) {
            let painter = ui.painter();
            let hover = paint::fade(&ctx, &ds, response.id.with("hover"), enabled && paint::hot(&response));
            let slide = ctx.animate_bool_with_time_and_easing(
                response.id.with("on"),
                on,
                ds.motion(dur::SECTION),
                easing::cubic_out,
            );
            let box_rect = paint::snap(
                &ctx,
                egui::Rect::from_min_size(pos2(row.left(), row.center().y - control.y / 2.0), control),
            );

            let look = Look { state, slide, hover, enabled: looks_enabled };
            self.draw_control(painter, &ds, &response, box_rect, look);
            if !self.hide_label {
                let text = paint::ink(t, looks_enabled, t.text);
                let area = egui::Rect::from_min_size(
                    pos2(row.left() + control.x + space::S8, row.top()),
                    vec2(label_w + 1.0, row.height()),
                );
                paint::text_in(painter, area, Align2::LEFT_CENTER, self.label, &ds.text.body, text);
            }
        }

        let is_switch = self.shape == Shape::Switch;
        a11y::describe(
            &response,
            || WidgetInfo::selected(WidgetType::Checkbox, enabled, on, self.label),
            |node| {
                if is_switch {
                    node.set_role(accesskit::Role::Switch);
                }
                if state == TriState::Mixed {
                    node.set_toggled(accesskit::Toggled::Mixed);
                }
                if let Some(reason) = self.disabled {
                    node.set_description(reason);
                }
            },
        );
        if truncated { response.tip(Tip::new(self.label)) } else { response }
    }
}

/// The least room a label keeps when the toggle is squeezed: an ellipsis and a letter.
const MIN_LABEL: f32 = 16.0;

/// What a toggle's salt is namespaced by (see [`ids`]).
const KIND: &str = "ds.toggle";

/// The animated state of a control for one frame.
#[derive(Clone, Copy)]
struct Look {
    state: TriState,
    /// 0 (off) to 1 (on), eased.
    slide: f32,
    /// 0 to 1 hover fade.
    hover: f32,
    enabled: bool,
}

impl Toggle<'_> {
    /// Draws the switch or the checkbox box (not the label) with its focus ring.
    fn draw_control(&self, painter: &egui::Painter, ds: &Ds, response: &Response, box_rect: egui::Rect, look: Look) {
        let t = &ds.theme;
        let Look { state, slide, hover, enabled } = look;
        let on = state != TriState::Off;
        let (track_off, track_on) = (t.bg4, t.accent);
        let mut fill = paint::lerp(track_off, track_on, slide);
        let mut border = paint::lerp(t.border_control, t.accent, slide);
        if hover > 0.0 && enabled {
            fill = paint::lerp(
                fill,
                if on { t.accent_hover } else { t.border_control },
                hover * if on { 1.0 } else { 0.18 },
            );
            border = paint::lerp(border, if on { t.accent_hover } else { t.text3 }, hover);
        }
        // The knob of an off switch brightens with the track when hovered, or it would sink into the lightened track.
        let ink_off = paint::lerp(t.text3, t.text, hover);
        let ink_on = t.on_accent;
        fill = paint::dim(enabled, fill);
        border = paint::dim(enabled, border);

        match self.shape {
            Shape::Switch => {
                painter.rect_filled(box_rect, radius::FULL, fill);
                painter.rect_stroke(box_rect, radius::FULL, Stroke::new(t.control_stroke, border), StrokeKind::Inside);
                let knob_r = 6.0;
                let travel = box_rect.width() - 2.0 * (knob_r + 3.0);
                let knob = pos2(box_rect.left() + 3.0 + knob_r + travel * slide, box_rect.center().y);
                let knob_fill = paint::dim(enabled, paint::lerp(ink_off, ink_on, slide));
                painter.circle_filled(knob, knob_r, knob_fill);
                if slide > 0.05 {
                    let check = paint::lerp(knob_fill, paint::dim(enabled, t.accent), slide);
                    paint::check_mark(painter, paint::square(knob, knob_r * 1.5), check, 1.5);
                }
                paint::focus_ring(painter, t, response, box_rect, radius::FULL, RingPlacement::Outside);
            }
            Shape::Checkbox => {
                painter.rect_filled(box_rect, 4, fill);
                painter.rect_stroke(box_rect, 4, Stroke::new(t.control_stroke, border), StrokeKind::Inside);
                let mark = paint::dim(enabled, ink_on);
                if state == TriState::Mixed {
                    let c = box_rect.center();
                    painter.line_segment([pos2(c.x - 4.0, c.y), pos2(c.x + 4.0, c.y)], Stroke::new(2.0, mark));
                } else if slide > 0.05 {
                    paint::check_mark(painter, box_rect.shrink(2.0), mark.gamma_multiply(slide), 2.0);
                }
                paint::focus_ring(painter, t, response, box_rect, 4, RingPlacement::Outside);
            }
        }
    }
}

register_contrast!("toggle", pairs);

/// The knob and the check mark on their track, at rest and hovered: shapes that carry the state (P9).
fn pairs(table: &mut Table<'_>) {
    let t = *table.theme();
    let hovered_off = mix(t.border_control, t.bg4, 18);
    table.add("switch knob (off) on its track", t.text3, t.bg4, Rule::NonText);
    table.add("switch knob (off) on its hovered track", t.text, hovered_off, Rule::NonText);
    table.add("switch knob (on) on its track", t.on_accent, t.accent, Rule::NonText);
    table.add("switch knob (on) on its hovered track", t.on_accent, t.accent_hover, Rule::NonText);
    table.add("check mark on the knob", t.accent, t.on_accent, Rule::NonText);
    table.add("checkbox mark on its box", t.on_accent, t.accent, Rule::NonText);
    table.add("checkbox mark on its hovered box", t.on_accent, t.accent_hover, Rule::NonText);
}
