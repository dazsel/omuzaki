//! How the docking widget draws and constrains the panels: what may be dragged or resized, the minimum size of each
//! panel, and which panel a tab press focuses (`shell.md` section 4).

use egui::{Context, Response, Ui, WidgetText};
use egui_tiles::{Behavior, Container, LinearDir, SimplificationOptions, Tile, TileId, Tiles, Tree, UiResponse};

use omuzaki_ui_api::panel::PanelCtx;
use omuzaki_ui_api::registry::{PanelKind, Registry};

use super::host::Bodies;
use super::layout::PanelId;
use super::text::title;
use crate::ds::theme::ds;
use crate::ds::tokens::size;

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
            self.bodies.draw(ui, def, self.panel_ctx);
            if self.focus.as_deref() == Some(def.id) {
                let accent = ds(self.ctx).theme.accent;
                ui.painter().hline(rect.x_range(), rect.top(), egui::Stroke::new(2.0, accent));
            }
        }
        UiResponse::None
    }

    fn on_tab_button(&mut self, tiles: &mut Tiles<PanelId>, tile: TileId, button: Response) -> Response {
        if (button.clicked() || button.drag_started())
            && let Some(PanelId(panel)) = tiles.get_pane(&tile)
        {
            *self.focus = Some(panel.clone());
        }
        button
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
