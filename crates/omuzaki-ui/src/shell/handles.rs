//! The docking widget's own handles, named and keyboard-operable (#707; WCAG 2.1.1, 2.4.7, 4.1.2).
//!
//! `egui_tiles` makes each divider between panels and the background of each movable tab strip a focusable widget with
//! no name: Tab stops on it, a screen reader reads nothing, and a divider cannot be moved from the keyboard. After the
//! tree is drawn, [`name_handles`] finds those widgets by where the tree put them, names them (a divider is a
//! `Splitter` called "Resize Outliner and Viewport"; a tab strip is a `TabList` called "Outliner and Library tabs"),
//! draws the focus ring, and lets the arrow keys move a focused divider or switch a focused strip's tab.

use egui::accesskit::{Orientation, Role};
use egui::{Context, EventFilter, Id, Key, Modifiers, Rect, Ui};
use egui_tiles::{Container, LinearDir, Tile, TileId, Tiles, Tree};

use omuzaki_ui_api::registry::Registry;

use super::layout::PanelId;
use super::text::{Word, say, title};
use crate::ds::paint::{RingPlacement, draw_ring};
use crate::ds::theme::ds;
use crate::ds::{focus, tokens::size};

/// How far one arrow press moves a divider, in points; with Shift, four times as far.
const STEP: f32 = 16.0;

/// A widget the docking widget made, by where it is.
#[derive(Clone, Copy)]
struct Found {
    id: Id,
    rect: Rect,
}

/// Names the dividers and tab strips the tree drew this frame and carries out the keys pressed on them. Returns
/// whether a divider moved, so the caller records the arrangement.
pub fn name_handles(ui: &Ui, tree: &mut Tree<PanelId>, registry: &Registry) -> bool {
    let ctx = ui.ctx().clone();
    let found = unnamed(&ctx);
    if found.is_empty() {
        return false;
    }
    let mut moved = false;
    let containers: Vec<TileId> = tree.tiles.tile_ids().collect();
    for container in containers {
        match tree.tiles.get(container) {
            Some(Tile::Container(Container::Linear(_))) => {
                moved |= dividers(ui, &ctx, &mut tree.tiles, container, &found, registry);
            }
            Some(Tile::Container(Container::Tabs(_))) => {
                strip(ui, &ctx, tree, container, &found, registry);
            }
            _ => {}
        }
    }
    moved
}

/// The focusable widgets drawn this frame that sense a drag: the candidates for a divider or a strip.
fn unnamed(ctx: &Context) -> Vec<Found> {
    ctx.viewport(|vp| {
        vp.this_pass
            .widgets
            .layers()
            .flat_map(|(_, rects)| rects.iter())
            .filter(|w| w.enabled && w.sense.is_focusable() && w.sense.senses_drag())
            .map(|w| Found { id: w.id, rect: w.rect })
            .collect()
    })
}

/// The widget at `rect`, give or take a pixel's rounding.
fn at(found: &[Found], rect: Rect) -> Option<Found> {
    found.iter().copied().find(|f| (f.rect.min - rect.min).length() <= 1.0 && (f.rect.max - rect.max).length() <= 1.0)
}

/// The name of what `tile` shows: its active panel's title, or the first panel inside it.
fn name_of(ctx: &Context, tiles: &Tiles<PanelId>, tile: TileId, registry: &Registry) -> String {
    let panel = match tiles.get(tile) {
        Some(Tile::Pane(PanelId(panel))) => Some(panel.clone()),
        Some(Tile::Container(Container::Tabs(tabs))) => {
            tabs.active.or_else(|| tabs.children.first().copied()).map(|t| name_of(ctx, tiles, t, registry))
        }
        Some(Tile::Container(container)) => container.children().next().map(|t| name_of(ctx, tiles, *t, registry)),
        None => None,
    };
    let Some(panel) = panel else { return String::new() };
    match (tiles.get(tile), registry.panel(&panel)) {
        (Some(Tile::Pane(_)), Some(def)) => title(ctx, def.title).into_owned(),
        _ => panel,
    }
}

