//! `TabBar` (`design-system.md` 7.0): the workspace tabs of the top bar and the dock tabs over a panel.
//!
//! A label with a 2 pt accent underline on the active tab, so the active one is never told by colour alone. The
//! workspace tab can show a **violet dot** while an agent works in it (violet means an agent, nothing else); a dock
//! tab can be closed by its close button, by a middle click or by Delete. A tab can be icon-only, which is how
//! inactive workspace tabs look in the medium width class, and then its label is its accessible name and its tooltip.
//!
//! The bar is **one tab stop**. Left and Right (and Home and End) move to a tab and select it, as in a radio group
//! (roving focus); that is the automatic-activation pattern for tabs. AccessKit gets a `TabList` of `Tab`s, each in
//! its set. The close buttons are named nodes that answer `Click` but are not focus stops; Delete is their keyboard
//! path, so a row of closable tabs does not cost a Tab press each.

use egui::{
    Align2, Context, Id, Key, Modifiers, PointerButton, Rect, Response, Sense, Ui, UiBuilder, Widget, WidgetInfo,
    WidgetType, accesskit, pos2, vec2,
};

use super::segmented::roving_target;
use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Theme, radius, size, space};
use crate::register_contrast;

/// Which bar a tab belongs to: it sets the height.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TabKind {
    /// A workspace tab in the 40 pt top bar.
    #[default]
    Workspace,
    /// A dock tab (28 pt) over a panel.
    Dock,
}

impl TabKind {
    const fn height(self) -> f32 {
        match self {
            Self::Workspace => size::TOP_BAR,
            Self::Dock => size::DOCK_TAB,
        }
    }
}

/// One tab: its label and what it can show.
#[derive(Clone, Copy, Debug)]
#[must_use = "a tab does nothing until it is passed to a tab bar"]
pub struct TabItem<'a> {
    label: &'a str,
    icon: Option<&'a str>,
    icon_only: bool,
    agent_active: bool,
    closable: bool,
}

impl<'a> TabItem<'a> {
    /// A tab named `label`.
    pub fn new(label: &'a str) -> Self {
        a11y::require_label(label);
        Self { label, icon: None, icon_only: false, agent_active: false, closable: false }
    }

    /// An icon glyph before the label.
    pub const fn icon(mut self, glyph: &'a str) -> Self {
        self.icon = Some(glyph);
        self
    }

    /// Shows only the icon; the label stays the accessible name and the tooltip.
    pub const fn icon_only(mut self) -> Self {
        self.icon_only = true;
        self
    }

    /// Shows the violet dot: an agent is working in this tab.
    pub const fn agent_active(mut self, active: bool) -> Self {
        self.agent_active = active;
        self
    }

    /// The tab can be closed (a dock tab).
    pub const fn closable(mut self) -> Self {
        self.closable = true;
        self
    }
}

/// A row of tabs bound to the index of the selected one.
#[must_use = "a tab bar does nothing until it is added to a Ui"]
pub struct TabBar<'a> {
    label: &'a str,
    selected: &'a mut usize,
    tabs: &'a [TabItem<'a>],
    kind: TabKind,
    salt: Option<Id>,
}

impl<'a> TabBar<'a> {
    /// A bar named `label` (its accessible name) over `tabs`; `selected` is the index of the active one.
    pub fn new(label: &'a str, selected: &'a mut usize, tabs: &'a [TabItem<'a>]) -> Self {
        a11y::require_label(label);
        Self { label, selected, tabs, kind: TabKind::Workspace, salt: None }
    }

    /// Workspace tabs (the default) or dock tabs.
    pub const fn kind(mut self, kind: TabKind) -> Self {
        self.kind = kind;
        self
    }

    /// Keys the bar by `salt` instead of by position.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(Id::new(salt));
        self
    }

    /// Shows the bar and reports which tab was closed, if any.
    pub fn show(self, ui: &mut Ui) -> TabBarOutput {
        Show::new(self, ui).run(ui)
    }
}

impl Widget for TabBar<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui).response
    }
}

