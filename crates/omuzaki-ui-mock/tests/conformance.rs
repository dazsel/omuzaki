//! The conformance check names every way a panel breaks the shell's rules (its own test binary, so this broken
//! panel never reaches another test's registry).

use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::Actor;
use omuzaki_ops_mock::MockRunner;
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use omuzaki_ui_api::layout::Node;
use omuzaki_ui_api::registry::{Group, Region};
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

/// Panics even at its minimum size.
struct Tiny;

impl Panel for Tiny {
    fn ui(&mut self, _ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        panic!("never fits");
    }
}

/// Overrides its tab title with nothing, behind a label that is fine.
struct BlankTitle;

impl Panel for BlankTitle {
    fn ui(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.label("Blank title");
    }

    fn title(&self) -> Option<Cow<'static, str>> {
        Some(Cow::Borrowed(""))
    }

    fn accessibility_label(&self) -> Option<Cow<'static, str>> {
        Some(Cow::Borrowed("Named for screen readers"))
    }
}

/// Overrides its screen-reader label with blanks.
struct BlankLabel;

impl Panel for BlankLabel {
    fn ui(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.label("Blank label");
    }

    fn accessibility_label(&self) -> Option<Cow<'static, str>> {
        Some(Cow::Borrowed("  "))
    }
}

/// Asks egui to discard its first pass, so egui draws it twice in one frame.
struct Discards;

/// How often [`Discards`] drew; the test reads it, because the host holds the panel as `dyn Panel`.
static DISCARD_CALLS: AtomicU32 = AtomicU32::new(0);

impl Panel for Discards {
    fn ui(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        if DISCARD_CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            ui.ctx().request_discard("test");
        }
        ui.label("Discards");
    }
}

/// Sets itself up on its first draw, and panics only when that first draw is narrow.
#[derive(Default)]
struct LateInit {
    ready: bool,
}

impl Panel for LateInit {
    fn ui(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        if !self.ready {
            assert!(ui.available_width() >= 200.0, "first draw too narrow");
            self.ready = true;
        }
        ui.label("Late init");
    }
}

/// Whether an [`Exclusive`] panel is alive: the stand-in for a resource only one instance can hold (a camera, a port).
static EXCLUSIVE_TAKEN: AtomicBool = AtomicBool::new(false);

/// Holds the exclusive token for its lifetime, as a panel that owns a device does; a second live instance can't build.
struct Exclusive;

impl Exclusive {
    fn take() -> Self {
        assert!(!EXCLUSIVE_TAKEN.swap(true, Ordering::SeqCst), "the token is held by another instance");
        Self
    }
}

impl Drop for Exclusive {
    fn drop(&mut self) {
        EXCLUSIVE_TAKEN.store(false, Ordering::SeqCst);
    }
}

impl Panel for Exclusive {
    fn ui(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.label("Exclusive");
    }
}

/// Panics when asked for its tab title.
struct TitlePanics;

impl Panel for TitlePanics {
    fn ui(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.label("Title panics");
    }

    fn title(&self) -> Option<Cow<'static, str>> {
        panic!("no title")
    }
}

/// Panics when asked for its screen-reader label.
struct LabelPanics;

impl Panel for LabelPanics {
    fn ui(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.label("Label panics");
    }

    fn accessibility_label(&self) -> Option<Cow<'static, str>> {
        panic!("no label")
    }
}

/// A fine panel whose id two workspaces also use.
struct Shared;

impl Panel for Shared {
    fn ui(&mut self, ui: &mut egui::Ui, _ctx: &PanelCtx<'_>) {
        ui.label("Shared");
    }
}

