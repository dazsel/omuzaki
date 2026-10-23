//! The Live panel, the Perform workspace and the Live story, registered with `omuzaki-ui-api` (`shell.md` sections 3
//! and 12), so the shell and the gallery show them once they read that registry.
//!
//! The panel needs nothing from the shell but its runner: the output window, the performer and the input are reached
//! through their ops, so a shell (or a build) without them gets each control disabled with its reason.

use omuzaki_metrics_api::MetricsReader;
use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::{Group, Region, StoryGroup};

use super::channels::{ChannelFeed, ChannelsPanel};
use super::health::{self, HealthPanel, HealthStory};
use super::panel::LivePanel;
use super::story::{self, StoryState};
use super::strip::ExpressionStrip;
use super::viewport::StagePanel;
use crate::ds::icons;

/// The Live panel's id.
pub const LIVE_PANEL: &str = "live";
/// The Perform workspace's id.
pub const PERFORM_WORKSPACE: &str = "perform";
/// The Channels panel's id: the Perform workspace's right column, every channel's meter and owner.
pub const CHANNELS_PANEL: &str = "live-channels";
/// The stage panel's id: the Perform workspace's centre, which draws the program frame the live viewport renders.
pub const STAGE_PANEL: &str = "live-stage";
/// The Health panel's id: a tab beside the Channels, the live-metrics snapshot (#1348).
pub const HEALTH_PANEL: &str = "live-health";

/// [`LivePanel`] as a registered panel, its health strip reading the app's [`MetricsReader`] when the shell has one.
pub struct LiveUiPanel(LivePanel);

impl Panel for LiveUiPanel {
    fn ui(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        if let (None, Some(reader)) = (self.0.metrics(), ctx.view::<MetricsReader>()) {
            self.0.set_metrics(reader.clone());
        }
        self.0.ui(ui, ctx.runner(), ctx.actor());
    }
}

/// [`HealthPanel`] as a registered panel, reading the app's [`MetricsReader`].
pub struct HealthUiPanel(HealthPanel);

impl Panel for HealthUiPanel {
    fn ui(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        egui::ScrollArea::vertical().id_salt("live.health").auto_shrink([false, false]).show(ui, |ui| {
            self.0.ui(ui, ctx.view::<MetricsReader>());
        });
    }
}

/// [`StagePanel`] as a registered panel, with the expression strip along its foot.
pub struct StageUiPanel(StagePanel, ExpressionStrip);

impl Panel for StageUiPanel {
    fn ui(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        let stage = ui.available_rect_before_wrap();
        self.0.ui(ui);
        self.1.ui(ui, stage, ctx.runner(), ctx.actor());
    }
}

/// [`ChannelsPanel`] as a registered panel, reading the app's [`ChannelFeed`] when the shell has one.
pub struct ChannelsUiPanel(ChannelsPanel);

impl Panel for ChannelsUiPanel {
    fn ui(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        self.0.ui(ui, ctx.view::<ChannelFeed>());
    }
}

omuzaki_ui_api::register_panel! {
    id: "live-channels", title: "Channels", icon: icons::GAUGE, region: Region::Right, min_size: [260.0, 200.0],
    order: 20, build: |_| Box::new(ChannelsUiPanel(ChannelsPanel::new())),
}

omuzaki_ui_api::register_panel! {
    id: "live-health", title: "Health", icon: icons::HEARTBEAT, region: Region::Right, min_size: [260.0, 200.0],
    order: 30, build: |_| Box::new(HealthUiPanel(HealthPanel)),
}

omuzaki_ui_api::register_panel! {
    id: "live-stage", title: "Stage", icon: icons::CUBE, region: Region::Center, min_size: [480.0, 320.0], order: 10,
    kind: omuzaki_ui_api::registry::PanelKind::Primary,
    build: |_| Box::new(StageUiPanel(StagePanel, ExpressionStrip::new())),
}

omuzaki_ui_api::register_panel! {
    id: "live", title: "Live", icon: icons::PLAY, region: Region::Right, min_size: [288.0, 160.0], order: 10,
    build: |ctx| Box::new(LiveUiPanel(LivePanel::new(ctx.runner()))),
}

/// The Perform workspace's default layout (`shell.md` section 12): the inputs and their controls (the Live panel) on
/// the left, the stage in the middle, the channels on the right with Health and Input in tabs beside them.
fn perform_layout() -> Node {
    Node::split(
        Dir::Horizontal,
        vec![
            Node::pane(LIVE_PANEL),
            Node::pane(STAGE_PANEL),
            Node::tabs(&[CHANNELS_PANEL, HEALTH_PANEL, crate::input::registration::INPUT_PANEL]),
        ],
        &[300.0, 746.0, 320.0],
    )
}

omuzaki_ui_api::register_workspace! {
    id: "perform", title: "Perform", icon: icons::PLAY, order: 10, group: Group::Run,
    default_layout: perform_layout,
}

omuzaki_ui_api::register_story! {
    id: "live-health", title: "Health", group: StoryGroup::Composites, order: 91, state: HealthStory,
    show: health::story,
}

omuzaki_ui_api::register_story! {
    id: "live-workspace", title: "Live workspace", group: StoryGroup::Composites, order: 90, state: StoryState,
    show: story::show,
}
