//! The automation panels and the Broadcast workspace, registered with `omuzaki-ui-api` (`shell.md` section 3): the
//! Rules table on the right, the graph editor and the values monitor as tabs in the centre.
//!
//! All three are centre panels: shown from another workspace they open as tabs beside its primary panel, where a wide
//! table has room, rather than crowding the right dock's tab strip.
//!
//! The panels need only the shell's runner to work; the approval card's Allow and its secret field use the app's
//! [`ShowApprover`] and keychain when the shell has them, and say why they can't when it doesn't.

use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::{Group, Region};

use super::graph::GraphPanel;
use super::rules::RulesPanel;
use super::values::ValuesPanel;
use super::{ShowApprover, ShowFocus};
use crate::ds::icons;
use crate::panels::io::Io;
use crate::settings::connectors::Keychain;
use crate::shell::requests::ShellRequests;

/// The Rules panel's id.
pub const RULES_PANEL: &str = "broadcast-rules";
/// The graph editor's id.
pub const GRAPH_PANEL: &str = "broadcast-graph";
/// The values monitor's id.
pub const VALUES_PANEL: &str = "broadcast-values";
/// The Broadcast workspace's id.
pub const BROADCAST_WORKSPACE: &str = "broadcast";

/// [`RulesPanel`] as a registered panel.
pub struct RulesUiPanel(RulesPanel);

impl Panel for RulesUiPanel {
    fn ui(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        let focus = ShowFocus::from_ctx(ctx);
        let approver = ctx.view::<ShowApprover>();
        let keychain = ctx.view::<Keychain>();
        let requests = ctx.view::<ShellRequests>();
        self.0.ui(ui, Io::from_ctx(ctx), &focus, approver, keychain, requests);
    }
}

/// [`GraphPanel`] as a registered panel.
pub struct GraphUiPanel(GraphPanel);

impl Panel for GraphUiPanel {
    fn ui(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        self.0.ui(ui, Io::from_ctx(ctx), &ShowFocus::from_ctx(ctx));
    }
}

/// [`ValuesPanel`] as a registered panel.
pub struct ValuesUiPanel(ValuesPanel);

impl Panel for ValuesUiPanel {
    fn ui(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        self.0.ui(ui, Io::from_ctx(ctx));
    }
}

omuzaki_ui_api::register_panel! {
    id: "broadcast-rules", title: "Rules", icon: icons::LIGHTNING, region: Region::Center, min_size: [300.0, 240.0],
    order: 32, build: |_| Box::new(RulesUiPanel(RulesPanel::new())),
}

omuzaki_ui_api::register_panel! {
    id: "broadcast-graph", title: "Graph", icon: icons::TREE_STRUCTURE, region: Region::Center,
    min_size: [480.0, 320.0], order: 30, build: |_| Box::new(GraphUiPanel(GraphPanel::new())),
}

omuzaki_ui_api::register_panel! {
    id: "broadcast-values", title: "Values", icon: icons::ACTIVITY, region: Region::Center, min_size: [360.0, 240.0],
    order: 31, build: |_| Box::new(ValuesUiPanel(ValuesPanel::new())),
}

/// The Broadcast workspace's default layout (`shell.md` section 3): the graph editor and the values monitor as tabs in
/// the centre, the Rules table on the right.
fn broadcast_layout() -> Node {
    Node::split(
        Dir::Horizontal,
        vec![Node::tabs(&[GRAPH_PANEL, VALUES_PANEL]), Node::pane(RULES_PANEL)],
        &[1000.0, 366.0],
    )
}

omuzaki_ui_api::register_workspace! {
    id: "broadcast", title: "Broadcast", icon: icons::BROADCAST, order: 30, group: Group::Run,
    default_layout: broadcast_layout,
}