/// What a [`TabBar`] reports.
#[derive(Debug)]
pub struct TabBarOutput {
    /// The bar as a whole; `changed()` when another tab became the active one.
    pub response: Response,
    /// The index of the tab that was closed this frame, by its button, a middle click or Delete.
    pub closed: Option<usize>,
}

/// Horizontal padding inside a tab.
const PAD: f32 = 12.0;
/// The close button's square, which is also its hit target (WCAG 2.5.8).
const CLOSE: f32 = size::MIN_TARGET;
/// What a closable tab reserves at its right edge: the close button and the gap between it and the edge.
const CLOSE_RESERVE: f32 = CLOSE + space::S2;
/// Narrowest a tab gets when the bar is short of room: its icon and an ellipsis.
const MIN_TAB: f32 = 40.0;
/// Diameter of the agent dot.
const DOT: f32 = 7.0;

struct Show<'a> {
    bar: TabBar<'a>,
    ds: std::sync::Arc<Ds>,
    ctx: Context,
    enabled: bool,
}

impl<'a> Show<'a> {
    fn new(bar: TabBar<'a>, ui: &Ui) -> Self {
        let ctx = ui.ctx().clone();
        Self { ds: ds(&ctx), enabled: ui.is_enabled(), ctx, bar }
    }

    /// The width a tab wants: padding, icon, dot, label and close button.
    fn natural(&self, tab: &TabItem<'_>) -> f32 {
        let label = if tab.icon_only { 0.0 } else { paint::text_width(&self.ctx, tab.label, &self.ds.text.strong) };
        let icon = if tab.icon.is_some() { 16.0 + if tab.icon_only { 0.0 } else { space::S6 } } else { 0.0 };
        let dot = if tab.agent_active && !tab.icon_only { DOT + space::S6 } else { 0.0 };
        let close = if tab.closable { CLOSE_RESERVE } else { PAD };
        PAD + icon + dot + label + close
    }

    fn run(self, ui: &mut Ui) -> TabBarOutput {
        if self.bar.tabs.is_empty() {
            // Nothing to show: an empty, inert bar rather than a panic.
            let (_, response) = ui.allocate_exact_size(vec2(0.0, 0.0), Sense::hover());
            return TabBarOutput { response, closed: None };
        }
        let (t, m) = (&self.ds.theme, &self.ds.metrics);
        let count = self.bar.tabs.len();
        let height = self.bar.kind.height().max(m.icon_button);
        let wanted: f32 = self.bar.tabs.iter().map(|tab| self.natural(tab)).sum();
        let room = paint::fit_width(ui);
        // A bar short of room shrinks every tab in proportion, down to an icon and an ellipsis.
        let floor: f32 = self.bar.tabs.iter().map(|tab| self.natural(tab).min(MIN_TAB)).sum();
        let shrink =
            if wanted > room && wanted > floor { ((room - floor) / (wanted - floor)).clamp(0.0, 1.0) } else { 1.0 };

        let (rect, outer) = ui.allocate_exact_size(vec2(wanted.min(room), height), Sense::hover());
        let gid = self.bar.salt.map_or(outer.id, |salt| ui.make_persistent_id(salt));
        let mut group = if self.bar.salt.is_some() { ui.interact(rect, gid, Sense::hover()) } else { outer };
        let rect = paint::snap(&self.ctx, rect);
        // The list's node first, so the tabs attach to it; `accessibility` below completes it once its size is known.
        let a11y_on = self.ctx.accesskit_node_builder(gid, |node| node.set_role(accesskit::Role::TabList)).is_some();

        let selected = (*self.bar.selected).min(count - 1);
        let mut chosen = selected;
        let mut closed = None;
        let mut changed = false;
        if self.enabled
            && let Some(to) = roving_target(&self.ctx, gid, count)
        {
            chosen = to;
            self.ctx.memory_mut(|mem| mem.request_focus(gid.with(to)));
        }

        let mut ring = None;
        // A child `Ui` only to say which node the tabs belong to, which matters only while AccessKit is on: making one
        // costs an allocation, and a bar must not cost any per frame.
        let mut child;
        let ui = if a11y_on {
            child = ui.new_child(UiBuilder::new().id_salt(gid).max_rect(rect).accessibility_parent(gid));
            &mut child
        } else {
            ui
        };
        {
            let mut x = rect.left();
            for (i, tab) in self.bar.tabs.iter().enumerate() {
                let natural = self.natural(tab);
                let least = natural.min(MIN_TAB);
                let width = least + (natural - least) * shrink;
                let area = Rect::from_min_size(pos2(x, rect.top()), vec2(width, height));
                x += width;
                let sense = if !self.enabled {
                    Sense::hover()
                } else if i == chosen {
                    // The tab that holds the bar's one Tab stop: after a key moved the selection, the new one.
                    Sense::click()
                } else {
                    Sense::CLICK
                };
                let response = ui.interact(area, gid.with(i), sense);
                if response.has_focus() {
                    ring = Some(area);
                }
                if self.enabled {
                    if response.clicked() {
                        chosen = i;
                        self.ctx.memory_mut(|mem| mem.request_focus(gid.with(i)));
                    }
                    if tab.closable && (response.clicked_by(PointerButton::Middle) || self.delete_pressed(&response)) {
                        closed = Some(i);
                    }
                }
                let state = TabState { selected: i == chosen, hot: paint::hot(&response) };
                let hover = paint::fade(&self.ctx, &self.ds, gid.with(i).with("hover"), self.enabled && state.hot);
                let cut = paint_tab(ui.painter(), &self.ds, area, tab, state, hover);
                self.accessibility(&response, i, count, tab, state.selected);
                if tab.closable && self.close_button(ui, gid.with(("close", i)), area, tab, state, response.has_focus())
                {
                    closed = Some(i);
                }
                if tab.icon_only || cut {
                    let _ = response.tip(Tip::new(tab.label));
                }
            }
            if let Some(area) = ring.filter(|_| crate::ds::focus::visible(&self.ctx)) {
                paint::draw_ring(ui.painter(), t, area, radius::CONTROL, RingPlacement::Inside);
            }
        }

        if chosen != *self.bar.selected {
            *self.bar.selected = chosen;
            changed = true;
        }
        if changed {
            group.mark_changed();
        }
        let (label, enabled) = (self.bar.label, self.enabled);
        a11y::describe(
            &group,
            || WidgetInfo::labeled(WidgetType::Other, enabled, label),
            |node| {
                node.set_role(accesskit::Role::TabList);
                node.set_orientation(accesskit::Orientation::Horizontal);
            },
        );
        TabBarOutput { response: group, closed }
    }

