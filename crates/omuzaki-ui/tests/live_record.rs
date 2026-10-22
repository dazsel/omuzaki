//! The studio's stream join: the stage's program frame reaches the stream service while an output runs, drawn clean
//! (the character alone, with alpha), and nothing is read back while none runs. A take started in the app's own
//! service records the stage, on the real renderer (software Vulkan).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::{Duration, Instant};

use omuzaki_core::glam::UVec2;
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_stream::record::runtime::RecordPlan;
use omuzaki_stream::record::settings::RecordFormat;
use omuzaki_track_mock::FakeTracker;
use omuzaki_ui::aux_window::Transparency;
use omuzaki_ui::live::viewport::{ReadbackPresenter, kit_stage};
use omuzaki_ui::live::wiring::{LiveApp, LiveScene, RealClock};

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
