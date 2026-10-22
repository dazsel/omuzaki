//! How the docking widget draws and constrains the panels: what may be dragged or resized, the minimum size of each
//! panel, and which panel a tab press focuses (`shell.md` section 4).

use egui::{Align, Context, Layout, Popup, Rect, Response, Sense, Ui, UiBuilder, Vec2, WidgetText};
use egui_tiles::{Behavior, Container, LinearDir, SimplificationOptions, Tile, TileId, Tiles, Tree, UiResponse};

use omuzaki_ui_api::panel::PanelCtx;
use omuzaki_ui_api::registry::{PanelDef, PanelKind, Registry};

use super::host::Bodies;
use super::layout::PanelId;
use super::text::{Word, say, title};
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::theme::ds;
use crate::ds::tokens::size;
use crate::ds::widgets::{Button, Tip};

/// The shortest any docked panel gets, whatever its registration says.
const MIN_TILE: f32 = 160.0;
/// Shares are kept this far above their minimum, in points, before a clamp is worth doing.
const SLACK: f32 = 0.5;

/// The docking widget's view of a frame.
pub struct Dock<'a> {
    /// The egui context, for the theme and the language.
    pub ctx: &'a Context,
    /// The registered panels.
    pub registry: &'a Registry,
    /// The panels built so far; a body is drawn through it.
    pub bodies: &'a mut Bodies,
    /// What a panel reads and runs this frame.
    pub panel_ctx: &'a PanelCtx<'a>,
    /// Layout lock: no tile moves and no divider drags.
    pub locked: bool,
    /// The tiles that hold the primary panel ([`pinned`]); they never move.
    pub pinned: &'a [TileId],
    /// The focused panel; a press in a panel or on its tab sets it.
    pub focus: &'a mut Option<String>,
    /// What the panels' menus asked for this frame; the shell carries it out after the tree is drawn.
    pub asks: &'a mut Vec<PanelAsk>,
    /// Which side of the primary panel each panel is on (`Layout::side_of`), so a menu offers only the other side.
    pub sides: &'a [(String, Option<bool>)],
    /// Where each tab group's "⋯" button goes, by the panel it is for. The tab bar reserves the place before it draws
    /// its tabs; the button is drawn with the panel, after them, so Tab meets the tabs first, as the eye does.
    pub menus: Vec<(String, Rect)>,
    /// The tabs of groups too narrow for every tab's full name, found as each group's bar starts ([`Dock::crowded`]).
    pub crowded: Vec<TileId>,
    /// The tabs of groups too narrow even for their icons, that have no tab this frame ([`Dock::overflow`]).
    pub hidden: Vec<TileId>,
    /// The panels of those tabs, by the open panel of their group, whose "⋯" menu offers them.
    pub overflow: Vec<(String, Vec<String>)>,
}

/// What a panel's "⋯" menu asks of the shell: the keyboard's way to do what a drag does (`shell.md` section 4 rule 4,
/// WCAG 2.5.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelAsk {
    /// Move the panel to the left of the primary panel.
    MoveLeft(String),
    /// Move the panel to the right of the primary panel.
    MoveRight(String),
    /// Take the panel out of the arrangement.
    Close(String),
    /// Open a panel whose tab didn't fit its group's bar.
    Show(String),
    /// Go back to the workspace's default arrangement.
    Reset,
}

