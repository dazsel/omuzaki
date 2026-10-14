//! `FormRow` (`design-system.md` 7.0): a visible label in the 38% label column, then the control.
//!
//! `ParamSlider` and `Toggle` draw their own label; `TextField`, `Segmented`, `Dropdown` and `ColourField` take theirs
//! only as an accessible name. A form built from them (the inspector's generated controls, the command palette's
//! argument form) wraps each in a `FormRow`, so every row has the same label column as a slider row, whatever
//! control follows.
//!
//! The label is not a node of its own, as in a slider row: the control carries the name, and the row hands the
//! label text to the control (`|ui, label| ui.add(TextField::new(label, text))`), so the name a screen reader says
//! is always the text a person sees. A label wider than its column ends in an ellipsis with the whole text in a
//! tooltip, and the column gives way first when the row is narrow, like a slider's.

use egui::{Align, Align2, InnerResponse, Layout, Rect, RichText, Sense, Ui, UiBuilder, pos2, vec2};

use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::paint;
use crate::ds::theme::ds;
use crate::ds::tokens::{size, space};

/// The gutter at the row's start, where a slider keeps its "modified" dot.
const GUTTER: f32 = 12.0;
/// Room the control keeps when the row is narrow: the label column gives way before the control does.
const CONTROL_MIN: f32 = 80.0;
/// Narrowest the label column gets.
const LABEL_MIN: f32 = 48.0;

/// A label and its control.
#[must_use = "a form row does nothing until it is shown"]
pub struct FormRow<'a> {
    label: &'a str,
    help: Option<&'a str>,
    modified: bool,
}

impl<'a> FormRow<'a> {
    /// A row labelled `label`.
    pub fn new(label: &'a str) -> Self {
        a11y::require_label(label);
        Self { label, help: None, modified: false }
    }

    /// A line of quiet help under the control ("Used when no face is found").
    pub const fn help(mut self, help: &'a str) -> Self {
        self.help = Some(help);
        self
    }

    /// Shows the accent "modified" dot, as a slider does when its value differs from the default.
    pub const fn modified(mut self, modified: bool) -> Self {
        self.modified = modified;
        self
    }

    /// Shows the row. `control` draws the control and is given the label text, to use as the control's accessible
    /// name; what it returns is handed back.
    pub fn show<R>(self, ui: &mut Ui, control: impl FnOnce(&mut Ui, &str) -> R) -> InnerResponse<R> {
        let ds = ds(ui.ctx());
        let (t, m) = (&ds.theme, &ds.metrics);
        let full = ui.available_rect_before_wrap();
        let label_w = label_width(full.width());
        let id = ui.next_auto_id();

        // The label cell. Placed by hand, not with `horizontal_top` and `allocate_ui_with_layout`, which box their
        // closures: a form is drawn every frame.
        let cell = Rect::from_min_size(full.min, vec2(GUTTER + label_w, m.control_row));
        if ui.is_rect_visible(cell) {
            let painter = ui.painter();
            if self.modified {
                painter.circle_filled(pos2(cell.left() + 4.0, cell.center().y), 3.0, t.accent);
            }
            let area = Rect::from_min_max(pos2(cell.left() + GUTTER, cell.top()), cell.max);
            let drawn = paint::text_in(painter, area, Align2::LEFT_CENTER, self.label, &ds.text.body, t.text2);
            if paint::text_width(ui.ctx(), self.label, &ds.text.body) > drawn.width() + 0.5 {
                let _ = ui.interact(cell, id.with("label"), Sense::hover()).tip(Tip::new(self.label));
            }
        }

        let at = pos2(cell.right() + space::S6, full.top());
        let builder = UiBuilder::new().max_rect(Rect::from_min_max(at, full.max)).layout(Layout::top_down(Align::Min));
        let mut cell_ui = ui.new_child(builder);
        let inner = control(&mut cell_ui, self.label);
        if let Some(help) = self.help {
            cell_ui.label(RichText::new(help).font(ds.text.small.clone()).color(t.text3));
        }
        let bottom = cell_ui.min_rect().bottom().max(cell.bottom());
        let row = Rect::from_min_max(full.min, pos2(full.right(), bottom));
        InnerResponse { inner, response: ui.allocate_rect(row, Sense::hover()) }
    }
}

/// The label column of a row `width` wide: 38% of what is left of the gutter, 96 to 160 pt, giving way when narrow.
fn label_width(width: f32) -> f32 {
    let usable = width - GUTTER;
    let wanted = (usable * size::LABEL_COLUMN).clamp(size::LABEL_COLUMN_MIN, size::LABEL_COLUMN_MAX);
    wanted.min((usable - CONTROL_MIN - space::S6).max(LABEL_MIN))
}
