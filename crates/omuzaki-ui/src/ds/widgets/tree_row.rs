//! `TreeRow`: one row of the part tree and other lists (`design-system.md` section 7.7).
//!
//! ```text
//! │ │ ▸ ▣ Hair_front                              👁 🔒 ●
//!  indent guides · chevron · type icon · name · state icons
//! ```
//!
//! A row is one painter pass and one interaction rectangle: the chevron and the state icons are found by arithmetic
//! on the pointer position, not as child widgets, so a 10,000-row tree costs what a 40-row one does when the panel
//! draws only the visible rows (`ScrollArea::show_rows`, uniform row height).

use egui::{
    Align, Align2, Color32, Id, Key, Rect, Response, Sense, Stroke, TextEdit, Ui, WidgetInfo, WidgetType, accesskit,
    pos2, vec2,
};

use super::ids;
use super::ime;
use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{radius, space};
use crate::register_contrast;

/// Indent per level, and the width of the chevron's hit area (both 16 pt in the spec).
const LEVEL: f32 = 16.0;
const ICON_SLOT: f32 = 20.0;
/// A row shows at most this many state icons (eye, lock, agent dot, gate warning), so their zones fit on the stack.
pub const MAX_ICONS: usize = 4;

/// Hit rectangles of a row's state icons, left to right, with no heap allocation.
struct Zones {
    rects: [Rect; MAX_ICONS],
    len: usize,
}

impl Zones {
    fn as_slice(&self) -> &[Rect] {
        &self.rects[..self.len]
    }

    fn leftmost(&self) -> Option<Rect> {
        self.as_slice().first().copied()
    }
}

/// Whether a row has children and whether they are showing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expand {
    /// No children: no chevron.
    Leaf,
    /// Children are showing.
    Open,
    /// Children are hidden.
    Closed,
}

/// A right-aligned state icon. Each has a tooltip and a screen-reader label.
#[derive(Clone, Copy, Debug)]
pub enum RowIcon<'a> {
    /// Visibility: an open eye when `true`, a slashed one when `false`. Clicking it toggles.
    Visible(bool),
    /// Fenced by an agent or locked by you; `owner` is who.
    Locked {
        /// Who holds the lock.
        owner: &'a str,
    },
    /// An agent touched this part since you last looked: a violet dot.
    AgentTouched {
        /// The agent's name.
        by: &'a str,
    },
    /// A gate on this part is failing; the text says which.
    GateWarning {
        /// "likeness 0.81 < 0.85".
        text: &'a str,
    },
}

impl RowIcon<'_> {
    /// The message that names the icon to a screen reader and in its tooltip.
    const fn name(&self) -> Msg {
        match self {
            Self::Visible(true) => Msg::RowVisible,
            Self::Visible(false) => Msg::RowHidden,
            Self::Locked { .. } => Msg::RowLocked,
            Self::AgentTouched { .. } => Msg::RowAgentTouched,
            Self::GateWarning { .. } => Msg::RowGateWarning,
        }
    }
}

/// Where a drop would land relative to this row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropLine {
    /// Not a drop target.
    None,
    /// Insert before this row.
    Above,
    /// Insert after this row.
    Below,
}

/// What a click landed on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowHit {
    /// The row body: select.
    Row,
    /// The chevron: expand or collapse.
    Chevron,
    /// The state icon at this index: act on it (toggle visibility, show the lock owner).
    Icon(usize),
}

/// What happened to a row this frame.
#[derive(Debug)]
pub struct TreeRowOutput {
    /// The row's response (select, drag, context menu, focus).
    pub response: Response,
    /// What a click landed on, when `response.clicked()`.
    pub hit: Option<RowHit>,
    /// The new name when an inline rename was committed with Enter or by leaving the field.
    pub renamed: Option<String>,
    /// An inline rename was cancelled with Esc.
    pub rename_cancelled: bool,
}

/// A tree or list row.
#[must_use = "a row does nothing until it is shown"]
pub struct TreeRow<'a> {
    id: Id,
    label: &'a str,
    level: usize,
    icon: &'a str,
    expand: Expand,
    selected: bool,
    panel_focused: bool,
    tab_stop: bool,
    icons: &'a [RowIcon<'a>],
    matched: Option<(usize, usize)>,
    drop: DropLine,
    renaming: Option<&'a mut String>,
}

impl<'a> TreeRow<'a> {
    /// A row keyed by `id` (use the part's id), named `label`, drawn with the type icon `icon`.
    pub fn new(id: impl std::hash::Hash + std::fmt::Debug, label: &'a str, icon: &'a str) -> Self {
        a11y::require_label(label);
        Self {
            id: Id::new(id),
            label,
            level: 0,
            icon,
            expand: Expand::Leaf,
            selected: false,
            panel_focused: true,
            tab_stop: false,
            icons: &[],
            matched: None,
            drop: DropLine::None,
            renaming: None,
        }
    }

