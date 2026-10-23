//! The Generated tray as a panel (`shell.md` section 3): a tab beside the Library in the Build workspace, and in any
//! workspace from the View menu.

use omuzaki_ui_api::registry::Region;

use super::tray::TrayPanel;
use crate::ds::icons;

/// The tray's id.
pub const TRAY_PANEL: &str = "generated";

omuzaki_ui_api::register_panel! {
    id: "generated", title: "Generated", icon: icons::SPARKLE, region: Region::Left, min_size: [240.0, 200.0],
    order: 25, build: |_| Box::new(TrayPanel::default()),
}
