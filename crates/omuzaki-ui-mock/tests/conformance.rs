//! The conformance check names every way a panel breaks the shell's rules (its own test binary, so this broken
//! panel never reaches another test's registry).

use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::Actor;
use omuzaki_ops_mock::MockRunner;
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::Region;
use omuzaki_ui_mock::MockHost;

/// Unavailable without an empty state, silent to screen readers, and panics when squeezed.
struct Broken;

impl Panel for Broken {
    fn ui(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        assert!(ui.available_width() >= 100.0, "too narrow");
    }

    fn available(&self, _ctx: &PanelCtx<'_>) -> bool {
        false
    }
}

/// Draws a body that is fine at its minimum size and panics below 200 points of width.
struct Narrow;

impl Panel for Narrow {
    fn ui(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        assert!(ui.available_width() >= 200.0, "too narrow");
        ui.label("Narrow");
    }
}

omuzaki_ui_api::register_panel! {
    id: "Broken Panel", title: " ", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 1,
    build: |_| Box::new(Broken),
}
omuzaki_ui_api::register_panel! {
    id: "narrow", title: "Narrow", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 2,
    build: |_| Box::new(Narrow),
}
omuzaki_ui_api::register_panel! {
    id: "narrow", title: "Again", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 3,
    build: |_| Box::new(Narrow),
}

#[test]
fn every_rule_a_panel_breaks_is_named() {
    let runner = MockRunner::new(MemDoc::new());
    let actor = Actor::System;
    let ctx = PanelCtx::new(&runner, &actor);
    let host = MockHost::new();

    let broken = host.check("Broken Panel", &ctx);
    let expected = [
        "id \"Broken Panel\" is not lower-case ASCII letters, digits, '.', '_' or '-'",
        "Broken Panel: the title or the screen-reader label is empty",
        "Broken Panel: unavailable with no document, and registers no empty state",
    ];
    assert_eq!(broken[..3], expected, "{broken:#?}");
    // The generic notice the host draws is announced, so only the three rules above fail.
    assert_eq!(broken.len(), 3, "{broken:#?}");

    let narrow = host.check("narrow", &ctx);
    assert!(narrow[0].starts_with("panel \"narrow\" is registered more than once"), "{narrow:#?}");
    assert_eq!(narrow[1..], ["narrow: panics at 120 by 80 points", "narrow: panics at 1 by 1 points"], "{narrow:#?}");

    assert_eq!(host.check("missing", &ctx), ["no panel is registered as \"missing\""]);
}