    /// Delete pressed while this tab has focus.
    fn delete_pressed(&self, response: &Response) -> bool {
        response.has_focus() && self.ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete))
    }

    /// The close button of a tab: drawn while the tab is selected, hovered or focused, a named node always. True when
    /// it was pressed.
    fn close_button(&self, ui: &mut Ui, id: Id, area: Rect, tab: &TabItem<'_>, state: TabState, focused: bool) -> bool {
        let t = &self.ds.theme;
        let button = Rect::from_center_size(
            pos2(area.right() - CLOSE_RESERVE + CLOSE / 2.0, area.center().y),
            vec2(CLOSE, CLOSE),
        );
        let sense = if self.enabled { Sense::CLICK } else { Sense::hover() };
        let response = ui.interact(button, id, sense);
        let visible = state.selected || state.hot || focused || response.hovered();
        if visible {
            let hover = paint::fade(&self.ctx, &self.ds, response.id.with("hover"), response.hovered());
            if hover > 0.0 {
                ui.painter().rect_filled(
                    paint::square(button.center(), 20.0),
                    radius::CONTROL,
                    t.bg4.gamma_multiply(hover),
                );
            }
            let ink = paint::lerp(t.text3, t.text, hover);
            paint::icon(ui.painter(), &self.ds, button.center(), icons::X, 12.0, ink, false);
        }
        // The spoken name is built only when AccessKit asks for it, so a frame without a screen reader allocates nothing.
        a11y::describe(
            &response,
            || {
                let name = self.ds.text_with(Msg::TabClose, &[("label", tab.label)]);
                WidgetInfo::labeled(WidgetType::Button, self.enabled, name)
            },
            |_| {},
        );
        self.enabled && response.clicked()
    }

    fn accessibility(&self, response: &Response, index: usize, count: usize, tab: &TabItem<'_>, selected: bool) {
        let enabled = self.enabled;
        a11y::describe(
            response,
            || WidgetInfo::labeled(WidgetType::Button, enabled, tab.label),
            |node| {
                node.set_role(accesskit::Role::Tab);
                node.set_selected(selected);
                node.set_position_in_set(index + 1);
                node.set_size_of_set(count);
                if tab.agent_active {
                    node.set_description(self.ds.text(Msg::TabAgentActive).as_ref());
                }
            },
        );
    }
}

