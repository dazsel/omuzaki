//! The Script workspace and its panels, registered with `omuzaki-ui-api` (`scripting.md` section 8, `shell.md` row
//! "Script console"): Scripts on the left, the Console in the centre, Macros and the Ops reference as tabs on the
//! right. Each panel also opens on its own in any workspace from the View menu.

use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::registry::{Group, Region};

use super::console::ConsolePanel;
use super::macros::MacrosPanel;
use super::ops_ref::OpsPanel;
use super::scripts::ScriptsPanel;
use crate::ds::icons;

/// The Scripts list's id.
pub const SCRIPTS_PANEL: &str = "script-scripts";
/// The Console's id.
pub const CONSOLE_PANEL: &str = "script-console";
/// The Macros panel's id.
pub const MACROS_PANEL: &str = "script-macros";
/// The Ops reference's id.
pub const OPS_PANEL: &str = "script-ops";
/// The Script workspace's id.
pub const SCRIPT_WORKSPACE: &str = "script";

omuzaki_ui_api::register_panel! {
    id: "script-scripts", title: "Scripts", icon: icons::FILE_CODE, region: Region::Left, min_size: [220.0, 200.0],
    order: 50, build: |_| Box::new(ScriptsPanel::default()),
}

omuzaki_ui_api::register_panel! {
    id: "script-console", title: "Console", icon: icons::TERMINAL_WINDOW, region: Region::Center,
    min_size: [480.0, 320.0], order: 50, build: |_| Box::new(ConsolePanel::default()),
}

omuzaki_ui_api::register_panel! {
    id: "script-macros", title: "Macros", icon: icons::RECORD, region: Region::Right, min_size: [300.0, 200.0],
    order: 50, build: |_| Box::new(MacrosPanel::default()),
}

omuzaki_ui_api::register_panel! {
    id: "script-ops", title: "Ops", icon: icons::BRACKETS_CURLY, region: Region::Right, min_size: [300.0, 200.0],
    order: 51, build: |_| Box::new(OpsPanel::default()),
}

/// The Script workspace's default layout: Scripts, the Console, and Macros with the Ops reference.
fn script_layout() -> Node {
    Node::split(
        Dir::Horizontal,
        vec![Node::pane(SCRIPTS_PANEL), Node::pane(CONSOLE_PANEL), Node::tabs(&[MACROS_PANEL, OPS_PANEL])],
        &[280.0, 1000.0, 400.0],
    )
}

omuzaki_ui_api::register_workspace! {
    id: "script", title: "Script", icon: icons::CODE, order: 95, group: Group::Make, default_layout: script_layout,
}
