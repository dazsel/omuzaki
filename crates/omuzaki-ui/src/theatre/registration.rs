//! The Theatre's panels and its workspace, registered with `omuzaki-ui-api` (`shell.md` sections 3 and 11): the plan
//! board on the left, the stage over the compare strip in the centre, the steps on the right. Each panel also opens on
//! its own in any workspace from the View menu.

use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::registry::{Group, Region};

use super::compare::ComparePanel;
use super::plan::PlanPanel;
use super::stage::StagePanel;
use super::steps::StepsPanel;
use crate::ds::icons;
use crate::panels::edit::EditFocus;

/// The plan board's id.
pub const PLAN_PANEL: &str = "theatre-plan";
/// The stage's id.
pub const STAGE_PANEL: &str = "theatre-stage";
/// The compare strip's id.
pub const COMPARE_PANEL: &str = "theatre-compare";
/// The steps list's id.
pub const STEPS_PANEL: &str = "theatre-steps";
/// The Theatre workspace's id.
pub const THEATRE_WORKSPACE: &str = "theatre";

omuzaki_ui_api::register_panel! {
    id: "theatre-plan", title: "Plan", icon: icons::LIST_CHECKS, region: Region::Left, min_size: [240.0, 200.0],
    order: 40, build: |_| Box::new(PlanPanel::default()),
}

omuzaki_ui_api::register_panel! {
    id: "theatre-stage", title: "Stage", icon: icons::BINOCULARS, region: Region::Center, min_size: [480.0, 320.0],
    order: 40, build: |ctx| Box::new(StagePanel::new(EditFocus::from_ctx(ctx))),
}

omuzaki_ui_api::register_panel! {
    id: "theatre-compare", title: "Compare", icon: icons::COLUMNS, region: Region::Center, min_size: [360.0, 200.0],
    order: 41, build: |_| Box::new(ComparePanel::default()),
}

omuzaki_ui_api::register_panel! {
    id: "theatre-steps", title: "Steps", icon: icons::CARDS, region: Region::Right, min_size: [320.0, 200.0],
    order: 40, build: |_| Box::new(StepsPanel::default()),
}

/// The Theatre's default layout (`shell.md` section 11, `theatre-1920.svg`): the plan board, the stage over the compare
/// strip, and the steps.
fn theatre_layout() -> Node {
    Node::split(
        Dir::Horizontal,
        vec![
            Node::pane(PLAN_PANEL),
            Node::split(Dir::Vertical, vec![Node::pane(STAGE_PANEL), Node::pane(COMPARE_PANEL)], &[640.0, 300.0]),
            Node::pane(STEPS_PANEL),
        ],
        &[320.0, 1000.0, 420.0],
    )
}

omuzaki_ui_api::register_workspace! {
    id: "theatre", title: "Theatre", icon: icons::BINOCULARS, order: 90, group: Group::Watch,
    default_layout: theatre_layout,
}
