//! The studio's stream join: the stage's program frame reaches the stream service while an output runs, drawn clean
//! (the character alone, with alpha), and nothing is read back while none runs. A take started in the app's own
//! service records the stage, on the real renderer (software Vulkan). The take carries the program: an overlay added
//! through the ops is drawn into the recorded frames.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::{Duration, Instant};

use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_stream::record::runtime::RecordPlan;
use omuzaki_stream::record::settings::RecordFormat;
use omuzaki_track_mock::FakeTracker;
use omuzaki_ui::aux_window::Transparency;
use omuzaki_ui::live::viewport::{ReadbackPresenter, kit_stage};
use omuzaki_ui::live::wiring::{LiveApp, LiveScene, RealClock};
use serde_json::{Value, json};

const SIZE: UVec2 = UVec2::new(160, 200);
const LIMIT: Duration = Duration::from_secs(120);

fn live() -> LiveApp {
    omuzaki_ui::link();
    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .expect("a software adapter (install mesa-vulkan-drivers)");
    let stage = kit_stage(&mut renderer, SIZE).expect("the kit stage starts");
    let base = LiveScene::base_pose(&stage);
    let mut app = LiveApp::start(
        Box::new(FakeTracker::scripted(Vec::new())),
        Box::new(RealClock::new()),
        Transparency::Fallback,
        base,
    )
    .expect("the live slice starts");
    app.attach_viewport(renderer, stage, SIZE, Box::new(ReadbackPresenter::default())).expect("the stage attaches");
    app
}

#[test]
fn a_take_in_the_studio_records_the_stage_with_alpha() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = live();
    let ctx = egui::Context::default();
    let stream = std::sync::Arc::clone(app.stream().expect("the app runs a stream service"));
    assert_eq!(stream.program().size, SIZE, "the program is the stage's size");
    // Nothing runs: frames are drawn and nothing is handed to the service.
    let idle = Instant::now() + Duration::from_millis(300);
    while Instant::now() < idle {
        app.show(&ctx);
        std::thread::yield_now();
    }
    assert_eq!(stream.counters().pushed, 0, "no frame is read back while no output runs");

    let plan = RecordPlan {
        dir: dir.path().to_owned(),
        stem: "studio".into(),
        format: RecordFormat::PngSequence,
        tracks: Vec::new(),
        hold: None,
        guard: None,
    };
    stream.record_start(plan).expect("a take starts");
    let deadline = Instant::now() + LIMIT;
    while stream.record_status().frames < 10 {
        assert!(Instant::now() < deadline, "the take got {:?}", stream.record_status());
        app.show(&ctx);
        std::thread::yield_now();
    }
    let report = stream.record_stop().expect("the take stops");
    assert!(report.frames >= 10, "{report:?}");
    assert_eq!((report.size, report.fps), ([SIZE.x, SIZE.y], [30, 1]));

    // The last frame written: clear in the corner (no studio backdrop), the character opaque.
    let mut frames: Vec<_> = std::fs::read_dir(&report.video).unwrap().map(|e| e.unwrap().path()).collect();
    frames.sort();
    let last = image::open(frames.last().unwrap()).unwrap().to_rgba8();
    assert_eq!(last.get_pixel(0, 0)[3], 0, "the take carries the character alone");
    assert!(last.pixels().any(|px| px[3] == 255), "the character itself is opaque");
    app.shutdown().expect("a clean shutdown");
}

fn run(runner: &std::sync::Arc<dyn Runner>, op: &str, args: Value) -> Result<Value, String> {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: Actor::Person { name: "you".into() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).map(|o| o.plan.output.unwrap_or(Value::Null)).map_err(|e| format!("{op}: {e}"))
}

#[test]
fn an_overlay_added_through_the_ops_is_drawn_into_the_take() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = live();
    let ctx = egui::Context::default();
    let runner = app.runner();
    run(
        &runner,
        "overlay.add",
        json!({ "id": "title", "kind": "text", "settings": { "text": "Omuzaki" }, "layout": { "anchor": "top_left" } }),
    )
    .unwrap();
    assert!(run(&runner, "program.switch_scene", json!({ "scene": "nowhere" })).unwrap_err().contains("no scene"));
    // The studio follows the document once its changes settle.
    let deadline = Instant::now() + LIMIT;
    let status = loop {
        app.show(&ctx);
        let status = run(&runner, "program.status", json!({})).unwrap();
        if status["widgets"] == 1 {
            break status;
        }
        assert!(Instant::now() < deadline, "the program never took the widget: {status}");
        std::thread::yield_now();
    };
    assert_eq!(status["quality"]["adaptive"], false, "a test slice's frames never change under it: {status}");

    let stream = std::sync::Arc::clone(app.stream().expect("the app runs a stream service"));
    let plan = RecordPlan {
        dir: dir.path().to_owned(),
        stem: "overlay".into(),
        format: RecordFormat::PngSequence,
        tracks: Vec::new(),
        hold: None,
        guard: None,
    };
    stream.record_start(plan).expect("a take starts");
    let deadline = Instant::now() + LIMIT;
    while stream.record_status().frames < 5 {
        assert!(Instant::now() < deadline, "the take got {:?}", stream.record_status());
        app.show(&ctx);
        std::thread::yield_now();
    }
    let report = stream.record_stop().expect("the take stops");
    let mut frames: Vec<_> = std::fs::read_dir(&report.video).unwrap().map(|e| e.unwrap().path()).collect();
    frames.sort();
    let last = image::open(frames.last().unwrap()).unwrap().to_rgba8();
    // The text panel in the top left, inside the safe margin, where the bare stage is clear.
    let panel =
        (8..60).flat_map(|x| (8..24).map(move |y| (x, y))).filter(|&(x, y)| last.get_pixel(x, y)[3] > 150).count();
    assert!(panel > 200, "the overlay's panel is in the take: {panel} pixels");
    assert_eq!(last.get_pixel(SIZE.x - 1, 0)[3], 0, "and the rest of the corner row stays clear");
    app.shutdown().expect("a clean shutdown");
}