    /// Nesting depth; each level indents 16 pt.
    pub const fn level(mut self, level: usize) -> Self {
        self.level = level;
        self
    }

    /// Whether the row has children and whether they show.
    pub const fn expand(mut self, expand: Expand) -> Self {
        self.expand = expand;
        self
    }

    /// Selected rows get a `bg4` fill and a 2 pt accent bar (hollow when the panel is not focused).
    pub const fn selected(mut self, selected: bool, panel_focused: bool) -> Self {
        self.selected = selected;
        self.panel_focused = panel_focused;
        self
    }

    /// Makes this the one row that takes Tab focus (roving focus: the panel moves it with the arrow keys).
    pub const fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.tab_stop = tab_stop;
        self
    }

    /// The right-aligned state icons, rightmost last.
    pub const fn icons(mut self, icons: &'a [RowIcon<'a>]) -> Self {
        self.icons = icons;
        self
    }

    /// The byte range of the label the filter matched: shown in Medium weight with an accent underline.
    pub const fn matched(mut self, range: (usize, usize)) -> Self {
        self.matched = Some(range);
        self
    }

    /// Shows the 2 pt accent drop line above or below the row.
    pub const fn drop_line(mut self, drop: DropLine) -> Self {
        self.drop = drop;
        self
    }

    /// Replaces the name with an inline text field editing `name`.
    pub fn renaming(mut self, name: &'a mut String) -> Self {
        self.renaming = Some(name);
        self
    }

    /// Shows the row at full width.
    pub fn show(mut self, ui: &mut Ui) -> TreeRowOutput {
        let ds = ds(ui.ctx());
        let (t, m) = (&ds.theme, &ds.metrics);
        let ctx = ui.ctx().clone();
        let size = vec2(ui.available_width(), m.tree_row);
        let sense = if self.tab_stop { Sense::click_and_drag() } else { Sense::CLICK | Sense::DRAG };
        let (rect, response) = {
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            (rect, ui.interact(rect, self.id, sense))
        };
        let rect = paint::snap(&ctx, rect);

        let chevron_zone =
            Rect::from_min_size(pos2(rect.left() + LEVEL * self.level as f32, rect.top()), vec2(LEVEL, rect.height()));
        let zones = self.icon_zones(rect);
        let icon_zones = zones.as_slice();

        let mut hit = None;
        if response.clicked() {
            let pos = response.interact_pointer_pos().unwrap_or(rect.center());
            hit = Some(if self.expand != Expand::Leaf && chevron_zone.contains(pos) {
                RowHit::Chevron
            } else if let Some(i) = icon_zones.iter().position(|z| z.contains(pos)) {
                RowHit::Icon(i)
            } else {
                RowHit::Row
            });
        }

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let hover = paint::fade(&ctx, &ds, self.id, paint::hot(&response) && !self.selected);
            self.paint_background(painter, t, rect, hover);
            let cy = rect.center().y;
            if self.expand != Expand::Leaf {
                let glyph = if self.expand == Expand::Open { icons::CARET_DOWN } else { icons::CARET_RIGHT };
                paint::icon(painter, &ds, chevron_zone.center(), glyph, 12.0, t.text2, false);
            }
            let icon_x = chevron_zone.right() + space::S2 + 8.0;
            paint::icon(
                painter,
                &ds,
                pos2(icon_x, cy),
                self.icon,
                size_of_icon(),
                if self.selected { t.text } else { t.text2 },
                self.selected,
            );

            let name_left = icon_x + 8.0 + space::S6;
            let name_right = zones.leftmost().map_or(rect.right(), |z| z.left()) - space::S4;
            let name_area =
                Rect::from_min_max(pos2(name_left, rect.top()), pos2(name_right.max(name_left), rect.bottom()));
            if self.renaming.is_none() {
                self.paint_name(painter, &ds, name_area);
            }
            for (zone, icon) in icon_zones.iter().zip(self.icons) {
                let hot = response.hovered() && response.hover_pos().is_some_and(|p| zone.contains(p));
                draw_state_icon(painter, &ds, zone.center(), icon, hot);
            }
            if self.drop != DropLine::None {
                let y = if self.drop == DropLine::Above { rect.top() + 1.0 } else { rect.bottom() - 1.0 };
                painter.rect_filled(
                    Rect::from_min_max(pos2(name_left - 24.0, y - 1.0), pos2(rect.right() - 4.0, y + 1.0)),
                    1,
                    t.accent,
                );
            }
            paint::focus_ring(painter, t, &response, rect, radius::CONTROL, RingPlacement::Inside);
        }

        // Tooltips for the state icons, found by pointer position.
        let mut response = response;
        if let Some(pos) = response.hover_pos()
            && let Some((_, icon)) = icon_zones.iter().zip(self.icons).find(|(z, _)| z.contains(pos))
        {
            let name = ds.text(icon.name());
            response = response.tip(icon_tip(&name, icon));
        }

        let (renamed, rename_cancelled) =
            self.rename_field(ui, &ds, rect, chevron_zone.right() + space::S2 + 8.0 + 8.0 + space::S6, &zones);
        // While renaming, the row's own node stays and the field is a second named node inside it.

        let (label, level, selected, expand) = (self.label, self.level, self.selected, self.expand);
        a11y::describe(
            &response,
            || WidgetInfo::selected(WidgetType::Other, true, selected, label),
            |node| {
                node.set_role(accesskit::Role::TreeItem);
                node.set_level(level + 1);
                node.set_selected(selected);
                match expand {
                    Expand::Open => node.set_expanded(true),
                    Expand::Closed => node.set_expanded(false),
                    Expand::Leaf => {}
                }
            },
        );
        TreeRowOutput { response, hit, renamed, rename_cancelled }
    }

    /// The selection fill and bar (or the hover fill) and the indent guides.
    fn paint_background(&self, painter: &egui::Painter, t: &crate::ds::tokens::Theme, rect: Rect, hover: f32) {
        if self.selected {
            painter.rect_filled(rect, radius::CONTROL, t.bg4);
            let bar = Rect::from_min_size(rect.min, vec2(t.selection_bar, rect.height()));
            if self.panel_focused {
                painter.rect_filled(bar, 1, t.accent);
            } else {
                // Hollow when the panel is not focused: the same shape, so selection never relies on colour alone.
                painter.rect_stroke(bar, 1, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
            }
        } else if hover > 0.0 {
            painter.rect_filled(rect, radius::CONTROL, t.bg3.gamma_multiply(hover));
        }
        // Indent guides: 1 pt `line` verticals that join into continuous lines down a column of rows.
        for level in 0..self.level {
            let x = (rect.left() + LEVEL * level as f32 + LEVEL / 2.0).round() + 0.5;
            painter.vline(x, rect.y_range(), Stroke::new(1.0, t.line));
        }
    }

    /// One hit rectangle per state icon, packed from the right edge (the first icon is leftmost).
    fn icon_zones(&self, rect: Rect) -> Zones {
        debug_assert!(self.icons.len() <= MAX_ICONS, "a row shows at most {MAX_ICONS} state icons");
        let len = self.icons.len().min(MAX_ICONS);
        let mut zones = Zones { rects: [Rect::NOTHING; MAX_ICONS], len };
        let mut right = rect.right() - space::S4;
        for slot in (0..len).rev() {
            zones.rects[slot] =
                Rect::from_min_size(pos2(right - ICON_SLOT, rect.top()), vec2(ICON_SLOT, rect.height()));
            right -= ICON_SLOT;
        }
        zones
    }

    fn paint_name(&self, painter: &egui::Painter, ds: &crate::ds::theme::Ds, area: Rect) {
        let t = &ds.theme;
        let font = if self.selected { &ds.text.strong } else { &ds.text.body };
        match self.matched.and_then(|(a, b)| Some((self.label.get(..a)?, self.label.get(a..b)?, self.label.get(b..)?)))
        {
            Some((pre, hit, post)) => {
                let mut x = area.left();
                let cy = area.center().y;
                let r = paint::text(painter, pos2(x, cy), Align2::LEFT_CENTER, pre, font, t.text);
                x = r.right();
                let r = paint::text(painter, pos2(x, cy), Align2::LEFT_CENTER, hit, &ds.text.strong, t.text);
                painter.rect_filled(
                    Rect::from_min_max(pos2(r.left(), r.bottom() - 1.0), pos2(r.right(), r.bottom() + 1.0)),
                    1,
                    t.accent,
                );
                x = r.right();
                paint::text(painter, pos2(x, cy), Align2::LEFT_CENTER, post, font, t.text);
            }
            None => {
                paint::text_in(painter, area, Align2::LEFT_CENTER, self.label, font, t.text);
            }
        }
    }

    /// Inline rename: a text field over the name. Enter or leaving commits; Esc cancels.
    fn rename_field(&mut self, ui: &mut Ui, ds: &Ds, rect: Rect, left: f32, zones: &Zones) -> (Option<String>, bool) {
        let Some(name) = self.renaming.as_deref_mut() else {
            // The auto id the editor's child `Ui` takes while renaming, so the widgets after this row are called the
            // same whether or not it is being renamed.
            ids::skip_editor_id(ui);
            return (None, false);
        };
        let t = &ds.theme;
        let right = zones.leftmost().map_or(rect.right(), |z| z.left()) - space::S4;
        let field =
            Rect::from_min_max(pos2(left - 6.0, rect.top() + 2.0), pos2(right.max(left + 60.0), rect.bottom() - 2.0));
        let id = self.id.with("rename");
        let ime = ime::begin(ui.ctx(), id, name);
        // The field is a part of the row, which was allocated already: a child `Ui`, not a scope that would pull the
        // cursor up to the field's bottom edge (2 pt above the row's) and let the next row overlap this one.
        let response = {
            let ui = &mut ids::editor_ui(ui, field);
            ui.visuals_mut().selection.stroke = Stroke::new(2.0, t.accent);
            let mut composing_layout = ime.layouter(ds.text.body.clone(), t.text, Align::LEFT);
            let mut edit = TextEdit::singleline(&mut *name)
                .id(id)
                .font(ds.text.body.clone())
                .text_color(t.text)
                .vertical_align(Align::Center)
                .margin(egui::Margin::symmetric(6, 0))
                .desired_width(field.width())
                .min_size(field.size())
                // Esc is ours (it cancels the rename, unless the IME is composing): egui must not surrender focus on it.
                .event_filter(egui::EventFilter { escape: true, ..egui::EventFilter::default() });
            if ime.composing() {
                edit = edit.layouter(&mut composing_layout);
            }
            let output = edit.show(ui);
            if !output.response.has_focus() && !output.response.lost_focus() {
                output.response.request_focus();
            }
            output.response
        };
        ime.finish(ui.ctx(), id, name);
        let name_for_readers = ds.text_with(Msg::RowRename, &[("name", self.label)]);
        ui.ctx().accesskit_node_builder(response.id, |node| node.set_label(name_for_readers));
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            return (None, true);
        }
        if response.lost_focus() {
            return (Some(name.clone()), false);
        }
        (None, false)
    }
}