impl Behavior<PanelId> for Dock<'_> {
    fn pane_ui(&mut self, ui: &mut Ui, _tile: TileId, pane: &mut PanelId) -> UiResponse {
        let def = self.registry.panel(&pane.0);
        debug_assert!(def.is_some(), "the layout was repaired against the registry, so {} exists", pane.0);
        if let Some(def) = def {
            let rect = ui.max_rect();
            if ui.input(|i| i.pointer.any_pressed()) && ui.rect_contains_pointer(rect) {
                *self.focus = Some(def.id.to_owned());
            }
            if let Some(at) = self.menus.iter().position(|(panel, _)| panel == def.id) {
                let (_, place) = self.menus.swap_remove(at);
                let mut header =
                    ui.new_child(UiBuilder::new().max_rect(place).layout(Layout::left_to_right(Align::Center)));
                // The bar is above the body's clip; the ring needs a few points around the button.
                header.set_clip_rect(place.expand(4.0));
                self.panel_menu(&mut header, def);
            }
            self.bodies.draw(ui, def, self.panel_ctx);
            if self.focus.as_deref() == Some(def.id) {
                let accent = ds(self.ctx).theme.accent;
                ui.painter().hline(rect.x_range(), rect.top(), egui::Stroke::new(2.0, accent));
            }
        }
        UiResponse::None
    }

    /// The place of the "⋯" menu at the right of a tab group, for its shown panel ([`Dock::menus`]); the primary panel
    /// has none, since it never moves.
    fn top_bar_right_ui(
        &mut self,
        tiles: &Tiles<PanelId>,
        ui: &mut Ui,
        _tile: TileId,
        tabs: &egui_tiles::Tabs,
        _scroll_offset: &mut f32,
    ) {
        let active = tabs.active.or_else(|| tabs.children.first().copied());
        if self.crowded(tiles, tabs, ui.available_width()) {
            self.crowded.extend(&tabs.children);
            let hidden = self.overflow(tiles, tabs, active, ui.available_width());
            let panels = hidden
                .iter()
                .filter_map(|tile| match tiles.get(*tile) {
                    Some(Tile::Pane(PanelId(panel))) => Some(panel.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if let (Some(Tile::Pane(PanelId(open))), false) = (active.and_then(|t| tiles.get(t)), panels.is_empty()) {
                self.overflow.push((open.clone(), panels));
            }
            self.hidden.extend(hidden);
        }
        let Some(Tile::Pane(PanelId(panel))) = active.and_then(|t| tiles.get(t)) else { return };
        let Some(def) = self.registry.panel(panel).filter(|d| d.kind != PanelKind::Primary) else { return };
        let (place, _) = ui.allocate_exact_size(Vec2::splat(ds(ui.ctx()).metrics.icon_button), Sense::hover());
        self.menus.push((def.id.to_owned(), place));
    }

    fn on_tab_button(&mut self, tiles: &mut Tiles<PanelId>, tile: TileId, button: Response) -> Response {
        if (button.clicked() || button.drag_started())
            && let Some(PanelId(panel)) = tiles.get_pane(&tile)
        {
            *self.focus = Some(panel.clone());
        }
        let painter = button.ctx.layer_painter(button.layer_id);
        paint::focus_ring(&painter, &ds(self.ctx).theme, &button, button.rect, 0, RingPlacement::Inside);
        button
    }

    /// A tab as `egui_tiles` draws it, except in a crowded group: there the tabs that aren't open show their panel's icon
    /// alone, named in full for a screen reader and on hover, and the tabs too many even for that have no tab and are
    /// offered by the group's "⋯" menu, so the strip never needs scroll arrows, which have no focus ring and sit out of
    /// reading order.
    fn tab_ui(
        &mut self,
        tiles: &mut Tiles<PanelId>,
        ui: &mut Ui,
        id: egui::Id,
        tile_id: TileId,
        state: &egui_tiles::TabState,
    ) -> Response {
        if self.hidden.contains(&tile_id) {
            // No room and no stop: the "⋯" menu has it.
            let (_, rect) = ui.allocate_space(egui::vec2(0.0, ui.available_height()));
            return ui.interact(rect, id, Sense::hover());
        }
        let name = self.tab_title_for_tile(tiles, tile_id);
        let icon = match tiles.get(tile_id) {
            Some(Tile::Pane(PanelId(panel))) => self.registry.panel(panel).map(|def| def.icon),
            _ => None,
        };
        let compact = !state.active && icon.is_some() && self.crowded.contains(&tile_id);
        let font = egui::TextStyle::Button.resolve(ui.style());
        let (shown, font) = match icon {
            Some(icon) if compact => (WidgetText::from(icon), ds(ui.ctx()).text.icon(font.size + 2.0, false)),
            _ => (name.clone(), font),
        };
        let galley = shown.into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, font);
        let margin = self.tab_title_spacing(ui.visuals());
        let (_, rect) = ui.allocate_space(egui::vec2(galley.size().x + 2.0 * margin, ui.available_height()));
        let sense = if self.is_tile_draggable(tiles, tile_id) { Sense::click_and_drag() } else { Sense::click() };
        let mut response = ui.interact(rect, id, sense);
        let label = name.text().to_owned();
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), state.active, &label)
        });
        if compact {
            response = response.on_hover_text(&label);
        }
        if ui.is_rect_visible(rect) && !state.is_being_dragged {
            let fill = self.tab_bg_color(ui.visuals(), tiles, tile_id, state);
            let stroke = self.tab_outline_stroke(ui.visuals(), tiles, tile_id, state);
            ui.painter().rect(rect.shrink(0.5), 0.0, fill, stroke, egui::StrokeKind::Inside);
            if state.active {
                // The open tab joins the panel below it.
                ui.painter().hline(rect.x_range(), rect.bottom(), egui::Stroke::new(stroke.width + 1.0, fill));
            }
            let colour = self.tab_text_color(ui.visuals(), tiles, tile_id, state);
            let at = egui::Align2::LEFT_CENTER.align_size_within_rect(galley.size(), rect.shrink(margin)).min;
            ui.painter().galley(at, galley, colour);
        }
        self.on_tab_button(tiles, tile_id, response)
    }

    fn tab_title_for_pane(&mut self, pane: &PanelId) -> WidgetText {
        let name = self.registry.panel(&pane.0).map(|def| self.bodies.title(def));
        name.map_or_else(|| pane.0.clone(), |name| title(self.ctx, &name).into_owned()).into()
    }

    /// Nothing moves while locked, and the primary panel never leaves the centre: a group holding it stays put too.
    fn is_tile_draggable(&self, _tiles: &Tiles<PanelId>, tile: TileId) -> bool {
        !self.locked && !self.pinned.contains(&tile)
    }

    fn is_container_resizable(&self, _tiles: &Tiles<PanelId>, _tile: TileId) -> bool {
        !self.locked
    }

    fn tab_bar_height(&self, _style: &egui::Style) -> f32 {
        size::DOCK_TAB
    }

    fn min_size(&self) -> f32 {
        MIN_TILE
    }

    fn simplification_options(&self) -> SimplificationOptions {
        // Every panel keeps a tab strip with its name, alone or in a group.
        SimplificationOptions { all_panes_must_have_tabs: true, ..SimplificationOptions::default() }
    }
}

