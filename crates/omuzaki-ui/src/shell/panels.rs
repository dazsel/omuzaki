//! The panels the shell ships with, and the Build workspace that arranges them (`shell.md` sections 2, 3 and 13). The
//! edit panels (outliner, viewport, inspector) register next to their own code in `crate::panels`.
//!
//! A body with nothing to show is its empty state: a headline, one line of why, and no action that would do nothing.
//! A panel never closes itself; it says why it is empty.

use egui::{RichText, ScrollArea, Ui};
use omuzaki_ops_api::OpRecord;
use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::{Group, Region};

use super::text::{Word, say};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::widgets::EmptyState;

omuzaki_ui_api::register_panel! { id: "library", title: "Library", icon: icons::FOLDER, region: Region::Left,
min_size: [240.0, 160.0], order: 20, build: |_| placeholder(icons::FOLDER, Word::LibraryHeadline, Word::LibraryLine) }
omuzaki_ui_api::register_panel! { id: "agent", title: "Agent", icon: icons::ROBOT, region: Region::Right,
min_size: [288.0, 160.0], order: 20, build: |_| placeholder(icons::ROBOT, Word::AgentHeadline, Word::AgentLine) }
omuzaki_ui_api::register_panel! { id: "history", title: "History", icon: icons::CLOCK, region: Region::Right,
min_size: [288.0, 160.0], order: 30, build: |_| Box::new(History) }
omuzaki_ui_api::register_workspace! { id: "build", title: "Build", icon: icons::CUBE, order: 20, group: Group::Make,
default_layout: build_layout }

/// The Build workspace's default layout (`wireframes/build-1920.svg`): Outliner and Library on the left, the viewport,
/// and the Inspector over Agent and History on the right. The shares are the dock widths of the widest class; the shell
/// narrows them for smaller windows before the first frame.
fn build_layout() -> Node {
    Node::split(
        Dir::Horizontal,
        vec![
            Node::tabs(&["outliner", "library"]),
            Node::pane("viewport"),
            Node::split(Dir::Vertical, vec![Node::pane("inspector"), Node::tabs(&["agent", "history"])], &[2.0, 1.0]),
        ],
        &[300.0, 1260.0, 360.0],
    )
}

/// The op log as the shell last read it: the view model the History panel reads from its context
/// ([`PanelCtx::view`]), refreshed when something changes.
#[derive(Clone, Debug, Default)]
pub struct HistoryLog {
    /// The op log, oldest first; empty for a new project.
    pub records: Vec<OpRecord>,
    /// Why the log could not be read, when it could not.
    pub error: Option<String>,
}

/// A panel with nothing to show yet: its headline and the line saying why. A panel never closes itself; it says why
/// it is empty.
struct Placeholder {
    icon: &'static str,
    headline: Word,
    line: Word,
}

fn placeholder(icon: &'static str, headline: Word, line: Word) -> Box<dyn Panel> {
    Box::new(Placeholder { icon, headline, line })
}

impl Panel for Placeholder {
    fn ui(&mut self, ui: &mut Ui, _ctx: &PanelCtx<'_>) {
        empty(ui, self.icon, self.headline, self.line);
    }
}

/// The History panel.
struct History;

impl Panel for History {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        history(ui, ctx.view::<HistoryLog>());
    }
}

/// A panel body that is only its empty state.
fn empty(ui: &mut Ui, icon: &str, headline: Word, line: Word) {
    let ctx = ui.ctx().clone();
    let (headline, line) = (say(&ctx, headline, &[]), say(&ctx, line, &[]));
    ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        EmptyState::new(icon, &headline, &line).show(ui);
    });
}

/// The op log when there is one, the reason it cannot be read, or the empty state.
fn history(ui: &mut Ui, log: Option<&HistoryLog>) {
    if let Some(log) = log.filter(|log| !log.records.is_empty()) {
        return history_list(ui, &log.records);
    }
    let ctx = ui.ctx().clone();
    if let Some(why) = log.and_then(|log| log.error.as_deref()) {
        let line = say(&ctx, Word::HistoryUnavailable, &[("why", why)]);
        ui.label(RichText::new(line).color(ds(&ctx).theme.text2));
    } else {
        empty(ui, icons::CLOCK, Word::HistoryHeadline, Word::HistoryLine);
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
    use omuzaki_ui_api::registry::{PanelKind, Registry};

    use super::super::layout::Layout;
    use super::*;

    #[test]
    fn every_shipped_panel_registers_itself_and_the_layout_uses_only_registered_panels() {
        let registry = Registry::collect();
        for panel in Layout::from_registered(&build_layout(), &|_| true).expect("a layout").panels() {
            assert!(registry.panel(panel).is_some(), "{panel} is not registered");
        }
        let layout = Layout::from_registered(&build_layout(), &|_| true).expect("a layout");
        let primary: Vec<_> = layout
            .panels()
            .into_iter()
            .filter(|p| registry.panel(p).is_some_and(|def| def.kind == PanelKind::Primary))
            .collect();
        assert_eq!(primary, ["viewport"], "one primary panel per workspace");
    }

    #[test]
    fn the_build_workspace_is_registered_in_the_make_group() {
        let registry = Registry::collect();
        let build = registry.workspace("build").expect("registered");
        assert_eq!(build.group, Group::Make);
        assert!(Layout::from_registered(&(build.default_layout)(), &|_| true).expect("a layout").contains("viewport"));
    }
}
