//! The Live panel, the Perform workspace and the Live story are registered with `omuzaki-ui-api`, and the panel passes
//! the conformance check every panel crate runs (`omuzaki-ui-mock`).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used)]

use omuzaki_ops_api::Actor;
use omuzaki_ui::live::registration::{LIVE_PANEL, PERFORM_WORKSPACE};
use omuzaki_ui::live::story::StoryRunner;
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui_api::panel::PanelCtx;
use omuzaki_ui_api::registry::{Group, Region};
use omuzaki_ui_mock::MockHost;

fn person() -> Actor {
    Actor::Person { name: "performer".into() }
}

#[test]
fn the_live_panel_passes_the_conformance_check_over_an_empty_document() {
    omuzaki_ui::link();
    let host = MockHost::new();
    let (runner, actor) = (NullRunner, person());
    let problems = host.check(LIVE_PANEL, &PanelCtx::new(&runner, &actor));
    assert!(problems.is_empty(), "{problems:#?}");
    let def = host.registry().panel(LIVE_PANEL).expect("registered");
    assert_eq!((def.title, def.region), ("Live", Region::Right));
}

#[test]
fn the_panel_draws_its_controls_from_the_runner_alone() {
    omuzaki_ui::link();
    let host = MockHost::new();
    let (runner, actor) = (StoryRunner::default(), person());
    let ctx = PanelCtx::new(&runner, &actor);
    let (def, mut panel) = host.open(LIVE_PANEL, &ctx).expect("registered");
    let frame = host.frame(panel.as_mut(), def, &ctx, [420.0, 1400.0]);
    assert!(frame.body);
    for label in ["Calibrate", "Smoothing", "Add hotkey", "Output window", "Keep on top", "Track my face"] {
        assert!(frame.labels.iter().any(|l| l == label), "{label} is drawn: {:?}", frame.labels);
    }
}

#[test]
fn the_perform_workspace_shows_the_viewport_and_the_live_panel_and_the_story_is_registered() {
    omuzaki_ui::link();
    let host = MockHost::new();
    let registry = host.registry();
    let perform = registry.workspace(PERFORM_WORKSPACE).expect("registered");
    assert_eq!((perform.title, perform.group), ("Perform", Group::Run));
    assert_eq!((perform.default_layout)().panels(), ["viewport", LIVE_PANEL]);
    assert!(registry.stories().iter().any(|story| story.id == "live-workspace"));
    assert!(registry.duplicates().is_empty(), "{:?}", registry.duplicates());
}