impl Dock<'_> {
    /// Whether the tabs of `tabs` need more than `width` with their full names and the "⋯" button beside them.
    fn crowded(&mut self, tiles: &Tiles<PanelId>, tabs: &egui_tiles::Tabs, width: f32) -> bool {
        let style = self.ctx.global_style();
        let margin = self.tab_title_spacing(&style.visuals);
        let font = egui::TextStyle::Button.resolve(&style);
        let names: f32 = tabs
            .children
            .iter()
            .map(|&child| {
                let name = self.tab_title_for_tile(tiles, child).text().to_owned();
                let width = self.ctx.fonts_mut(|f| f.layout_no_wrap(name, font.clone(), egui::Color32::WHITE).size().x);
                width + 2.0 * margin
            })
            .sum();
        // `egui_tiles` puts a gap before the "⋯" button and another after it, and scrolls at the first pixel over.
        names + ds(self.ctx).metrics.icon_button + 2.0 * style.spacing.item_spacing.x > width
    }

    /// The tabs of a crowded group that don't fit `width` even as icons, after the open one and the "⋯" button: the
    /// last ones, so the strip keeps its order.
    fn overflow(
        &mut self,
        tiles: &Tiles<PanelId>,
        tabs: &egui_tiles::Tabs,
        active: Option<TileId>,
        width: f32,
    ) -> Vec<TileId> {
        let style = self.ctx.global_style();
        let margin = self.tab_title_spacing(&style.visuals);
        let font = egui::TextStyle::Button.resolve(&style);
        let icon_font = ds(self.ctx).text.icon(font.size + 2.0, false);
        let measure = |text: String, font: egui::FontId| {
            self.ctx.fonts_mut(|f| f.layout_no_wrap(text, font, egui::Color32::WHITE).size().x) + 2.0 * margin
        };
        let mut used = ds(self.ctx).metrics.icon_button + 2.0 * style.spacing.item_spacing.x;
        if let Some(open) = active {
            used += measure(self.tab_title_for_tile(tiles, open).text().to_owned(), font.clone());
        }
        let mut hidden = Vec::new();
        for &child in tabs.children.iter().filter(|c| Some(**c) != active) {
            let icon = match tiles.get(child) {
                Some(Tile::Pane(PanelId(panel))) => self.registry.panel(panel).map(|def| def.icon),
                _ => None,
            };
            let width_of = match icon {
                Some(icon) => measure(icon.to_owned(), icon_font.clone()),
                None => measure(self.tab_title_for_tile(tiles, child).text().to_owned(), font.clone()),
            };
            if !hidden.is_empty() || used + width_of > width {
                hidden.push(child);
            } else {
                used += width_of;
            }
        }
        hidden
    }

    /// The "⋯" menu of the panel `def`: move it to the other side, close it, or reset the layout.
    fn panel_menu(&mut self, ui: &mut Ui, def: &PanelDef) {
        let ctx = ui.ctx().clone();
        let name = title(&ctx, &self.bodies.title(def)).into_owned();
        let label = say(&ctx, Word::PanelMenu, &[("panel", &name)]);
        let button = super::app::icon_button(ui, icons::DOTS_THREE, &Tip::new(&label), true);
        let id = def.id.to_owned();
        let hidden = self.overflow.iter().find(|(open, _)| open == def.id).map(|(_, panels)| panels.clone());
        Popup::menu(&button).show(|ui| {
            // The group's tabs that didn't fit, first: opening one is what the person came for.
            for panel in hidden.iter().flatten() {
                let Some(other) = self.registry.panel(panel) else { continue };
                let name = title(&ctx, &self.bodies.title(other)).into_owned();
                let text = say(&ctx, Word::PanelShow, &[("panel", &name)]);
                if ui.add(Button::new(&text).ghost().icon(other.icon)).clicked() {
                    self.asks.push(PanelAsk::Show(other.id.to_owned()));
                }
            }
            if hidden.is_some() {
                ui.separator();
            }
            let items = [
                (Word::PanelMoveLeft, icons::ARROW_LEFT, PanelAsk::MoveLeft(id.clone())),
                (Word::PanelMoveRight, icons::ARROW_RIGHT, PanelAsk::MoveRight(id.clone())),
                (Word::PanelClose, icons::X, PanelAsk::Close(id.clone())),
                (Word::ResetLayout, icons::ARROW_COUNTER_CLOCKWISE, PanelAsk::Reset),
            ];
            let side = self.sides.iter().find(|(p, _)| *p == id).and_then(|(_, side)| *side);
            for (word, glyph, ask) in items {
                // A panel already on one side is offered only the other.
                match (&ask, side) {
                    (PanelAsk::MoveLeft(_), Some(true)) | (PanelAsk::MoveRight(_), Some(false)) => continue,
                    _ => {}
                }
                let locked = self.locked && ask != PanelAsk::Reset;
                let reason = say(&ctx, Word::PanelLocked, &[]);
                let text = say(&ctx, word, &[]);
                let button = Button::new(&text).ghost().icon(glyph).disabled_if(locked, &reason);
                if ui.add(button).clicked() {
                    self.asks.push(ask);
                }
            }
        });
    }
}

