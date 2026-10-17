//! Registrations that go wrong are reported at startup and never settled silently (#2042): an id registered twice (the
//! shell reports workspaces and panels, the gallery stories), a default layout that names a panel nobody registered,
//! and one that holds nothing the shell can show. This crate registers all of them, so it has a binary of its own: the
//! other shell tests assert that nothing is registered twice.

use std::io;
use std::sync::{Arc, Mutex};

use egui::{Key, Modifiers, Ui, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::gallery::{self, Gallery, Group};
use omuzaki_ui::ds::testing::press;
use omuzaki_ui::shell::Shell;
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::{DefKind, Group as WorkspaceGroup, Region};
use omuzaki_ui_api::{register_panel, register_story, register_workspace};
use tracing_subscriber::fmt::MakeWriter;

struct Quiet;

impl Panel for Quiet {
    fn ui(&mut self, ui: &mut Ui, _ctx: &PanelCtx<'_>) {
        ui.label("quiet");
    }
}

// Each id twice, the second registered first so the order a clash is settled in is not the order in the source.
register_panel! { id: "dup.panel", title: "Dup later", icon: "", region: Region::Right, min_size: [288.0, 160.0],
order: 800, build: |_| Box::new(Quiet) }
register_panel! { id: "dup.panel", title: "Dup first", icon: "", region: Region::Right, min_size: [288.0, 160.0],
order: 700, build: |_| Box::new(Quiet) }
register_workspace! { id: "dup.ws", title: "Dup later", icon: "", order: 800, group: WorkspaceGroup::Make,
default_layout: || Node::tabs(&["viewport", "dup.panel"]) }
register_workspace! { id: "dup.ws", title: "Dup first", icon: "", order: 700, group: WorkspaceGroup::Make,
default_layout: || Node::tabs(&["viewport", "dup.panel"]) }
register_story! { id: "dup.story", title: "Story later", group: Group::Display, order: 800, state: u32,
show: |ui, _| { ui.label("later body"); } }
register_story! { id: "dup.story", title: "Story first", group: Group::Display, order: 700, state: u32,
show: |ui, _| { ui.label("first body"); } }

// A default layout naming a panel nobody registered, a workspace with no primary panel, and one with nothing to show.
register_workspace! { id: "stray.ws", title: "Stray", icon: "", order: 710, group: WorkspaceGroup::Make,
default_layout: || Node::split(Dir::Horizontal, vec![Node::pane("viewport"), Node::pane("nobody.here")], &[2.0, 1.0]) }
register_workspace! { id: "headless.ws", title: "Headless", icon: "", order: 720, group: WorkspaceGroup::Make,
default_layout: || Node::tabs(&["inspector", "agent"]) }
register_workspace! { id: "ghost.ws", title: "Ghost", icon: "", order: 730, group: WorkspaceGroup::Make,
default_layout: || Node::pane("nobody.here") }

/// A log sink the test can read.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().map_err(|_| io::Error::other("poisoned"))?.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MakeWriter<'_> for Capture {
    type Writer = Self;

    fn make_writer(&self) -> Self {
        self.clone()
    }
}

