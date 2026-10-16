//! The Live panel, the Perform workspace and the Live story, registered with `omuzaki-ui-api` (`shell.md` sections 3
//! and 12), so the shell and the gallery show them once they read that registry.
//!
//! The panel reads the output window from the shell's view models ([`SharedOutput`]); a shell that provides none
//! gets the toggle disabled with its reason, as a build without an output window does.

use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::{Group, Region, StoryGroup};

use super::panel::{LivePanel, SharedOutput};
use super::story::{self, StoryState};
use crate::ds::icons;

/// The Live panel's id.
pub const LIVE_PANEL: &str = "live";
/// The Perform workspace's id.
pub const PERFORM_WORKSPACE: &str = "perform";
/// The shell's viewport panel, the Perform workspace's centre.
const VIEWPORT: &str = "viewport";

/// [`LivePanel`] as a registered panel.
pub struct LiveUiPanel(LivePanel);

impl Panel for LiveUiPanel {
    fn ui(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        // Looked up each frame, so a window the shell makes after the panel is built still reaches the toggle.
        self.0.set_output(ctx.view::<SharedOutput>().cloned());
        self.0.ui(ui, ctx.runner(), ctx.actor());
    }
}

omuzaki_ui_api::register_panel! {
    id: "live", title: "Live", icon: icons::PLAY, region: Region::Right, min_size: [288.0, 160.0], order: 10,
    build: |ctx| Box::new(LiveUiPanel(LivePanel::new(ctx.runner(), ctx.view::<SharedOutput>().cloned()))),
}

/// The Perform workspace's default layout: the viewport, and the Live panel in the right dock.
fn perform_layout() -> Node {
    Node::split(Dir::Horizontal, vec![Node::pane(VIEWPORT), Node::pane(LIVE_PANEL)], &[1060.0, 306.0])
}

omuzaki_ui_api::register_workspace! {
    id: "perform", title: "Perform", icon: icons::PLAY, order: 10, group: Group::Run,
    default_layout: perform_layout,
}

omuzaki_ui_api::register_story! {
    id: "live-workspace", title: "Live workspace", group: StoryGroup::Composites, order: 90, state: StoryState,
    show: story::show,
}
