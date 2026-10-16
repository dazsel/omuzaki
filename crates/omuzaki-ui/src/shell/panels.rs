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

/// How a panel draws its body. Registered beside the panel (`register_panel_view!`), so adding a panel never edits a
/// central list.
pub struct PanelView {
    /// The registered panel it draws.
    pub id: &'static str,
    /// Draws the body.
    pub draw: fn(&mut Ui, &PanelDef, &Env<'_>),
}

inventory::collect!(PanelView);

/// Registers how a panel draws its body.
#[macro_export]
macro_rules! register_panel_view {
    (id: $id:literal, draw: $draw:expr $(,)?) => {
        ::inventory::submit! {
            $crate::shell::panels::PanelView { id: $id, draw: $draw }
        }
        $crate::__record_registration!("panel_view", $id);
    };
}

register_panel_view! { id: "outliner", draw: |ui, def, _| empty(ui, def, Word::OutlinerHeadline, Word::OutlinerLine) }
register_panel_view! { id: "library", draw: |ui, def, _| empty(ui, def, Word::LibraryHeadline, Word::LibraryLine) }
register_panel_view! { id: "viewport", draw: |ui, def, _| empty(ui, def, Word::ViewportHeadline, Word::ViewportLine) }
register_panel_view! { id: "inspector", draw: |ui, def, _| empty(ui, def, Word::InspectorHeadline, Word::InspectorLine) }
register_panel_view! { id: "agent", draw: |ui, def, _| empty(ui, def, Word::AgentHeadline, Word::AgentLine) }
register_panel_view! { id: "history", draw: history }

/// The view registered for `panel`.
fn view(panel: &str) -> Option<&'static PanelView> {
    inventory::iter::<PanelView>.into_iter().find(|v| v.id == panel)
}

/// Draws the body of `def`: its registered view, or a note that it has none.
pub fn body(ui: &mut Ui, def: &PanelDef, env: &Env<'_>) {
    if let Some(view) = view(def.id) {
        (view.draw)(ui, def, env);
        return;
    }
    let ctx = ui.ctx().clone();
    let title = super::text::title(&ctx, def.title);
    let line = say(&ctx, Word::PanelNoView, &[("panel", &title)]);
    ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        EmptyState::new(def.icon, &title, &line).show(ui);
    });
}

/// A panel with nothing to show yet: its headline and the line saying why.
fn empty(ui: &mut Ui, def: &PanelDef, headline: Word, line: Word) {
    let ctx = ui.ctx().clone();
    let (headline, line) = (say(&ctx, headline, &[]), say(&ctx, line, &[]));
    ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        EmptyState::new(def.icon, &headline, &line).show(ui);
    });
}

/// The History panel: the op log when there is one, the reason it cannot be read, or its empty state.
fn history(ui: &mut Ui, def: &PanelDef, env: &Env<'_>) {
    if !env.history.is_empty() {
        return history_list(ui, env.history);
    }
    let ctx = ui.ctx().clone();
    if let Some(why) = env.history_error {
        let line = say(&ctx, Word::HistoryUnavailable, &[("why", why)]);
        ui.label(RichText::new(line).color(ds(&ctx).theme.text2));
    } else {
        empty(ui, def, Word::HistoryHeadline, Word::HistoryLine);
    }
}

/// The op log as a list, newest first. Rows are one fixed height, so only the visible ones are built.
fn history_list(ui: &mut Ui, records: &[OpRecord]) {
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
    fn every_shipped_panel_registers_its_own_view_and_the_layout_uses_only_registered_panels() {
        for panel in build_layout().panels() {
            let def = registry::panel(panel).expect("registered");
            assert!(view(def.id).is_some(), "{panel} has no view");
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