/// The dividers between the visible children of the split `container`, where `egui_tiles` puts them.
fn dividers(
    ui: &Ui,
    ctx: &Context,
    tiles: &mut Tiles<PanelId>,
    container: TileId,
    found: &[Found],
    registry: &Registry,
) -> bool {
    let Some(Tile::Container(Container::Linear(linear))) = tiles.get(container) else { return false };
    let (dir, children) = (linear.dir, linear.children.clone());
    let visible: Vec<TileId> = children.into_iter().filter(|c| tiles.is_visible_in_layout(*c)).collect();
    let Some(parent) = tiles.rect(container) else { return false };
    let grab = 2.0 * ui.style().interaction.resize_grab_radius_side;
    let mut moved = false;
    for pair in visible.windows(2) {
        let [first, second] = [pair[0], pair[1]];
        let (Some(a), Some(b)) = (tiles.rect(first), tiles.rect(second)) else { continue };
        let line = match dir {
            LinearDir::Horizontal => {
                let x = egui::lerp(a.right()..=b.left(), 0.5);
                Rect::from_center_size(egui::pos2(x, parent.center().y), egui::vec2(grab, parent.height()))
            }
            LinearDir::Vertical => {
                let y = egui::lerp(a.bottom()..=b.top(), 0.5);
                Rect::from_center_size(egui::pos2(parent.center().x, y), egui::vec2(parent.width(), grab))
            }
        };
        let Some(handle) = at(found, line) else { continue };
        let (one, two) = (name_of(ctx, tiles, first, registry), name_of(ctx, tiles, second, registry));
        let label = say(ctx, Word::DockSplitter, &[("first", &one), ("second", &two)]);
        ctx.accesskit_node_builder(handle.id, |node| {
            node.set_role(Role::Splitter);
            node.set_label(label);
            node.set_orientation(match dir {
                // The divider of a side-by-side split is a vertical line.
                LinearDir::Horizontal => Orientation::Vertical,
                LinearDir::Vertical => Orientation::Horizontal,
            });
        });
        if !ctx.memory(|m| m.has_focus(handle.id)) {
            continue;
        }
        keep_arrows(ctx, handle.id);
        if focus::visible(ctx) {
            draw_ring(ui.painter(), &ds(ctx).theme, handle.rect, 0, RingPlacement::Inside);
        }
        let (back, forth) = match dir {
            LinearDir::Horizontal => (Key::ArrowLeft, Key::ArrowRight),
            LinearDir::Vertical => (Key::ArrowUp, Key::ArrowDown),
        };
        let step = if ctx.input(|i| i.modifiers.shift) { STEP * 4.0 } else { STEP };
        let mut points = 0.0;
        for (key, sign) in [(back, -1.0), (forth, 1.0)] {
            // Logical matching: Shift held still counts as the plain arrow.
            let presses = ctx.input_mut(|i| i.count_and_consume_key(Modifiers::NONE, key));
            points += sign * step * presses as f32;
        }
        if points != 0.0 {
            let length = match dir {
                LinearDir::Horizontal => parent.width(),
                LinearDir::Vertical => parent.height(),
            };
            if let Some(Tile::Container(Container::Linear(linear))) = tiles.get_mut(container) {
                moved |= shift(linear, &visible, first, second, points, length);
            }
        }
    }
    moved
}

/// Moves the divider between `first` and `second` by `points`, keeping both at least a tab strip wide; the minimums
/// the shell registers are held on the next frame (`dock::enforce_minimums`).
fn shift(
    linear: &mut egui_tiles::Linear,
    visible: &[TileId],
    first: TileId,
    second: TileId,
    points: f32,
    length: f32,
) -> bool {
    let total: f32 = visible.iter().map(|c| linear.shares[*c]).sum();
    if length <= 0.0 || total <= 0.0 {
        return false;
    }
    let per_point = total / length;
    let (a, b) = (linear.shares[first], linear.shares[second]);
    let floor = size::DOCK_TAB * per_point;
    let delta = (points * per_point).clamp(floor - a, b - floor);
    if delta.abs() < f32::EPSILON {
        return false;
    }
    linear.shares.set_share(first, a + delta);
    linear.shares.set_share(second, b - delta);
    true
}

/// The background of the movable tab strip of `container`: named as the group's tab list, and Left and Right switch
/// its tab while it has the keyboard.
fn strip(ui: &Ui, ctx: &Context, tree: &mut Tree<PanelId>, container: TileId, found: &[Found], registry: &Registry) {
    let Some(Tile::Container(Container::Tabs(tabs))) = tree.tiles.get(container) else { return };
    let children = tabs.children.clone();
    let Some(rect) = tree.tiles.rect(container) else { return };
    let tab_ids: Vec<Id> = children.iter().map(|c| c.egui_id(tree.id())).collect();
    // The strip's background spans the top of the group, under its tabs; the tabs themselves are named already. The
    // tree rounds its widgets to whole pixels while the gaps between groups leave their edges on half points.
    let Some(background) = found.iter().copied().find(|f| {
        !tab_ids.contains(&f.id)
            && (f.rect.top() - rect.top()).abs() <= 1.0
            && f.rect.height() <= size::DOCK_TAB + 1.0
            && rect.expand(1.0).contains_rect(f.rect)
            && f.rect.width() >= rect.width() / 2.0
    }) else {
        return;
    };
    let names: Vec<String> = children.iter().map(|c| name_of(ctx, &tree.tiles, *c, registry)).collect();
    let label = say(ctx, Word::DockTabs, &[("panels", &names.join(", "))]);
    ctx.accesskit_node_builder(background.id, |node| {
        node.set_role(Role::TabList);
        node.set_label(label);
    });
    if !ctx.memory(|m| m.has_focus(background.id)) {
        return;
    }
    keep_arrows(ctx, background.id);
    if focus::visible(ctx) {
        draw_ring(ui.painter(), &ds(ctx).theme, background.rect, 0, RingPlacement::Inside);
    }
    let step = [(Key::ArrowLeft, -1_isize), (Key::ArrowRight, 1)]
        .into_iter()
        .filter(|(key, _)| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, *key)))
        .map(|(_, step)| step)
        .sum::<isize>();
    if step == 0 || children.is_empty() {
        return;
    }
    if let Some(Tile::Container(Container::Tabs(tabs))) = tree.tiles.get_mut(container) {
        let at = tabs.active.and_then(|a| children.iter().position(|c| *c == a)).unwrap_or(0);
        let len = isize::try_from(children.len()).unwrap_or(isize::MAX);
        let next = usize::try_from((isize::try_from(at).unwrap_or(0) + step).rem_euclid(len)).unwrap_or(0);
        tabs.active = children.get(next).copied();
    }
}

/// The arrow keys stay with the focused handle instead of moving the keyboard to a neighbouring widget.
fn keep_arrows(ctx: &Context, id: Id) {
    let filter = EventFilter { horizontal_arrows: true, vertical_arrows: true, ..EventFilter::default() };
    ctx.memory_mut(|m| m.set_focus_lock_filter(id, filter));
}