/// The tiles that are, or contain, a primary panel. Worked out before the frame: while a container is drawn the widget
/// takes it out of the tree, so the question cannot be asked of the tree during the frame.
pub fn pinned(tree: &Tree<PanelId>, registry: &Registry) -> Vec<TileId> {
    tree.tiles.tile_ids().filter(|tile| holds_primary(&tree.tiles, *tile, registry)).collect()
}

/// Whether `tile` is, or contains, a primary panel.
fn holds_primary(tiles: &Tiles<PanelId>, tile: TileId, registry: &Registry) -> bool {
    match tiles.get(tile) {
        Some(Tile::Pane(PanelId(panel))) => registry.panel(panel).is_some_and(|def| def.kind == PanelKind::Primary),
        Some(Tile::Container(container)) => container.children().any(|child| holds_primary(tiles, *child, registry)),
        None => false,
    }
}

/// The least room `tile` needs along an axis: its panels' registered minimum, summed along a split's own axis and the
/// largest of its children across it.
fn min_extent(tiles: &Tiles<PanelId>, tile: TileId, horizontal: bool, registry: &Registry) -> f32 {
    match tiles.get(tile) {
        Some(Tile::Pane(PanelId(panel))) => {
            registry.panel(panel).map_or(MIN_TILE, |def| def.min_size[usize::from(!horizontal)])
        }
        Some(Tile::Container(Container::Linear(linear))) => {
            let along = (linear.dir == LinearDir::Horizontal) == horizontal;
            let each = linear.children.iter().map(|child| min_extent(tiles, *child, horizontal, registry));
            if along { each.sum() } else { each.fold(0.0, f32::max) }
        }
        Some(Tile::Container(container)) => {
            container.children().map(|child| min_extent(tiles, *child, horizontal, registry)).fold(0.0, f32::max)
        }
        None => 0.0,
    }
}

/// New shares (in points) that give each part at least its minimum, taking the room from the parts with some to spare.
/// `None` when every part already has its minimum. When the minimums do not fit in `length` at all they are scaled down
/// together, so no panel is starved more than another.
pub fn clamp_shares(shares: &[f32], mins: &[f32], length: f32) -> Option<Vec<f32>> {
    let total: f32 = shares.iter().sum();
    if shares.len() != mins.len() || length <= 0.0 || total <= 0.0 || !total.is_finite() {
        return None;
    }
    let points: Vec<f32> = shares.iter().map(|share| share / total * length).collect();
    if points.iter().zip(mins).all(|(p, m)| *p >= m - SLACK) {
        return None;
    }
    let need: f32 = mins.iter().sum();
    if need >= length {
        return Some(mins.iter().map(|m| m * length / need).collect());
    }
    let deficit: f32 = points.iter().zip(mins).map(|(p, m)| (m - p).max(0.0)).sum();
    let spare: f32 = points.iter().zip(mins).map(|(p, m)| (p - m).max(0.0)).sum();
    Some(points.iter().zip(mins).map(|(p, m)| if p < m { *m } else { p - (p - m) / spare * deficit }).collect())
}

