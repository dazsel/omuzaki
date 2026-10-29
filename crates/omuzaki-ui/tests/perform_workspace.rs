//! The Perform workspace (#347, `docs/design/ux/shell.md` section 12) in the studio's shell, over the app's own live
//! slice: the perform thread with the life layer, its newest-wins mailbox, and the runner with the perform and trigger
//! ops in its services. No camera is chosen, so the life layer drives the face, as on a fresh install.
//!
//! - The layout matches the wireframe's structure at 1920 × 1080 and 1366 × 768: the inputs and their controls (the
//!   Live panel) on the left, the stage in the middle, the channels on the right.
//! - The Channels panel meters the perform thread's channels with who drives each, Face, Head and Body, with a search.
//! - The expression strip along the stage's foot shows the bound expressions with their keys, and a click shows one
//!   through `trigger.press`.
//!
//! Set `OMUZAKI_EVIDENCE_OUT=/some/dir` and run the ignored test to write the screenshots, with the kit character on
//! the stage on the software renderer.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_anim_api::HumanoidPose;
use omuzaki_char_api::{BuildServices, RegistryBuilder};
use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_track_mock::FakeTracker;
use omuzaki_ui::aux_window::Transparency;
use omuzaki_ui::ds::icons;
use omuzaki_ui::ds::testing::press_for;
use omuzaki_ui::ds::widgets::Button;
use omuzaki_ui::live::registration::{CHANNELS_PANEL, LIVE_PANEL, STAGE_PANEL};
use omuzaki_ui::live::viewport::{ReadbackPresenter, kit_stage, published};
use omuzaki_ui::live::wiring::{LiveApp, LiveScene, RealClock};
use omuzaki_ui::panels::agent::Agents;
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::viewport::ViewportServices;
use omuzaki_ui::shell::Shell;
use omuzaki_ui::studio;
use omuzaki_ui_api::registry::Registry;
use serde_json::{Value, json};

struct World {
    app: LiveApp,
    shell: Shell,
}

/// The studio as `main.rs` builds it over a live slice, switched to the Perform workspace.
fn perform(app: LiveApp, size: egui::Vec2, gpu: bool) -> Harness<'static, World> {
    let core = app.core();
    let character = studio::open_character(core.runner.as_ref()).expect("the kit character opens");
    let mut shell = Shell::new(Arc::clone(&core.runner), None);
    shell.insert_view(EditFocus::new(character));
    shell.insert_view(Agents(Arc::clone(&core.sessions)));
    shell.insert_view(ViewportServices {
        gpu: None,
        builder: Arc::new(RegistryBuilder),
        build: BuildServices::default(),
    });
    shell.insert_view(app.channel_feed());
    let mut builder = Harness::builder().with_size(size).with_pixels_per_point(1.0).with_step_dt(1.0 / 60.0);
    if gpu {
        builder = builder.wgpu_setup(support::wgpu_setup());
    }
    let mut h = builder.build_ui_state(
        |ui, world: &mut World| {
            // The stage first, so its picture is this frame's, as the app draws it.
            world.app.show(ui.ctx());
            world.shell.ui(ui);
        },
        World { app, shell },
    );
    h.run_steps(4);
    let index = Registry::collect().workspaces().iter().position(|ws| ws.id == "perform").expect("registered");
    let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
    press_for(&mut h, Modifiers::COMMAND, keys[index], 4);
    assert_eq!(h.state().shell.active_workspace(), Some("perform"));
    h
}

/// The live slice with no camera and no stage, as the app starts before a GPU is ready.
fn live() -> LiveApp {
    omuzaki_ui::link();
    LiveApp::start(
        Box::new(FakeTracker::scripted(Vec::new())),
        Box::new(RealClock::new()),
        Transparency::Fallback,
        HumanoidPose::REST,
    )
    .expect("the live slice starts")
}

fn run(runner: &Arc<dyn Runner>, op: &str, args: Value) -> Value {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: Actor::Person { name: "performer".into() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).unwrap_or_else(|e| panic!("{op}: {e:?}")).plan.output.unwrap_or(Value::Null)
}