/// How a tab is drawn.
#[derive(Clone, Copy)]
struct TabState {
    selected: bool,
    hot: bool,
}

/// The ink of a tab's label and icon, and the fill of its hover.
struct TabInk {
    label: egui::Color32,
    fill: Option<egui::Color32>,
}

fn tab_ink(t: &Theme, selected: bool, hot: bool) -> TabInk {
    TabInk { label: if selected || hot { t.text } else { t.text2 }, fill: (hot && !selected).then_some(t.bg3) }
}

/// Draws a tab; true when its label had to be cut.
fn paint_tab(painter: &egui::Painter, ds: &Ds, area: Rect, tab: &TabItem<'_>, state: TabState, hover: f32) -> bool {
    let t = &ds.theme;
    let ink = tab_ink(t, state.selected, state.hot);
    if let Some(fill) = ink.fill {
        painter.rect_filled(area.shrink2(vec2(space::S2, space::S4)), radius::CONTROL, fill.gamma_multiply(hover));
    }
    if state.selected {
        let bar = Rect::from_min_max(
            pos2(area.left() + space::S4, area.bottom() - t.selection_bar),
            pos2(area.right() - space::S4, area.bottom()),
        );
        painter.rect_filled(bar, 1, t.accent);
    }
    let cy = area.center().y - if state.selected { 1.0 } else { 0.0 };
    let mut x = area.left() + PAD;
    if let Some(glyph) = tab.icon {
        paint::icon(painter, ds, pos2(x + 8.0, cy), glyph, 16.0, ink.label, state.selected);
        if tab.icon_only && tab.agent_active {
            // The dot sits on the icon's corner, with a ring of the bar's colour so it reads over the glyph.
            let dot = pos2(x + 15.0, cy - 6.0);
            painter.circle_filled(dot, DOT / 2.0 + 1.5, t.bg2);
            painter.circle_filled(dot, DOT / 2.0, t.agent);
        }
        x += 16.0 + if tab.icon_only { 0.0 } else { space::S6 };
    }
    if tab.icon_only {
        return false;
    }
    if tab.agent_active {
        painter.circle_filled(pos2(x + DOT / 2.0, cy), DOT / 2.0, t.agent);
        x += DOT + space::S6;
    }
    let right = area.right() - if tab.closable { CLOSE_RESERVE } else { PAD };
    let room = (right - x).max(0.0);
    let font = if state.selected { &ds.text.strong } else { &ds.text.body };
    let label = Rect::from_min_max(pos2(x, area.top()), pos2(x + room, area.bottom()));
    paint::text_in(painter, label, Align2::LEFT_CENTER, tab.label, font, ink.label);
    paint::text_width(painter.ctx(), tab.label, font) > room
}

register_contrast!("tab", pairs);

/// The label, the underline, the agent dot and the close glyph on the surfaces a tab bar sits on, and the label on the
/// hover fill.
fn pairs(table: &mut Table<'_>) {
    let t: Theme = *table.theme();
    let surfaces = [("bg1", t.bg1), ("bg2", t.bg2)];
    table.add_on("selected label", tab_ink(&t, true, false).label, &surfaces, Rule::Text);
    table.add_on("idle label", tab_ink(&t, false, false).label, &surfaces, Rule::Text);
    table.add("hovered label", tab_ink(&t, false, true).label, t.bg3, Rule::Text);
    table.add_on("underline", t.accent, &surfaces, Rule::NonText);
    table.add_on("agent dot", t.agent, &surfaces, Rule::NonText);
    table.add_on("close glyph", t.text3, &surfaces, Rule::Text);
    table.add("close glyph on its hover fill", t.text, t.bg4, Rule::Text);
}