const fn size_of_icon() -> f32 {
    16.0
}

fn icon_tip<'a>(name: &'a str, icon: &RowIcon<'a>) -> Tip<'a> {
    let detail = match icon {
        RowIcon::Visible(_) => None,
        RowIcon::Locked { owner } => Some(*owner),
        RowIcon::AgentTouched { by } => Some(*by),
        RowIcon::GateWarning { text } => Some(*text),
    };
    let mut tip = Tip::new(name);
    tip.detail = detail;
    tip
}

fn draw_state_icon(
    painter: &egui::Painter,
    ds: &crate::ds::theme::Ds,
    centre: egui::Pos2,
    icon: &RowIcon<'_>,
    hot: bool,
) {
    let t = &ds.theme;
    let strong = |c: Color32| if hot { paint::lerp(c, t.text, 0.35) } else { c };
    match icon {
        RowIcon::Visible(true) => {
            paint::icon(painter, ds, centre, icons::EYE, 16.0, strong(t.text2), true);
        }
        RowIcon::Visible(false) => {
            paint::icon(painter, ds, centre, icons::EYE_SLASH, 16.0, strong(t.text3), false);
        }
        RowIcon::Locked { .. } => {
            paint::icon(painter, ds, centre, icons::LOCK, 16.0, strong(t.text2), true);
        }
        RowIcon::AgentTouched { .. } => {
            painter.circle_filled(centre, 4.0, t.agent);
        }
        RowIcon::GateWarning { .. } => {
            paint::icon(painter, ds, centre, icons::WARNING, 16.0, t.warn, true);
        }
    }
}

register_contrast!("tree_row", pairs);

/// The state icons on a selected row (`bg4`), where status colours are shapes rather than text, and the drop line.
fn pairs(table: &mut Table<'_>) {
    let t = *table.theme();
    table.add("gate warning icon on a selected row", t.warn, t.bg4, Rule::NonText);
    table.add("agent dot on a selected row", t.agent, t.bg4, Rule::NonText);
    table.add("lock icon on a selected row", t.text2, t.bg4, Rule::NonText);
    table.add("hidden eye on a selected row", t.text3, t.bg4, Rule::NonText);
    table.add("selection bar on a selected row", t.accent, t.bg4, Rule::NonText);
    table.add("drop line on the panel", t.accent, t.bg1, Rule::NonText);
}
