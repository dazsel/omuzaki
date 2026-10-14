//! `Listbox` (`design-system.md` 7.0): the list half of a combobox, shared by the dropdown and the command palette.
//!
//! In a combobox the control that owns the keyboard keeps focus (the dropdown button, the palette's text field) and
//! the list only shows which option is **active**: the one Enter would choose. That is the "active descendant" pattern,
//! so a screen reader follows the active row while focus never leaves the owner. This file therefore has three
//! pieces that work together:
//!
//! - [`Listbox`] draws the rows (fixed height, so a long list is virtualised), marks the active and the chosen one,
//!   and reports a click or hover;
//! - [`step`], [`first`] and [`last`] move the active row over the enabled ones, and [`pressed_move`] reads the
//!   navigation keys, so every combobox answers the same keys the same way;
//! - [`link_combobox`] tells AccessKit that the owner is a combobox controlling this list, expanded, with this
//!   active option.
//!
//! Rows are not Tab stops. Each is a named `ListBoxOption` node that answers `Click`, positioned in its set so
//! assistive technology can say "3 of 12" even though only the visible rows exist.

use egui::{
    Align, Align2, Context, Id, Key, Layout, Modifiers, Rect, Response, ScrollArea, Sense, Ui, UiBuilder, WidgetInfo,
    WidgetType, accesskit, pos2, vec2,
};

use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::icons;
use crate::ds::paint;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Theme, radius, space};
use crate::register_contrast;

/// What a row says. Every string is the caller's.
#[derive(Clone, Copy, Debug)]
#[must_use = "an item does nothing until it is put in a list"]
pub struct ListItem<'a> {
    /// The row's text, and its accessible name.
    pub label: &'a str,
    /// Quiet text at the right end: a summary, a key binding.
    pub detail: Option<&'a str>,
    /// An icon glyph before the label.
    pub icon: Option<&'a str>,
    /// Why the row can't be chosen (shown on hover and read as its description); `None` when it can.
    pub disabled: Option<&'a str>,
    /// Draw the label in mono: op names, ids.
    pub mono: bool,
}

impl<'a> ListItem<'a> {
    /// A row that says `label`.
    pub fn new(label: &'a str) -> Self {
        a11y::require_label(label);
        Self { label, detail: None, icon: None, disabled: None, mono: false }
    }

    /// Adds quiet text at the right end.
    pub const fn detail(mut self, detail: &'a str) -> Self {
        self.detail = Some(detail);
        self
    }

    /// Adds an icon glyph from [`crate::ds::icons`].
    pub const fn icon(mut self, glyph: &'a str) -> Self {
        self.icon = Some(glyph);
        self
    }

    /// Makes the row unavailable, saying why.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Draws the label in mono.
    pub const fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    const fn enabled(&self) -> bool {
        self.disabled.is_none()
    }
}

/// Anything a [`Listbox`] can show a row for: a [`ListItem`], or a choice that carries a value besides its text.
pub trait Row {
    /// What the row says.
    fn item(&self) -> ListItem<'_>;
}

impl Row for ListItem<'_> {
    fn item(&self) -> ListItem<'_> {
        *self
    }
}

/// The node id of option `index` in the list `list`: what a combobox names as its active descendant.
pub fn option_id(list: Id, index: usize) -> Id {
    list.with(("option", index))
}

/// The row nearest to `index` that can be chosen, looking in direction `dir` (1 or -1) first and then the other way;
/// `None` when no row can be chosen.
fn nearest<R: Row>(rows: &[R], index: usize, dir: isize) -> Option<usize> {
    let along = |dir: isize| {
        let mut i = index;
        loop {
            if rows[i].item().enabled() {
                return Some(i);
            }
            i = i.checked_add_signed(dir).filter(|&n| n < rows.len())?;
        }
    };
    along(dir).or_else(|| along(-dir))
}