/// Steps the workspace until `done`; the perform thread runs on the real clock, so this waits on what it publishes.
fn until(h: &mut Harness<'_, World>, what: &str, mut done: impl FnMut(&Harness<'_, World>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(h) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.run_steps(1);
        // Paces the loop; what it waits for is the state `done` reads, not the time.
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn rect_of(h: &Harness<'_, World>, id: &str) -> egui::Rect {
    h.state().shell.panel_rects().into_iter().find(|(panel, _)| panel == id).map(|(_, r)| r).expect(id)
}

#[test]
fn the_inputs_the_stage_and_the_channels_stand_left_to_right_at_both_reference_sizes() {
    let registry = Registry::collect();
    for size in [vec2(1920.0, 1080.0), vec2(1366.0, 768.0)] {
        let mut h = perform(live(), size, false);
        let (inputs, stage, channels) =
            (rect_of(&h, LIVE_PANEL), rect_of(&h, STAGE_PANEL), rect_of(&h, CHANNELS_PANEL));
        assert!(inputs.right() <= stage.left() + 1.0 && stage.right() <= channels.left() + 1.0, "{size:?}: order");
        assert!(
            stage.width() > inputs.width() && stage.width() > channels.width(),
            "{size:?}: the stage is the widest"
        );
        for (id, rect) in [(LIVE_PANEL, inputs), (STAGE_PANEL, stage), (CHANNELS_PANEL, channels)] {
            let min = registry.panel(id).expect("registered").min_size;
            assert!(
                rect.width() + 1.0 >= min[0],
                "{size:?}: {id} is {} wide, under its minimum {}",
                rect.width(),
                min[0]
            );
        }
        until(&mut h, "the first channels", |h| h.query_by_label("Face channels").is_some());
        // The top bar's last control keeps its whole width once the bundled fonts are in: a width measured with
        // the first frame's fallback font once left it cut to "Go L…".
        let go_live = h.get_by_label("Go Live").rect().width();
        let whole = Button::new("Go Live").icon(icons::BROADCAST).width(&h.ctx);
        assert!(go_live + 0.5 >= whole, "{size:?}: Go Live is {go_live} wide, cut from {whole}");
        h.state_mut().app.shutdown().expect("a clean shutdown");
    }
}

#[test]
fn the_channels_panel_meters_the_perform_threads_channels_with_who_drives_them() {
    let mut h = perform(live(), vec2(1920.0, 1080.0), false);
    until(&mut h, "the face meters", |h| h.query_by_label("Face channels").is_some());

    // The grid is one tab stop; its value names the focused channel and who owns it. With no camera, the life layer
    // breathes: on the Body tab, Breath is the life layer's.
    h.get_by_label("Body").click();
    until(&mut h, "the body meters", |h| h.query_by_label("Body channels").is_some());
    h.get_by_label("Body channels").focus();
    h.run_steps(2);
    press_for(&mut h, Modifiers::NONE, Key::End, 1);
    until(&mut h, "the breath owned by the life layer", |h| {
        let value = h.get_by_label("Body channels").accesskit_node().value().unwrap_or_default();
        value.starts_with("Breath:") && value.contains("owner life layer")
    });

    // Head and Body narrow the meters to their channels; the search narrows by name.
    h.get_by_label("Head").click();
    until(&mut h, "the head meters", |h| h.query_by_label("Head channels").is_some());
    h.get_by_label("Face").click();
    until(&mut h, "the face meters again", |h| h.query_by_label("Face channels").is_some());
    let search = h.get_by_label("Search channels");
    search.focus();
    h.run_steps(2);
    h.event(egui::Event::Text("smile".to_owned()));
    h.run_steps(4);
    h.get_by_label("Face channels").focus();
    h.run_steps(2);
    press_for(&mut h, Modifiers::NONE, Key::Home, 1);
    let first = h.get_by_label("Face channels").accesskit_node().value().unwrap_or_default();
    assert!(first.starts_with("Smile L:"), "the search leaves the smiles: {first}");
    press_for(&mut h, Modifiers::NONE, Key::ArrowDown, 1);
    let second = h.get_by_label("Face channels").accesskit_node().value().unwrap_or_default();
    assert!(second.starts_with("Smile R:"), "{second}");
    h.state_mut().app.shutdown().expect("a clean shutdown");
}

#[test]
fn the_expression_strip_shows_the_bound_keys_and_a_click_shows_the_expression() {
    let app = live();
    let runner = app.runner();
    let mut h = perform(app, vec2(1366.0, 768.0), false);
    for (key, expression) in [("F1", "happy"), ("F2", "sad")] {
        run(&runner, "trigger.bind", json!({"key": key, "expression": expression}));
    }
    until(&mut h, "the strip", |h| h.query_by_label("Show Happy (F1)").is_some());
    let stage = rect_of(&h, STAGE_PANEL);
    let button = h.get_by_label("Show Happy (F1)");
    assert_eq!(button.accesskit_node().role(), Role::Button);
    assert!(stage.contains(button.rect().center()), "the strip is on the stage");
    assert!(h.query_by_label("Show Sad (F2)").is_some());
    button.click();
    // The press reaches the performer at its next step, on its own thread.
    until(&mut h, "Happy showing", |_| {
        let list = run(&runner, "trigger.list", json!({}));
        list["triggers"]
            .as_array()
            .and_then(|t| t.iter().find(|t| t["binding"]["key"] == "F1"))
            .map(|t| t["active"].clone())
            == Some(Value::Bool(true))
    });
    h.state_mut().app.shutdown().expect("a clean shutdown");
}

/// At both reference sizes the Perform workspace cuts no label short: every channel's name, the Live panel's controls and
/// the top bar read whole (`studio_layout.rs` audits the other workspaces; this one needs the live slice).
#[test]
fn the_perform_workspace_cuts_no_label_at_either_reference_size() {
    for size in [vec2(1366.0, 768.0), vec2(1920.0, 1080.0)] {
        let mut h = perform(live(), size, false);
        h.run_steps(8);
        let mut found: Vec<String> = omuzaki_ui::ds::audit::elided_findings(h.output())
            .into_iter()
            .chain(omuzaki_ui::ds::audit::cut_sideways(h.output()))
            .map(|f| format!("{size:?}: {f}"))
            .collect();
        found.dedup();
        h.state_mut().app.shutdown().expect("a clean shutdown");
        assert!(found.is_empty(), "{} labels cut:\n{}", found.len(), found.join("\n"));
    }
}

/// The Perform workspace at both reference sizes with the kit character on the stage, for the pull request.
#[test]
#[ignore = "writes images; run by the visual evidence job"]
fn perform_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    for (w, ht) in [(1920.0_f32, 1080.0_f32), (1366.0, 768.0)] {
        omuzaki_ui::link();
        let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
            .expect("a software adapter (install mesa-vulkan-drivers)");
        // The stage draws the program at about the size of its panel.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // whole sizes
        let size = UVec2::new((w * 0.55) as u32, (ht - 80.0) as u32);
        let stage = kit_stage(&mut renderer, size).expect("the kit stage starts");
        let base = LiveScene::base_pose(&stage);
        let mut app = LiveApp::start(
            Box::new(FakeTracker::scripted(Vec::new())),
            Box::new(RealClock::new()),
            Transparency::Fallback,
            base,
        )
        .expect("the live slice starts");
        app.attach_viewport(renderer, stage, size, Box::new(ReadbackPresenter::default())).expect("attaches");
        let runner = app.runner();
        for (key, expression) in [("F1", "happy"), ("F2", "surprised"), ("F3", "angry"), ("F4", "sad")] {
            run(&runner, "trigger.bind", json!({"key": key, "expression": expression}));
        }
        let mut h = perform(app, vec2(w, ht), true);
        until(&mut h, "the stage's picture and the strip", |h| {
            published(&h.ctx).is_some() && h.query_by_label("Show Happy (F1)").is_some()
        });
        h.run_steps(30);
        let image = h.render().expect("renders");
        let path = format!("{out}/perform-{w}x{ht}.png");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
        h.state_mut().app.shutdown().expect("a clean shutdown");
    }
}