/// Runs `start` with a subscriber that records what is logged, and returns the log.
fn logged(start: impl FnOnce()) -> String {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt().with_writer(capture.clone()).with_ansi(false).finish();
    tracing::subscriber::with_default(subscriber, start);
    let bytes = capture.0.lock().map(|log| log.clone()).unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn the_shell_reports_a_workspace_or_panel_registered_twice_and_shows_the_first_once() {
    let mut shell = None;
    let log = logged(|| shell = Some(Shell::new(Arc::new(NullRunner), None)));
    let shell = shell.expect("the shell started");

    // The registry lists the story too; the shell reports only what it shows.
    let found: Vec<(DefKind, &str)> = shell.registry().duplicates().iter().map(|d| (d.kind, d.id)).collect();
    assert_eq!(found, [(DefKind::Workspace, "dup.ws"), (DefKind::Panel, "dup.panel"), (DefKind::Story, "dup.story")]);
    for duplicate in shell.registry().duplicates() {
        assert!(duplicate.kept.contains("registry_problems.rs:"), "{duplicate}");
        assert!(duplicate.dropped.contains("registry_problems.rs:"), "{duplicate}");
        assert_ne!(duplicate.kept, duplicate.dropped);
    }

    // The log line names the registry, the id and both places.
    assert!(log.contains("WARN"), "{log}");
    for (kind, id) in [("workspace", "dup.ws"), ("panel", "dup.panel")] {
        let line = format!("{kind} \"{id}\" is registered more than once");
        assert!(log.contains(&line), "no line for {kind} {id}:\n{log}");
    }
    assert!(!log.contains("story"), "stories are the gallery's to report:\n{log}");

    // The lower `order` wins; the other is left out of the lists.
    let registry = shell.registry();
    assert_eq!(registry.panel("dup.panel").map(|p| p.title), Some("Dup first"));
    assert_eq!(registry.workspaces().iter().filter(|w| w.id == "dup.ws").count(), 1);
    assert_eq!(registry.workspace("dup.ws").map(|w| w.title), Some("Dup first"));
}

#[test]
fn the_shell_draws_the_kept_workspace_once() {
    let mut harness = Harness::builder()
        .with_size(vec2(1920.0, 1080.0))
        .build_ui_state(|ui, shell: &mut Shell| shell.ui(ui), Shell::new(Arc::new(NullRunner), None));
    harness.run_steps(4);
    assert_eq!(harness.query_all_by_label("Dup first").count(), 1, "one tab for the one workspace");
    assert!(harness.query_by_label("Dup later").is_none());
}

#[test]
fn the_gallery_reports_a_story_registered_twice_and_lists_the_first_once() {
    let log = logged(|| drop(Gallery::new(Settings::default())));
    assert!(
        log.contains("story") && log.contains("dup.story") && log.contains("is registered more than once"),
        "{log}"
    );
    assert!(!log.contains("panel"), "panels are the shell's to report:\n{log}");

    let ids: Vec<_> = gallery::all().iter().filter(|s| s.id == "dup.story").map(|s| s.title).collect();
    assert_eq!(ids, ["Story first"]);
    let reported: Vec<_> = gallery::duplicates().iter().map(|d| (d.kind, d.id)).collect();
    assert_eq!(reported, [(DefKind::Story, "dup.story")]);
}

#[test]
fn a_default_layout_naming_a_missing_panel_shows_without_it_and_one_with_nothing_to_show_is_left_out() {
    let mut harness = None;
    let log = logged(|| {
        let mut started = Harness::builder()
            .with_size(vec2(1920.0, 1080.0))
            .build_ui_state(|ui, shell: &mut Shell| shell.ui(ui), Shell::new(Arc::new(NullRunner), None));
        started.run_steps(4);
        harness = Some(started);
    });
    let harness = harness.expect("the shell started");

    // Reported, by workspace and panel.
    assert!(log.contains("stray.ws") && log.contains("nobody.here"), "{log}");
    assert!(log.contains("ghost.ws") && log.contains("left out"), "{log}");
    assert!(log.contains("headless.ws") && log.contains("no primary panel"), "{log}");

    // The stray one is a tab with the panels that do exist; the ghost has no tab; the headless one still shows.
    for tab in ["Stray", "Headless"] {
        assert!(harness.query_by_label(tab).is_some(), "{tab} is not a tab");
    }
    assert!(harness.query_by_label("Ghost").is_none(), "a workspace with nothing to show has no tab");
}

#[test]
fn a_workspace_with_a_missing_panel_or_without_a_primary_panel_still_opens_and_draws() {
    let mut harness = Harness::builder()
        .with_size(vec2(1920.0, 1080.0))
        .build_ui_state(|ui, shell: &mut Shell| shell.ui(ui), Shell::new(Arc::new(NullRunner), None));
    harness.run_steps(4);
    // Tabs, in order: build, dup.ws, stray.ws, headless.ws.
    for (key, workspace, body) in
        [(Key::Num3, "stray.ws", "Start with a character"), (Key::Num4, "headless.ws", "Select a part to edit it")]
    {
        press(&mut harness, Modifiers::COMMAND, key);
        assert_eq!(harness.state().active_workspace(), Some(workspace));
        harness.run_steps(3);
        assert!(harness.query_all_by_label(body).next().is_some(), "{workspace} does not draw {body:?}");
    }
}
