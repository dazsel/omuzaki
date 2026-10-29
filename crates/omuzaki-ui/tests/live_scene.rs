//! The live view draws the project's scene and follows it (#2401, #2415): the scene's backdrop and props are in the
//! program frame, and an edit through an op (a backdrop, a second cast member, a hug between the two) shows in it a few
//! frames later, on the real renderer (software Vulkan), with nothing restarted.
//!
//! Set `OMUZAKI_EVIDENCE_DIR=/some/folder` to keep the pictures (the PR's evidence).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::Image;
use omuzaki_stage::scene::stage::SceneStage;
use omuzaki_stage_api::MemberId;
use omuzaki_track_mock::FakeTracker;
use omuzaki_ui::aux_window::Transparency;
use omuzaki_ui::live::viewport::{ReadbackPresenter, StudioBodies, document_stage};
use omuzaki_ui::live::wiring::{LiveApp, LiveScene, RealClock};
use omuzaki_ui::studio;
use omuzaki_ui::wiring::{Document, core_on};
use serde_json::{Value, json};

const SIZE: UVec2 = UVec2::new(320, 240);
const LIMIT: Duration = Duration::from_secs(180);

fn request(op: &str, args: Value) -> Request {
    Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: Actor::Person { name: "director".into() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    }
}

#[allow(clippy::needless_pass_by_value, reason = "every call site passes a json! literal")]
fn run(runner: &Arc<dyn Runner>, op: &str, args: Value) -> Value {
    runner
        .run(request(op, args.clone()))
        .unwrap_or_else(|e| panic!("{op} {args}: {e:?}"))
        .plan
        .output
        .unwrap_or(Value::Null)
}

/// The share of the picture's pixels within a few levels of `rgb`.
fn share_of(image: &Image, rgb: [u8; 3]) -> f64 {
    let (pixels, _) = image.rgba8.as_chunks::<4>();
    let near = pixels.iter().filter(|p| p.iter().zip(rgb).all(|(a, b)| a.abs_diff(b) <= 12)).count();
    #[allow(clippy::cast_precision_loss)] // pixel counts
    let share = near as f64 / pixels.len() as f64;
    share
}

/// How many pixels differ between two pictures by more than a few levels.
fn changed(a: &Image, b: &Image) -> usize {
    let (left, _) = a.rgba8.as_chunks::<4>();
    let (right, _) = b.rgba8.as_chunks::<4>();
    left.iter().zip(right).filter(|(x, y)| x.iter().zip(*y).take(3).any(|(p, q)| p.abs_diff(*q) > 8)).count()
}

fn keep(name: &str, image: &Image) {
    let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") else { return };
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    image::RgbaImage::from_raw(image.size.x, image.size.y, image.rgba8.clone()).unwrap().save(path).unwrap();
}

/// The live slice over the project in `dir`, staged as `main.rs` stages it.
fn live(dir: &std::path::Path) -> LiveApp {
    let document = Document::project(dir).expect("the project opens");
    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .expect("a software adapter (install mesa-vulkan-drivers)");
    let mut bodies = StudioBodies::new(None, omuzaki_render_api::MtoonSupport::NONE);
    let stage = document_stage(&mut renderer, &document, &mut bodies, SIZE).expect("the project's scene stages");
    let base = LiveScene::base_pose(&stage);
    let tracker = Box::new(FakeTracker::scripted(Vec::new()));
    let mut app = LiveApp::start_on(document, tracker, Box::new(RealClock::new()), Transparency::Fallback, base)
        .expect("the live slice starts");
    app.attach_viewport(renderer, stage, SIZE, Box::new(ReadbackPresenter::default())).expect("the stage attaches");
    app
}

/// Shows frames until `done` holds for the staged scene, then a few more so the picture has caught up, and returns it.
fn until_staged(
    app: &mut LiveApp,
    ctx: &egui::Context,
    what: &str,
    mut done: impl FnMut(&SceneStage) -> bool,
) -> Image {
    let deadline = Instant::now() + LIMIT;
    loop {
        app.show(ctx);
        if done(app.viewport_mut().expect("attached").scene().stage()) {
            break;
        }
        assert!(Instant::now() < deadline, "{what} was never staged");
        std::thread::yield_now();
    }
    for _ in 0..10 {
        app.show(ctx);
    }
    app.viewport_mut().expect("attached").snapshot().expect("the picture reads back")
}

/// Shows frames until `done` holds for the program picture, and returns that picture.
fn until(app: &mut LiveApp, ctx: &egui::Context, what: &str, mut done: impl FnMut(&Image) -> bool) -> Image {
    let deadline = Instant::now() + LIMIT;
    loop {
        app.show(ctx);
        if let Some(view) = app.viewport_mut() {
            let picture = view.snapshot().expect("the picture reads back");
            if done(&picture) {
                return picture;
            }
        }
        assert!(Instant::now() < deadline, "{what} never showed");
        std::thread::yield_now();
    }
}

#[test]
fn the_live_view_draws_the_projects_scene_and_follows_each_edit() {
    omuzaki_ui::link();
    let dir = tempfile::tempdir().expect("a folder");
    // The project as a person left it: their character in a scene with a red backdrop and a desk before them.
    let core = core_on(Document::project(dir.path()).expect("opens"), omuzaki_ops_api::Services::default())
        .expect("the ops start");
    let id = studio::open_character(core.runner.as_ref()).expect("the sample is made");
    run(&core.runner, "stage.create_scene", json!({ "id": "main" }));
    run(&core.runner, "stage.set_backdrop", json!({ "kind": "colour", "colour": "#ff0000" }));
    run(&core.runner, "stage.add_prop", json!({ "kind": "desk", "position": [0.0, 0.0, -0.9] }));
    drop(core);

    let mut app = live(dir.path());
    let ctx = egui::Context::default();
    let runner = app.runner();
    let red = until(&mut app, &ctx, "the red backdrop", |p| share_of(p, [255, 0, 0]) > 0.2);
    keep("live-scene-1-red", &red);

    // A person (or an agent) turns the backdrop blue: the program shows it, with no restart.
    run(&runner, "stage.set_backdrop", json!({ "kind": "colour", "colour": "#0000ff" }));
    let blue = until(&mut app, &ctx, "the blue backdrop", |p| share_of(p, [0, 0, 255]) > 0.2);
    assert!(share_of(&blue, [255, 0, 0]) < 0.01, "the red is gone");
    keep("live-scene-2-blue", &blue);

    // A second member steps in beside the first, and then the two hug: each is staged, and each changes the picture.
    let friend = MemberId::new("friend").unwrap();
    run(&runner, "stage.place_character", json!({ "character": id.as_str(), "id": "friend" }));
    let two = until_staged(&mut app, &ctx, "the second member", |stage| stage.members().any(|m| *m == friend));
    assert!(changed(&two, &blue) > 2000, "the second member is drawn");
    keep("live-scene-3-two", &two);
    run(&runner, "stage.interact", json!({ "member": "main", "with": "friend", "kind": "hug" }));
    let main = MemberId::new("main").unwrap();
    let hug = until_staged(&mut app, &ctx, "the hug", |stage| stage.cast().touch_report(&main).is_some());
    assert!(changed(&hug, &two) > 2000, "the two meet: the friend steps in, facing the subject");
    keep("live-scene-4-hug", &hug);
    app.shutdown().expect("a clean shutdown");
}
