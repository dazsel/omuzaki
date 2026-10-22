//! The panels the shell ships with, and the Build workspace that arranges them (`shell.md` sections 2, 3 and 13). The
//! edit panels (outliner, viewport, inspector) register next to their own code in `crate::panels`.
//!
//! A body with nothing to show is its empty state: a headline, one line of why, and no action that would do nothing.
//! A panel never closes itself; it says why it is empty.

use egui::{ScrollArea, Ui};
use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::{Group, Region};

use super::text::{Word, say};
use crate::ds::icons;
use crate::ds::widgets::EmptyState;

omuzaki_ui_api::register_panel! { id: "library", title: "Library", icon: icons::FOLDER, region: Region::Left,
min_size: [240.0, 160.0], order: 20, build: |_| placeholder(icons::FOLDER, Word::LibraryHeadline, Word::LibraryLine) }
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

/// A panel body that is only its empty state.
fn empty(ui: &mut Ui, icon: &str, headline: Word, line: Word) {
    let ctx = ui.ctx().clone();
    let (headline, line) = (say(&ctx, headline, &[]), say(&ctx, line, &[]));
    ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        EmptyState::new(icon, &headline, &line).show(ui);
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