/// Where the active row goes when it moves `delta` rows from `from` (`None`: nothing is active yet). The move stops at
/// the ends and lands on a row that can be chosen.
pub fn step<R: Row>(rows: &[R], from: Option<usize>, delta: isize) -> Option<usize> {
    if delta == 0 {
        // No movement asked for: stay (a zero step would otherwise search in no direction, forever).
        return from;
    }
    let end = rows.len().checked_sub(1)?;
    let target = match from {
        Some(f) => f.saturating_add_signed(delta).min(end),
        None if delta > 0 => 0,
        None => end,
    };
    nearest(rows, target, delta.signum())
}

/// The first row that can be chosen.
pub fn first<R: Row>(rows: &[R]) -> Option<usize> {
    rows.iter().position(|r| r.item().enabled())
}

/// The last row that can be chosen.
pub fn last<R: Row>(rows: &[R]) -> Option<usize> {
    rows.iter().rposition(|r| r.item().enabled())
}

/// How a navigation key asks the active row to move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Move {
    /// One row up (-1) or down (1).
    Step(isize),
    /// A page up or down: eight rows.
    Page(isize),
    /// To the first row.
    First,
    /// To the last row.
    Last,
}

/// Rows a Page Up or Page Down moves.
pub const PAGE: isize = 8;

/// The navigation key pressed this frame, consumed so nothing else acts on it.
///
/// `with_ends` includes Home and End; a combobox with a text field leaves them to the caret.
pub fn pressed_move(ctx: &Context, with_ends: bool) -> Option<Move> {
    ctx.input_mut(|i| {
        let mut key = |k| i.consume_key(Modifiers::NONE, k);
        if key(Key::ArrowDown) {
            Some(Move::Step(1))
        } else if key(Key::ArrowUp) {
            Some(Move::Step(-1))
        } else if key(Key::PageDown) {
            Some(Move::Page(1))
        } else if key(Key::PageUp) {
            Some(Move::Page(-1))
        } else if with_ends && key(Key::Home) {
            Some(Move::First)
        } else if with_ends && key(Key::End) {
            Some(Move::Last)
        } else {
            None
        }
    })
}

/// Where a [`Move`] takes the active row.
pub fn apply<R: Row>(rows: &[R], from: Option<usize>, movement: Move) -> Option<usize> {
    match movement {
        Move::Step(delta) => step(rows, from, delta),
        Move::Page(pages) => step(rows, from, pages * PAGE),
        Move::First => first(rows),
        Move::Last => last(rows),
    }
}

/// Tells AccessKit that `owner` is a combobox controlling the list `list`: expanded or not, and (while open) pointing
/// at the active option. `role` is `ComboBox` for a button-like owner and `EditableComboBox` for a text field.
pub fn link_combobox(owner: &Response, role: accesskit::Role, list: Id, expanded: bool, active: Option<usize>) {
    owner.ctx.accesskit_node_builder(owner.id, |node| {
        node.set_role(role);
        node.set_expanded(expanded);
        node.set_has_popup(accesskit::HasPopup::Listbox);
        if expanded {
            node.set_controls(vec![list.accesskit_id()]);
            if let Some(i) = active {
                node.set_active_descendant(option_id(list, i).accesskit_id());
            }
        }
    });
}

/// A list of rows in a scroll area, `rows.len()` of them but only the visible ones built.
#[must_use = "a list does nothing until it is shown"]
pub struct Listbox<'a, R> {
    id: Id,
    label: &'a str,
    rows: &'a [R],
    selected: Option<usize>,
    active: Option<usize>,
    visible_rows: usize,
    follow_active: bool,
    check_column: bool,
}

/// Rows shown before the list scrolls.
pub const VISIBLE_ROWS: usize = 8;

impl<'a, R: Row> Listbox<'a, R> {
    /// A list named `label` (its accessible name) over `rows`, keyed by `id`.
    pub fn new(id: Id, label: &'a str, rows: &'a [R]) -> Self {
        a11y::require_label(label);
        Self {
            id,
            label,
            rows,
            selected: None,
            active: None,
            visible_rows: VISIBLE_ROWS,
            follow_active: false,
            check_column: false,
        }
    }