/// Pulls every split's shares back to what its panels need, from where the last frame laid them out. A divider dragged
/// past a panel's minimum is put back on the next frame.
pub fn enforce_minimums(tree: &mut Tree<PanelId>, registry: &Registry) {
    let splits: Vec<TileId> = tree.tiles.tile_ids().collect();
    for id in splits {
        let Some(Tile::Container(Container::Linear(linear))) = tree.tiles.get(id) else { continue };
        let Some(rect) = tree.tiles.rect(id) else { continue };
        let horizontal = linear.dir == LinearDir::Horizontal;
        let length = if horizontal { rect.width() } else { rect.height() };
        let children = linear.children.clone();
        let shares: Vec<f32> = children.iter().map(|child| linear.shares[*child]).collect();
        let mins: Vec<f32> =
            children.iter().map(|child| min_extent(&tree.tiles, *child, horizontal, registry)).collect();
        let Some(fixed) = clamp_shares(&shares, &mins, length) else { continue };
        if let Some(Tile::Container(Container::Linear(linear))) = tree.tiles.get_mut(id) {
            for (child, share) in children.iter().zip(fixed) {
                linear.shares.set_share(*child, share);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::layout::{Dir, Layout, Node};

    #[test]
    fn a_part_below_its_minimum_is_raised_and_the_room_comes_from_the_others() {
        // 1200 points: 100 | 900 | 200 with minimums 240 | 480 | 288.
        let fixed = clamp_shares(&[100.0, 900.0, 200.0], &[240.0, 480.0, 288.0], 1200.0).expect("needs a fix");
        assert!((fixed.iter().sum::<f32>() - 1200.0).abs() < 0.01);
        assert!((fixed[0] - 240.0).abs() < 0.01 && (fixed[2] - 288.0).abs() < 0.01, "{fixed:?}");
        assert!(fixed[1] >= 480.0, "{fixed:?}");
    }

    #[test]
    fn nothing_changes_when_every_part_has_room_and_impossible_minimums_are_scaled_together() {
        assert_eq!(clamp_shares(&[300.0, 700.0], &[240.0, 480.0], 1000.0), None);
        let scaled = clamp_shares(&[1.0, 1.0], &[600.0, 600.0], 600.0).expect("cannot fit");
        assert_eq!(scaled, [300.0, 300.0]);
        assert_eq!(clamp_shares(&[f32::NAN, 1.0], &[1.0, 1.0], 100.0), None);
        assert_eq!(clamp_shares(&[1.0], &[1.0, 1.0], 100.0), None, "mismatched lengths are refused");
    }

    #[test]
    fn minimums_follow_the_registered_panels_along_each_axis() {
        let layout = Layout::new(Node::split(
            Dir::Horizontal,
            vec![
                Node::tabs(&["outliner", "library"]),
                Node::pane("viewport"),
                Node::split(Dir::Vertical, vec![Node::pane("inspector"), Node::pane("agent")], &[1.0, 1.0]),
            ],
            &[1.0, 1.0, 1.0],
        ));
        let tree = layout.to_tree("t");
        let root = tree.root.expect("a root");
        let registry = Registry::collect();
        assert_eq!(min_extent(&tree.tiles, root, true, &registry), 240.0 + 480.0 + 288.0);
        assert_eq!(min_extent(&tree.tiles, root, false, &registry), 320.0, "the stacked right dock needs 160 + 160");
    }

    #[test]
    fn the_primary_panel_and_any_group_holding_it_cannot_be_dragged() {
        let tree = Layout::new(Node::split(
            Dir::Horizontal,
            vec![Node::tabs(&["outliner", "viewport"]), Node::pane("inspector")],
            &[1.0, 1.0],
        ))
        .to_tree("t");
        let pinned = pinned(&tree, &Registry::collect());
        // The viewport, the group it is in and the split above that; not the Inspector or the Outliner.
        assert_eq!(pinned.len(), 3, "{pinned:?}");
        for id in tree.tiles.tile_ids() {
            if let Some(Tile::Pane(PanelId(panel))) = tree.tiles.get(id) {
                assert_eq!(pinned.contains(&id), panel == "viewport", "{panel}");
            }
        }
    }
}