omuzaki_ui_api::register_panel! {
    id: "tiny", title: "Tiny", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 4,
    build: |_| Box::new(Tiny),
}
omuzaki_ui_api::register_panel! {
    id: "blank-title", title: "Blank title", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 5,
    build: |_| Box::new(BlankTitle),
}
omuzaki_ui_api::register_panel! {
    id: "blank-label", title: "Blank label", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 6,
    build: |_| Box::new(BlankLabel),
}
omuzaki_ui_api::register_panel! {
    id: "discards", title: "Discards", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 7,
    build: |_| Box::new(Discards),
}
omuzaki_ui_api::register_panel! {
    id: "bad-build", title: "Bad build", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 10,
    build: |_| panic!("cannot build"),
}
omuzaki_ui_api::register_panel! {
    id: "late-init", title: "Late init", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 9,
    build: |_| Box::new(LateInit::default()),
}
omuzaki_ui_api::register_panel! {
    id: "title-panics", title: "Title panics", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 12,
    build: |_| Box::new(TitlePanics),
}
omuzaki_ui_api::register_panel! {
    id: "label-panics", title: "Label panics", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 13,
    build: |_| Box::new(LabelPanics),
}
omuzaki_ui_api::register_panel! {
    id: "exclusive", title: "Exclusive", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 11,
    build: |_| Box::new(Exclusive::take()),
}
omuzaki_ui_api::register_panel! {
    id: "shared", title: "Shared", icon: "", region: Region::Left, min_size: [240.0, 160.0], order: 8,
    build: |_| Box::new(Shared),
}
omuzaki_ui_api::register_workspace! {
    id: "shared", title: "One", icon: "", order: 1, group: Group::Make, default_layout: || Node::pane("shared"),
}
omuzaki_ui_api::register_workspace! {
    id: "shared", title: "Two", icon: "", order: 2, group: Group::Make, default_layout: || Node::pane("shared"),
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
        "Broken Panel: the title is empty",
        "Broken Panel: unavailable with no document, and registers no empty state",
    ];
    assert_eq!(broken[..3], expected, "{broken:#?}");
    // The generic notice the host draws is announced, so only the three rules above fail.
    assert_eq!(broken.len(), 3, "{broken:#?}");

    let narrow = host.check("narrow", &ctx);
    assert!(narrow[0].starts_with("panel \"narrow\" is registered more than once"), "{narrow:#?}");
    assert_eq!(narrow[1..], ["narrow: panics at 120 by 80 points", "narrow: panics at 1 by 1 points"], "{narrow:#?}");

    assert_eq!(host.check("missing", &ctx), ["no panel is registered as \"missing\""]);

    // The minimum-size draw is guarded too: all three sizes are named.
    assert_eq!(
        host.check("tiny", &ctx),
        ["tiny: panics at 240 by 160 points", "tiny: panics at 120 by 80 points", "tiny: panics at 1 by 1 points"]
    );
}

#[test]
fn the_title_and_the_screen_reader_label_are_checked_apart() {
    let runner = MockRunner::new(MemDoc::new());
    let actor = Actor::System;
    let ctx = PanelCtx::new(&runner, &actor);
    let host = MockHost::new();
    assert_eq!(host.check("blank-title", &ctx), ["blank-title: the title is empty"]);
    assert_eq!(host.check("blank-label", &ctx), ["blank-label: the screen-reader label is empty"]);
}

#[test]
fn a_workspace_sharing_a_panels_id_does_not_fail_the_panel() {
    let runner = MockRunner::new(MemDoc::new());
    let actor = Actor::System;
    let ctx = PanelCtx::new(&runner, &actor);
    let host = MockHost::new();
    assert!(host.registry().duplicates().iter().any(|d| d.id == "shared"), "the workspaces do repeat the id");
    assert_eq!(host.check("shared", &ctx), Vec::<String>::new());
}

#[test]
fn ui_runs_again_when_egui_discards_a_pass() {
    let runner = MockRunner::new(MemDoc::new());
    let actor = Actor::System;
    let ctx = PanelCtx::new(&runner, &actor);
    let host = MockHost::new();
    let Some((def, mut panel)) = host.open("discards", &ctx) else { panic!("registered above") };
    assert!(host.frame(panel.as_mut(), def, &ctx, def.min_size).body);
    // The host keeps egui's default passes, as the shell does: the discarded pass draws the panel a second time.
    assert_eq!(DISCARD_CALLS.load(Ordering::SeqCst), 2, "ui runs once per pass, and egui ran two");
}

#[test]
fn every_size_is_tried_on_a_freshly_built_panel() {
    let runner = MockRunner::new(MemDoc::new());
    let actor = Actor::System;
    let ctx = PanelCtx::new(&runner, &actor);
    let host = MockHost::new();
    assert_eq!(
        host.check("late-init", &ctx),
        ["late-init: panics at 120 by 80 points", "late-init: panics at 1 by 1 points"]
    );
}

#[test]
fn a_build_that_panics_is_a_named_problem_and_never_a_panic() {
    let runner = MockRunner::new(MemDoc::new());
    let actor = Actor::System;
    let ctx = PanelCtx::new(&runner, &actor);
    let host = MockHost::new();
    assert_eq!(host.check("bad-build", &ctx), ["bad-build: its build panics"]);
}

#[test]
fn a_panel_holding_an_exclusive_resource_passes_because_one_instance_lives_at_a_time() {
    let runner = MockRunner::new(MemDoc::new());
    let actor = Actor::System;
    let ctx = PanelCtx::new(&runner, &actor);
    let host = MockHost::new();
    assert_eq!(host.check("exclusive", &ctx), Vec::<String>::new());
    assert!(!EXCLUSIVE_TAKEN.load(Ordering::SeqCst), "check leaves no instance alive");
}

#[test]
fn a_title_or_label_that_panics_is_a_named_problem_and_never_a_panic() {
    let runner = MockRunner::new(MemDoc::new());
    let actor = Actor::System;
    let ctx = PanelCtx::new(&runner, &actor);
    let host = MockHost::new();
    assert_eq!(host.check("title-panics", &ctx), ["title-panics: its title panics"]);
    assert_eq!(host.check("label-panics", &ctx), ["label-panics: its screen-reader label panics"]);
}