    /// The row that holds the current value: it gets a check mark and the strong weight.
    pub const fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self.check_column = true;
        self
    }

    /// The row Enter would choose: highlighted, with a bar.
    pub const fn active(mut self, active: Option<usize>) -> Self {
        self.active = active;
        self
    }

    /// How many rows show before the list scrolls.
    pub const fn visible_rows(mut self, rows: usize) -> Self {
        self.visible_rows = rows;
        self
    }

    /// Scrolls the active row into view this frame (pass true after a key moved it).
    pub const fn follow_active(mut self, follow: bool) -> Self {
        self.follow_active = follow;
        self
    }

    /// Shows the list at the width of the `Ui`.
    pub fn show(self, ui: &mut Ui) -> ListboxOutput {
        let ds = ds(ui.ctx());
        let row_height = ds.metrics.tree_row;
        #[allow(clippy::cast_precision_loss, reason = "a row count")]
        let height = row_height * self.rows.len().min(self.visible_rows) as f32;
        let size = vec2(ui.available_width(), height);
        // The list's node first, so the options below attach to it and not to the window.
        ui.ctx().accesskit_node_builder(self.id, |node| node.set_role(accesskit::Role::ListBox));
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let builder = UiBuilder::new()
            .id_salt(self.id)
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min))
            .accessibility_parent(self.id);
        let mut inner = ui.new_child(builder);
        // `show_rows` places rows by the spacing in force when it is called, so rows must touch.
        inner.spacing_mut().item_spacing.y = 0.0;
        let mut clicked = None;
        let mut hovered = None;
        ScrollArea::vertical().id_salt(self.id).max_height(height).auto_shrink([false, true]).show_rows(
            &mut inner,
            row_height,
            self.rows.len(),
            |ui, range| {
                if self.follow_active
                    && let Some(active) = self.active
                {
                    // The rows start where the cursor is, minus the rows scrolled past.
                    #[allow(clippy::cast_precision_loss, reason = "a row index")]
                    let top = ui.cursor().top() + (active as f32 - range.start as f32) * row_height;
                    ui.scroll_to_rect(
                        Rect::from_min_size(pos2(rect.left(), top), vec2(rect.width(), row_height)),
                        None,
                    );
                }
                for index in range {
                    let hit = self.row(ui, &ds, index);
                    if hit.clicked() {
                        clicked = Some(index);
                    }
                    if hit.hovered() && self.rows[index].item().enabled() {
                        hovered = Some(index);
                    }
                }
            },
        );
        let response = ui.interact(rect, self.id, Sense::hover());
        let (len, label) = (self.rows.len(), self.label);
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::Other, true, label),
            |node| {
                node.set_role(accesskit::Role::ListBox);
                node.set_size_of_set(len);
            },
        );
        ListboxOutput { response, clicked, hovered }
    }

    /// Draws row `index` and returns its response.
    fn row(&self, ui: &mut Ui, ds: &Ds, index: usize) -> Response {
        let item = self.rows[index].item();
        let enabled = item.enabled();
        let row_height = ds.metrics.tree_row;
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::hover());
        // Not a focus stop: the combobox that owns the list keeps the focus and points at the active row.
        let sense = if enabled { Sense::CLICK } else { Sense::hover() };
        let response = ui.interact(rect, option_id(self.id, index), sense);
        let selected = self.selected == Some(index);
        let active = self.active == Some(index);
        let cut = if ui.is_rect_visible(rect) {
            paint_row(ui.painter(), ds, rect, &item, RowState { active, selected, check_column: self.check_column })
        } else {
            false
        };
        let position = index + 1;
        a11y::describe(
            &response,
            || WidgetInfo::selected(WidgetType::SelectableLabel, enabled, selected, item.label),
            |node| {
                node.set_role(accesskit::Role::ListBoxOption);
                node.set_selected(selected);
                node.set_position_in_set(position);
                if let Some(reason) = item.disabled {
                    node.set_description(reason);
                }
            },
        );
        // A cut label, or a row that can't be chosen, is completed by the tooltip.
        if cut || item.disabled.is_some() {
            let tip = Tip::new(item.label);
            let tip = item.disabled.map_or(tip, |reason| tip.detail(reason));
            return response.tip(tip);
        }
        response
    }
}

