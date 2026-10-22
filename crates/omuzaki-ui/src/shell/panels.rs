//! The panels the shell ships with, and the Build workspace that arranges them (`shell.md` sections 2, 3 and 13). The
//! edit panels (outliner, viewport, inspector) register next to their own code in `crate::panels`.
//!
//! A body with nothing to show is its empty state: a headline, one line of why, and no action that would do nothing.
//! A panel never closes itself; it says why it is empty.

use std::collections::HashSet;

use egui::{RichText, ScrollArea, Ui};
use omuzaki_ops_api::{Actor, Grants, OpRecord, TxId};
use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::{Group, Region};

use super::text::{Word, say};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::widgets::{Button, EmptyState};

omuzaki_ui_api::register_panel! { id: "library", title: "Library", icon: icons::FOLDER, region: Region::Left,
min_size: [240.0, 160.0], order: 20, build: |_| placeholder(icons::FOLDER, Word::LibraryHeadline, Word::LibraryLine) }
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
/// ([`PanelCtx::view`]), kept current from the runner's events.
#[derive(Clone, Debug, Default)]
pub struct HistoryLog {
    /// The op log, oldest first; empty for a new project.
    pub records: Vec<OpRecord>,
    /// The transactions undone and not redone, as the runner's events told them.
    pub undone: HashSet<TxId>,
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

/// The History panel (`shell.md` section 9): one list for the whole project, newest on top, each row saying who did
/// what; Undo and Redo above it act for the person, as Ctrl+Z does.
struct History;

impl Panel for History {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        let log = ctx.view::<HistoryLog>();
        if log.is_some_and(|log| !log.records.is_empty()) {
            undo_bar(ui, ctx);
        }
        history(ui, log);
    }
}

/// Undo and Redo for the person. The result reaches the list through the runner's events.
fn undo_bar(ui: &mut Ui, ctx: &PanelCtx<'_>) {
    let egui_ctx = ui.ctx().clone();
    ui.horizontal(|ui| {
        let (undo, redo) = (say(&egui_ctx, Word::HistoryUndo, &[]), say(&egui_ctx, Word::HistoryRedo, &[]));
        let grants = Grants::person();
        if ui.add(Button::new(&undo).ghost().icon(icons::ARROW_COUNTER_CLOCKWISE).shortcut_hint("Ctrl Z")).clicked()
            && let Err(error) = ctx.runner().undo(ctx.actor(), &grants)
        {
            tracing::warn!(%error, "undo failed");
        }
        if ui.add(Button::new(&redo).ghost().icon(icons::ARROW_CLOCKWISE).shortcut_hint("Ctrl Shift Z")).clicked()
            && let Err(error) = ctx.runner().redo(ctx.actor(), &grants)
        {
            tracing::warn!(%error, "redo failed");
        }
    });
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
        return history_list(ui, log);
    }
    let ctx = ui.ctx().clone();
    if let Some(why) = log.and_then(|log| log.error.as_deref()) {
        let line = say(&ctx, Word::HistoryUnavailable, &[("why", why)]);
        ui.label(RichText::new(line).color(ds(&ctx).theme.text2));
    } else {
        empty(ui, icons::CLOCK, Word::HistoryHeadline, Word::HistoryLine);
    }
}

/// The op log as a list, newest first: who, then what. An undone step is dimmed and says so. Rows are one fixed
/// height, so only the visible ones are built.
fn history_list(ui: &mut Ui, log: &HistoryLog) {
    let ctx = ui.ctx().clone();
    let ds = ds(&ctx);
    let row = ds.metrics.tree_row;
    let records = &log.records;
    ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, row, records.len(), |ui, rows| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for index in rows {
            // Newest on top: row 0 is the last record.
            let Some(record) = records.len().checked_sub(index + 1).and_then(|i| records.get(i)) else { continue };
            let undone = log.undone.contains(&record.tx);
            let colour = if undone { ds.theme.text3 } else { ds.theme.text };
            ui.horizontal(|ui| {
                let who = who(&ctx, &record.actor);
                let chip = if matches!(record.actor, Actor::Agent { .. }) { ds.theme.agent } else { ds.theme.text2 };
                ui.label(RichText::new(who).font(ds.text.small.clone()).color(chip));
                ui.label(RichText::new(what(record)).font(ds.text.mono.clone()).color(colour));
                if undone {
                    ui.label(
                        RichText::new(say(&ctx, Word::HistoryUndone, &[]))
                            .font(ds.text.small.clone())
                            .color(ds.theme.text3),
                    );
                }
            });
        }
    });
}

/// Who ran a step, as History names them.
fn who(ctx: &egui::Context, actor: &Actor) -> String {
    match actor {
        Actor::Person { name } if name == "you" => say(ctx, Word::HistoryYou, &[]),
        Actor::Person { name } => name.clone(),
        Actor::Agent { id, .. } | Actor::Plugin { id } => id.clone(),
        Actor::System => say(ctx, Word::HistorySystem, &[]),
    }
}

/// What a step did, in the op's own terms: its name, then the part and parameter it set when it names them
/// (`part.set_param head › cranium/half_width = 0.61`).
fn what(record: &OpRecord) -> String {
    let arg = |key: &str| record.args.get(key);
    let text = |key: &str| arg(key).and_then(|v| v.as_str());
    let mut line = record.op.clone();
    if let Some(part) = text("part") {
        line.push(' ');
        line.push_str(part);
    }
    if let Some(path) = text("path") {
        line.push_str(" \u{203a} ");
        line.push_str(path);
    }
    if let Some(value) = arg("value") {
        let shown = value.as_f64().map_or_else(|| value.to_string(), |n| format!("{n:.3}"));
        line.push_str(" = ");
        line.push_str(&shown);
    }
    line
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
