//! The panels the shell ships with, and the Build workspace that arranges them (`shell.md` sections 2, 3 and 13).
//!
//! Every body is its empty state until the panel's own task lands: a headline, one line of why, and no action that
//! would do nothing. A panel never closes itself; it says why it is empty.

use egui::{RichText, ScrollArea, Ui};
use omuzaki_ops_api::OpRecord;

use super::layout::{Dir, Layout, Node};
use super::registry::{Group, PanelDef, PanelKind, Region};
use super::text::{Word, say};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::widgets::EmptyState;
use crate::{register_panel, register_workspace};

register_panel! { id: "outliner", title: "Outliner", icon: icons::TREE_STRUCTURE, region: Region::Left,
min_size: [240.0, 160.0], order: 10, kind: PanelKind::Dockable }
register_panel! { id: "library", title: "Library", icon: icons::FOLDER, region: Region::Left,
min_size: [240.0, 160.0], order: 20, kind: PanelKind::Dockable }
register_panel! { id: "viewport", title: "Viewport", icon: icons::CUBE, region: Region::Center,
min_size: [480.0, 320.0], order: 10, kind: PanelKind::Primary }
register_panel! { id: "inspector", title: "Inspector", icon: icons::SLIDERS_HORIZONTAL, region: Region::Right,
min_size: [288.0, 160.0], order: 10, kind: PanelKind::Dockable }
register_panel! { id: "agent", title: "Agent", icon: icons::ROBOT, region: Region::Right,
min_size: [288.0, 160.0], order: 20, kind: PanelKind::Dockable }
register_panel! { id: "history", title: "History", icon: icons::CLOCK, region: Region::Right,
min_size: [288.0, 160.0], order: 30, kind: PanelKind::Dockable }
register_workspace! { id: "build", title: "Build", icon: icons::CUBE, order: 20, group: Group::Make,
layout: build_layout }

/// The Build workspace's default layout (`wireframes/build-1920.svg`): Outliner and Library on the left, the viewport,
/// and the Inspector over Agent and History on the right. The shares are the dock widths of the widest class; the shell
/// narrows them for smaller windows before the first frame.
fn build_layout() -> Layout {
    Layout::new(Node::split(
        Dir::Horizontal,
        vec![
            Node::tabs(&["outliner", "library"]),
            Node::pane("viewport"),
            Node::split(Dir::Vertical, vec![Node::pane("inspector"), Node::tabs(&["agent", "history"])], &[2.0, 1.0]),
        ],
        &[300.0, 1260.0, 360.0],
    ))
}

/// What a panel body reads: the shell's view of the runner, refreshed when something changes.
#[derive(Clone, Copy, Debug)]
pub struct Env<'a> {
    /// The op log, oldest first; empty for a new project.
    pub history: &'a [OpRecord],
    /// Why the log could not be read, when it could not.
    pub history_error: Option<&'a str>,
}

/// The empty state text of a panel: its headline and the line saying why.
fn copy(panel: &str) -> Option<(Word, Word)> {
    match panel {
        "outliner" => Some((Word::OutlinerHeadline, Word::OutlinerLine)),
        "library" => Some((Word::LibraryHeadline, Word::LibraryLine)),
        "viewport" => Some((Word::ViewportHeadline, Word::ViewportLine)),
        "inspector" => Some((Word::InspectorHeadline, Word::InspectorLine)),
        "history" => Some((Word::HistoryHeadline, Word::HistoryLine)),
        "agent" => Some((Word::AgentHeadline, Word::AgentLine)),
        _ => None,
    }
}

/// Draws the body of `def`.
pub fn body(ui: &mut Ui, def: &PanelDef, env: &Env<'_>) {
    if def.id == "history" && !env.history.is_empty() {
        return history(ui, env.history);
    }
    let ctx = ui.ctx().clone();
    if def.id == "history"
        && let Some(why) = env.history_error
    {
        let line = say(&ctx, Word::HistoryUnavailable, &[("why", why)]);
        ui.label(RichText::new(line).color(ds(&ctx).theme.text2));
        return;
    }
    let title = super::text::title(&ctx, def.title);
    let (headline, line) = match copy(def.id) {
        Some((headline, line)) => (say(&ctx, headline, &[]), say(&ctx, line, &[])),
        None => (title.to_string(), say(&ctx, Word::PanelNoView, &[("panel", &title)])),
    };
    ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        EmptyState::new(def.icon, &headline, &line).show(ui);
    });
}

/// The op log as a list, newest first. Rows are one fixed height, so only the visible ones are built.
fn history(ui: &mut Ui, records: &[OpRecord]) {
    let ds = ds(ui.ctx());
    let row = ds.metrics.tree_row;
    ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, row, records.len(), |ui, rows| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for index in rows {
            // Newest on top: row 0 is the last record.
            if let Some(record) = records.len().checked_sub(index + 1).and_then(|i| records.get(i)) {
                ui.label(RichText::new(format!("{}  {}", record.seq, record.op)).font(ds.text.mono.clone()));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::super::registry;
    use super::*;

    #[test]
    fn every_shipped_panel_has_its_own_empty_state_and_the_layout_uses_only_registered_panels() {
        for panel in build_layout().panels() {
            let def = registry::panel(panel).expect("registered");
            assert!(copy(def.id).is_some(), "{panel} has no empty state");
        }
        let primary: Vec<_> = registry::panels().into_iter().filter(|p| p.kind == PanelKind::Primary).collect();
        assert_eq!(primary.len(), 1, "one primary panel per workspace");
    }

    #[test]
    fn the_build_workspace_is_registered_in_the_make_group() {
        let build = registry::workspace("build").expect("registered");
        assert_eq!(build.group, Group::Make);
        assert!((build.layout)().contains("viewport"));
    }
}