/// What a [`Listbox`] reports.
#[derive(Debug)]
pub struct ListboxOutput {
    /// The list as a whole.
    pub response: Response,
    /// The row that was clicked this frame.
    pub clicked: Option<usize>,
    /// The enabled row under the pointer.
    pub hovered: Option<usize>,
}

/// How a row is drawn.
#[derive(Clone, Copy)]
struct RowState {
    active: bool,
    selected: bool,
    check_column: bool,
}

/// The icon slot at the start of a row: a check for the chosen row, or the row's own icon.
const SLOT: f32 = 16.0;

/// The colours of a row: its fill, then the ink of its label, its detail and its check mark.
struct RowInk {
    fill: Option<egui::Color32>,
    label: egui::Color32,
    detail: egui::Color32,
    check: egui::Color32,
}

fn row_ink(t: &Theme, active: bool, enabled: bool) -> RowInk {
    RowInk {
        fill: active.then_some(t.bg4),
        label: paint::ink(t, enabled, t.text),
        detail: paint::ink(t, enabled, t.text3),
        check: t.accent_text,
    }
}

/// Draws one row; true when its label had to be cut to fit.
fn paint_row(painter: &egui::Painter, ds: &Ds, rect: Rect, item: &ListItem<'_>, state: RowState) -> bool {
    let t = &ds.theme;
    let ink = row_ink(t, state.active, item.enabled());
    if let Some(fill) = ink.fill {
        painter.rect_filled(rect.shrink2(vec2(space::S4, 0.0)), radius::CONTROL, fill);
        let bar = Rect::from_min_size(
            pos2(rect.left() + space::S4, rect.top() + space::S4),
            vec2(t.selection_bar, rect.height() - 2.0 * space::S4),
        );
        painter.rect_filled(bar, 1, t.accent);
    }
    let mut x = rect.left() + space::S12;
    let cy = rect.center().y;
    if state.check_column {
        if state.selected {
            paint::icon(painter, ds, pos2(x + SLOT / 2.0, cy), icons::CHECK, 14.0, ink.check, false);
        }
        x += SLOT + space::S4;
    }
    if let Some(glyph) = item.icon {
        paint::icon(painter, ds, pos2(x + SLOT / 2.0, cy), glyph, 16.0, paint::ink(t, item.enabled(), t.text2), false);
        x += SLOT + space::S6;
    }
    let mut right = rect.right() - space::S12;
    if let Some(detail) = item.detail {
        let width = paint::text_width(painter.ctx(), detail, &ds.text.small).min((right - x) * 0.4);
        let area = Rect::from_min_max(pos2(right - width, rect.top()), pos2(right, rect.bottom()));
        paint::text_in(painter, area, Align2::RIGHT_CENTER, detail, &ds.text.small, ink.detail);
        right -= width + space::S8;
    }
    let font = match (item.mono, state.selected) {
        (true, _) => &ds.text.mono,
        (false, true) => &ds.text.strong,
        (false, false) => &ds.text.body,
    };
    let room = (right - x).max(0.0);
    let area = Rect::from_min_max(pos2(x, rect.top()), pos2(x + room, rect.bottom()));
    paint::text_in(painter, area, Align2::LEFT_CENTER, item.label, font, ink.label);
    paint::text_width(painter.ctx(), item.label, font) > room
}

register_contrast!("listbox", pairs);

/// A row's label, detail and check mark on the popup surface and on the active row's fill.
fn pairs(table: &mut Table<'_>) {
    let t: Theme = *table.theme();
    for (state, ink, fill) in [("rest", row_ink(&t, false, true), t.bg2), ("active", row_ink(&t, true, true), t.bg4)] {
        table.add(format!("{state} label"), ink.label, fill, Rule::Text);
        table.add(format!("{state} detail"), ink.detail, fill, Rule::Text);
        table.add(format!("{state} check mark"), ink.check, fill, Rule::NonText);
    }
    table.add("disabled label", row_ink(&t, false, false).label, t.bg2, Rule::Disabled);
    table.add("active bar on the active row", t.accent, t.bg4, Rule::NonText);
}
