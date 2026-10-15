//! Registrations from two crates (the mock's library and this test crate) meet in one registry without either
//! editing a shared file, in a deterministic order (#64).

use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Event, TxId};
use omuzaki_ops_mock::MockRunner;
use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::panel::{EmptyState, Panel, PanelCtx};
use omuzaki_ui_api::registry::{Group, PanelKind, Region, Registry};
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
    id: "test.notes", title: "Notes", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 900,
    build: |_| Box::new(ProbePanel::default()),
}
omuzaki_ui_api::register_workspace! {
    id: "test.sheet", title: "Sheet", icon: "", order: 900, group: Group::Make,
    default_layout: || Node::split(Dir::Horizontal, vec![Node::pane("test.notes"), Node::pane("test.canvas")], &[1.0, 3.0]),
}

#[test]
fn panels_and_workspaces_from_two_crates_are_listed_by_order_then_id() {
    omuzaki_ui_mock::link();
    let registry = Registry::collect();
    let panels: Vec<&str> = registry.panels().iter().map(|p| p.id).collect();
    assert_eq!(panels, ["test.canvas", PROBE, "test.notes"], "order 10 first; order 900 by id");
    let workspaces: Vec<&str> = registry.workspaces().iter().map(|w| w.id).collect();
    assert_eq!(workspaces, [PROBE, "test.sheet"]);
    assert!(registry.duplicates().is_empty(), "{:?}", registry.duplicates());
    let sheet = registry.workspace("test.sheet").map(|w| (w.default_layout)().panels().join(","));
    assert_eq!(sheet.as_deref(), Some("test.notes,test.canvas"));
}

#[test]
fn the_host_draws_a_registered_panel_or_its_empty_state() {
    let registry = Registry::collect();
    let (Some(probe_def), Some(canvas_def)) = (registry.panel(PROBE), registry.panel("test.canvas")) else {
        panic!("both panels are registered");
    };
    let runner = MockRunner::new(MemDoc::new());
    let actor = Actor::System;
    let events = [Event::tx_committed(TxId(1), "Edit", Actor::System)];
    let ctx = PanelCtx::new(&runner, &actor).with_events(&events);
    let host = MockHost::new();

    let mut probe = (probe_def.build)(&ctx);
    assert!(host.frame(probe.as_mut(), probe_def, &ctx).body);
    assert!(host.frame(probe.as_mut(), probe_def, &ctx).body);
    assert_eq!(probe.title().as_deref(), Some("Probe (2 frames)"));

    let mut canvas = (canvas_def.build)(&ctx);
    let frame = host.frame(canvas.as_mut(), canvas_def, &ctx);
    assert!(!frame.body);
    assert_eq!((canvas_def.kind, canvas_def.empty), (PanelKind::Primary, Some(NO_SHEET)));
    assert!(frame.shapes > 0, "the empty state is drawn");
}
