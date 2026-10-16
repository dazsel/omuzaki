//! Panels and workspaces from two crates (the mock's fixture and this test crate) appear in the mock host and
//! render, without either crate editing a shared file, in a deterministic order (#64, part 1).

use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Event, TxId};
use omuzaki_ops_mock::MockRunner;
use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::panel::{EmptyState, Panel, PanelCtx};
use omuzaki_ui_api::registry::{Group, PanelKind, Region};
use omuzaki_ui_mock::{MockHost, PROBE, ProbePanel};

/// The canvas of this crate: never available, so the host shows its empty state.
struct Canvas;

impl Panel for Canvas {
    fn ui(&mut self, _ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {}

    fn available(&self, _ctx: &PanelCtx<'_>) -> bool {
        false
    }
}

const NO_SHEET: EmptyState = EmptyState::new("No sheet is loaded", "Import a sheet");

omuzaki_ui_api::register_panel! {
    id: "test.canvas", title: "Canvas", icon: "", region: Region::Center, min_size: [480.0, 320.0], order: 10,
    kind: PanelKind::Primary, empty: NO_SHEET, build: |_| Box::new(Canvas),
}
omuzaki_ui_api::register_panel! {
    id: "test.notes", title: "Notes", icon: "", region: Region::Right, min_size: [288.0, 160.0], order: 900,
    build: |_| Box::new(ProbePanel::default()),
}
omuzaki_ui_api::register_workspace! {
    id: "test.sheet", title: "Sheet", icon: "", order: 900, group: Group::Make,
    default_layout: || Node::split(Dir::Horizontal, vec![Node::pane("test.notes"), Node::pane("test.canvas")], &[1.0, 3.0]),
}

#[test]
fn registrations_from_two_crates_appear_in_the_host_by_group_then_order_then_id() {
    omuzaki_ui_mock::link();
    let host = MockHost::new();
    let registry = host.registry();
    let panels: Vec<&str> = registry.panels().iter().map(|p| p.id).collect();
    assert_eq!(panels, ["test.canvas", PROBE, "test.notes"], "centre first; then the right region by id");
    let workspaces: Vec<&str> = registry.workspaces().iter().map(|w| w.id).collect();
    assert_eq!(workspaces, ["test.sheet", PROBE], "Make before Watch");
    assert!(registry.duplicates().is_empty(), "{:?}", registry.duplicates());
    let sheet = registry.workspace("test.sheet").map(|w| (w.default_layout)().panels().join(","));
    assert_eq!(sheet.as_deref(), Some("test.notes,test.canvas"));
}

#[test]
fn the_host_renders_both_crates_panels_and_each_passes_the_conformance_check() {
    let runner = MockRunner::new(MemDoc::new());
    let actor = Actor::System;
    let events = [Event::tx_committed(TxId(1), "Edit", Actor::System)];
    let ctx = PanelCtx::new(&runner, &actor).with_events(&events);
    let host = MockHost::new();

    let Some((probe_def, mut probe)) = host.open(PROBE, &ctx) else { panic!("the mock's fixture is registered") };
    for _ in 0..2 {
        let frame = host.frame(probe.as_mut(), probe_def, &ctx, [1920.0, 1080.0]);
        assert!(frame.body && frame.shapes > 0);
    }
    assert_eq!(probe.title().as_deref(), Some("Probe (2 frames)"));

    let Some((canvas_def, mut canvas)) = host.open("test.canvas", &ctx) else { panic!("this crate's canvas") };
    let frame = host.frame(canvas.as_mut(), canvas_def, &ctx, [1920.0, 1080.0]);
    assert!(!frame.body, "the canvas is unavailable");
    assert_eq!(frame.labels, ["Import a sheet", "No sheet is loaded"], "its empty state is drawn and announced");

    for id in [PROBE, "test.canvas", "test.notes"] {
        let problems = host.check(id, &ctx);
        assert!(problems.is_empty(), "{id}: {problems:#?}");
    }
}
