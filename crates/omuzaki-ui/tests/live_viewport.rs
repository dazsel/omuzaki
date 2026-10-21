//! The Live workspace's stage view, end to end on the real renderer (software Vulkan): the kit character the perform
//! thread moves, the latency the health strip measures, and the evidence pictures of the workspace with the stage in it.
//!
//! The pipeline is the app's: `LiveApp` over `FakeTracker`'s synthetic face, the perform thread, the newest-wins
//! mailbox, `LiveViewport` drawing the kit stage on `WgpuRenderer`, and the Live panel reading the app's own ops.
//!
//! Set `OMUZAKI_LIVE_EVIDENCE_DIR=/some/dir` to write the evidence pictures (CI's `live-workspace` case does).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use omuzaki_anim_mock::track::{SyntheticSpec, synthetic_track};
use omuzaki_core::glam::UVec2;
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::Image;
use omuzaki_track_mock::{FakeCamera, FakeTracker};
use omuzaki_ui::aux_window::Transparency;
use omuzaki_ui::ds::Settings;
use omuzaki_ui::live::latency::LatencyMeter;
use omuzaki_ui::live::panel::LivePanel;
use omuzaki_ui::live::viewport::{ReadbackPresenter, StagePanel, kit_stage, published};
use omuzaki_ui::live::wiring::{LiveApp, LiveScene, RealClock};

/// A tracker replaying `seconds` of a synthetic face at 30 fps.
fn tracker(seconds: f32) -> FakeTracker {
    let track = synthetic_track(&SyntheticSpec { seconds, ..SyntheticSpec::default() });
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // whole nanoseconds of a short script
    FakeTracker::scripted(track.frames.iter().map(|f| (Duration::from_nanos((f.time.0 * 1e9) as u64), *f)).collect())
}

/// The app's pipeline over `tracker` with the kit stage drawn at `size` on the software renderer.
fn live(tracker: FakeTracker, size: UVec2, meter: &Arc<LatencyMeter>) -> LiveApp {
    omuzaki_ui::link();
    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .expect("a software adapter (install mesa-vulkan-drivers)");
    let stage = kit_stage(&mut renderer, size).expect("the kit stage starts");
    let base = LiveScene::base_pose(&stage);
    let mut app = LiveApp::start(Box::new(tracker), Box::new(RealClock::new()), Transparency::Fallback, base)
        .expect("the live slice starts");
    app.attach_viewport(renderer, stage, size, Box::new(ReadbackPresenter::default())).expect("the stage attaches");
    app.viewport_mut().expect("attached").set_meter(Arc::clone(meter));
    app
}

fn request(op: &str, args: Value) -> Request {
    Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: Actor::Person { name: "performer".into() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    }
}

fn args(pairs: &[(&str, Value)]) -> Value {
    Value::Object(pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect())
}

fn run(runner: &Arc<dyn Runner>, op: &str, pairs: &[(&str, Value)]) -> Value {
    runner.run(request(op, args(pairs))).unwrap_or_else(|e| panic!("{op}: {e:?}")).plan.output.unwrap_or(Value::Null)
}

/// Picks the fake camera and starts tracking, as the panel's Camera section does.
fn track_my_face(runner: &Arc<dyn Runner>) {
    run(runner, "input.select", &[("source", Value::from("webcam"))]);
    run(runner, "input.bind_camera", &[("camera", Value::from(FakeCamera::id().0))]);
    run(runner, "input.preview", &[("on", Value::from(true))]);
}

/// Calls `step` every few milliseconds until `done` holds or `limit` passes; whether it held.
fn until(limit: Duration, mut step: impl FnMut(), mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        step();
        if done() {
            return true;
        }
        // Paces the loop; what it waits for is the state `done` reads, not the time.
        std::thread::sleep(Duration::from_millis(8));
    }
    done()
}

const LIMIT: Duration = Duration::from_secs(120);

/// How many pixels differ from the picture's top-left corner (the background) by more than a few levels.
fn drawn_pixels(image: &Image) -> usize {
    let (pixels, _) = image.rgba8.as_chunks::<4>();
    let corner = pixels[0];
    pixels.iter().filter(|px| px.iter().zip(corner).any(|(a, b)| a.abs_diff(b) > 12)).count()
}

/// How many pixels differ between two pictures of one size by more than a few levels.
fn changed_pixels(a: &Image, b: &Image) -> usize {
    assert_eq!(a.size, b.size);
    let ((a, _), (b, _)) = (a.rgba8.as_chunks::<4>(), b.rgba8.as_chunks::<4>());
    a.iter().zip(b).filter(|(p, q)| p.iter().zip(*q).any(|(x, y)| x.abs_diff(*y) > 12)).count()
}

#[test]
fn the_kit_character_is_drawn_and_moves_before_any_camera_is_chosen() {
    let meter = Arc::new(LatencyMeter::new());
    let size = UVec2::new(320, 400);
    let mut app = live(FakeTracker::scripted(Vec::new()), size, &meter);
    let ctx = egui::Context::default();
    assert!(published(&ctx).is_none(), "nothing is shown before the first frame");
    assert!(until(LIMIT, || app.show(&ctx), || published(&ctx).is_some()), "the stage draws its first frame");
    let program = published(&ctx).expect("published");
    assert_eq!((program.size.width, program.size.height), (size.x, size.y), "the program frame is the stage's size");

    let first = app.viewport_mut().expect("attached").snapshot().expect("the picture reads back");
    let total = first.rgba8.len() / 4;
    assert!(drawn_pixels(&first) * 20 > total, "the character fills a good part of the frame, not a blank one");
    // The life layer breathes and blinks: a second later the picture is not the same one.
    let later = Instant::now() + Duration::from_millis(1500);
    until(LIMIT, || app.show(&ctx), || Instant::now() > later);
    let second = app.viewport_mut().expect("attached").snapshot().expect("the picture reads back");
    assert!(changed_pixels(&first, &second) > 0, "the character moves on the life layer alone");
    assert_eq!(meter.reading(), None, "no camera frame has reached the picture, so the latency is still unknown");
    app.shutdown().expect("a clean shutdown");
}

/// The share of the picture's height above the first row that holds anything but the background.
fn headroom(image: &Image) -> f32 {
    let (pixels, _) = image.rgba8.as_chunks::<4>();
    let width = image.size.x as usize;
    let corner = pixels[0];
    let top = pixels
        .chunks(width)
        .position(|row| row.iter().any(|px| px.iter().zip(corner).any(|(a, b)| a.abs_diff(b) > 12)))
        .expect("the character is drawn");
    #[allow(clippy::cast_precision_loss)] // a picture's sides are far below 2^24
    {
        top as f32 / image.size.y as f32
    }
}

#[test]
fn the_whole_head_is_in_frame_with_headroom_at_every_stage_size() {
    // The 1920 x 1080 program frame (the output window), and the stage panel beside the dock at 1366 x 768 and
    // 1920 x 1080.
    for (w, h) in [(1920, 1080), (1060, 768), (1480, 1080)] {
        let meter = Arc::new(LatencyMeter::new());
        let mut app = live(FakeTracker::scripted(Vec::new()), UVec2::new(w, h), &meter);
        let ctx = egui::Context::default();
        assert!(until(LIMIT, || app.show(&ctx), || published(&ctx).is_some()), "the stage draws its first frame");
        let picture = app.viewport_mut().expect("attached").snapshot().expect("the picture reads back");
        let room = headroom(&picture);
        // Bust framing puts the top of the hair about 8 % below the frame; the margin allows for the idle sway.
        assert!((0.05..=0.13).contains(&room), "{w} x {h}: the hair's top is {room} of the height from the top");
        // The shoulders are in frame: the bottom row is body, not background.
        let (pixels, _) = picture.rgba8.as_chunks::<4>();
        let last = &pixels[pixels.len() - picture.size.x as usize..];
        assert!(
            last.iter().any(|px| px.iter().zip(pixels[0]).any(|(a, b)| a.abs_diff(b) > 12)),
            "{w} x {h}: body at the bottom"
        );
        app.shutdown().expect("a clean shutdown");
    }
}

/// The alpha of the picture's top-left pixel: the backdrop, since the character never reaches the corner.
fn corner_alpha(app: &mut LiveApp) -> u8 {
    app.viewport_mut().expect("attached").snapshot().expect("the picture reads back").rgba8[3]
}

#[test]
fn the_program_frame_carries_no_backdrop_while_the_output_window_is_open() {
    let meter = Arc::new(LatencyMeter::new());
    let mut app = live(FakeTracker::scripted(Vec::new()), UVec2::new(160, 200), &meter);
    let runner = app.runner();
    // A context that can open OS windows, so the output window really opens.
    let ctx = egui::Context::default();
    ctx.set_embed_viewports(false);
    let frame = |app: &mut LiveApp| {
        ctx.run_ui(egui::RawInput::default(), |ui| app.show(ui.ctx())).drop_without_applying_deltas();
    };
    let until_corner = |app: &mut LiveApp, alpha: u8| {
        let deadline = Instant::now() + LIMIT;
        while Instant::now() < deadline {
            frame(app);
            if published(&ctx).is_some() && corner_alpha(app) == alpha {
                return true;
            }
            // Paces the loop; what it waits for is a new perform frame drawn under the new backdrop.
            std::thread::sleep(Duration::from_millis(8));
        }
        false
    };

    assert!(until_corner(&mut app, 255), "with the window closed the stage draws its studio backdrop, opaque");

    run(&runner, "output.window.open", &[("backdrop", Value::from("key"))]);
    assert!(until_corner(&mut app, 0), "an output window over a key gets the character alone: the backdrop is clear");
    assert!(app.output_mut().status().open, "the window is open");
    let picture = app.viewport_mut().expect("attached").snapshot().expect("the picture reads back");
    let (pixels, _) = picture.rgba8.as_chunks::<4>();
    assert!(pixels.iter().any(|px| px[3] == 255), "the character itself is still opaque");

    run(&runner, "output.window.close", &[]);
    assert!(until_corner(&mut app, 255), "closing the window brings the studio backdrop back");
    app.shutdown().expect("a clean shutdown");
}

#[test]
fn the_latency_is_a_dash_until_a_tracked_frame_is_drawn_and_a_number_after() {
    let meter = Arc::new(LatencyMeter::new());
    let mut app = live(tracker(20.0), UVec2::new(160, 200), &meter);
    let runner = app.runner();
    let ctx = egui::Context::default();
    assert_eq!(meter.reading(), None, "before the first frame");
    track_my_face(&runner);
    let seen = until(LIMIT, || app.show(&ctx), || meter.reading().is_some());
    assert!(seen, "a tracked frame reached the picture and was timed");
    let latency = meter.reading().expect("measured");
    // A frame or two on a GPU; software Vulkan in a debug build, with other jobs on the machine (the full gate), takes
    // seconds per render. The bound only says the number is a measurement and not a runaway.
    assert!(latency < Duration::from_secs(30), "camera to picture took {latency:?}");
    assert!(latency > Duration::ZERO, "a measured latency is more than nothing");
    let status = run(&runner, "perform.status", &[]);
    assert_eq!(status["tracking"], "tracking");
    app.shutdown().expect("a clean shutdown");
}

/// The Live panel over a latency meter: a dash before any sample, the measured milliseconds after.
#[test]
fn the_health_strip_shows_the_measured_latency_and_a_dash_before_the_first_frame() {
    use omuzaki_ui::ds::testing::all_strings;
    omuzaki_ui::link();
    let meter = Arc::new(LatencyMeter::new());
    let runner = omuzaki_ui::shell::stub::NullRunner;
    let panel = LivePanel::new(&runner).with_latency(Arc::clone(&meter));
    let mut harness = Harness::builder().with_size(egui::vec2(306.0, 400.0)).build_ui_state(
        |ui, (panel, runner): &mut (LivePanel, omuzaki_ui::shell::stub::NullRunner)| {
            omuzaki_ui::ds::apply_settings(ui.ctx(), Settings::default());
            panel.ui(ui, runner, &Actor::Person { name: "performer".into() });
        },
        (panel, runner),
    );
    harness.run_steps(3);
    assert!(all_strings(&harness).iter().any(|s| s == "Latency: —"), "{:?}", all_strings(&harness));
    meter.record(Duration::from_millis(42));
    harness.run_steps(2);
    assert!(all_strings(&harness).iter().any(|s| s == "Latency: 42 ms"), "{:?}", all_strings(&harness));
}

/// The Perform workspace's default layout: the Live panel's share of the width beside the stage.
fn live_dock_share() -> f32 {
    use omuzaki_ui_api::layout::Node;
    omuzaki_ui::link();
    let host = omuzaki_ui_mock::MockHost::new();
    let perform = host.registry().workspace("perform").expect("registered");
    let Node::Split { children, shares, .. } = (perform.default_layout)() else { panic!("a split") };
    let live = children.iter().position(|child| *child == Node::pane("live")).expect("the Live panel is in it");
    shares[live] / shares.iter().sum::<f32>()
}

struct World {
    app: LiveApp,
    runner: Arc<dyn Runner>,
    panel: LivePanel,
    stage: StagePanel,
    dock: f32,
    /// Whether the stage has published a picture.
    shown: bool,
}

/// The Perform workspace as a streamer sees it: the stage in the middle and the Live panel docked at its registered
/// share, over the app's own pipeline.
fn workspace(app: LiveApp, meter: &Arc<LatencyMeter>, size: egui::Vec2) -> Harness<'static, World> {
    let runner = app.runner();
    let panel = LivePanel::new(&*runner).with_latency(Arc::clone(meter));
    let dock = (size.x * live_dock_share()).round();
    Harness::builder().with_size(size).wgpu_setup(support::wgpu_setup()).build_ui_state(
        |ui, world: &mut World| {
            omuzaki_ui::ds::apply_settings(ui.ctx(), Settings::default());
            // The stage first, so its picture is this frame's.
            world.app.show(ui.ctx());
            world.shown = published(ui.ctx()).is_some();
            let theme = omuzaki_ui::ds::Theme::of(omuzaki_ui::ds::ThemeKind::Dark);
            let frame = egui::Frame::new().fill(theme.bg1).inner_margin(12.0);
            let (runner, panel, dock) = (&world.runner, &mut world.panel, world.dock);
            egui::Panel::right("live.dock").exact_size(dock).frame(frame).show(ui, |ui| {
                panel.ui(ui, &**runner, &Actor::Person { name: "performer".into() });
            });
            let stage = &mut world.stage;
            egui::CentralPanel::default().frame(egui::Frame::new().fill(theme.bg0)).show(ui, |ui| stage.ui(ui));
        },
        World { app, runner, panel, stage: StagePanel, dock, shown: false },
    )
}

/// Steps the workspace until `done`, so the perform thread, the stage and the panel all move on.
fn settle(harness: &mut Harness<'_, World>, what: &str, mut done: impl FnMut(&World) -> bool) {
    let deadline = Instant::now() + LIMIT;
    while !done(harness.state()) && Instant::now() < deadline {
        harness.run_steps(1);
        // Paces the loop; what it waits for is the state `done` reads, not the time.
        std::thread::sleep(Duration::from_millis(8));
    }
    assert!(done(harness.state()), "the workspace reached: {what}");
    // One more frame so the picture and the panel agree on what they just reached.
    harness.run_steps(2);
}

fn save(harness: &mut Harness<'_, World>, dir: Option<&std::path::Path>, state: &str, size: (f32, f32)) {
    let image = harness.render().expect("a software or hardware adapter renders");
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // whole sizes
    let want = (size.0 as u32, size.1 as u32);
    assert_eq!(image.dimensions(), want);
    if let Some(dir) = dir {
        let name = format!("live-{state}-{}x{}.png", want.0, want.1);
        image.save(dir.join(name)).expect("the picture saves");
    }
}

/// Whether the stage panel's part of `harness`'s picture has the character in it: more than a flat background.
fn stage_shows_a_character(harness: &mut Harness<'_, World>, dock: f32) -> bool {
    let image = harness.render().expect("renders");
    let (width, height) = image.dimensions();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a whole, positive dock width
    let stage_width = width - dock as u32;
    let background = *image.get_pixel(2, 2);
    let mut differing = 0_usize;
    for y in 0..height {
        for x in 0..stage_width {
            let px = image.get_pixel(x, y);
            if px.0.iter().zip(background.0).any(|(a, b)| a.abs_diff(b) > 12) {
                differing += 1;
            }
        }
    }
    differing * 20 > (stage_width * height) as usize
}

#[test]
fn evidence_pictures_of_idle_calibrating_and_face_lost_with_the_character_on_the_stage() {
    let dir = std::env::var_os("OMUZAKI_LIVE_EVIDENCE_DIR").map(std::path::PathBuf::from);
    for (w, h) in [(1366.0_f32, 768.0_f32), (1920.0, 1080.0)] {
        let meter = Arc::new(LatencyMeter::new());
        let size = egui::vec2(w, h);
        let dock = (w * live_dock_share()).round();
        // The stage draws at the size of the space it has; the program frame itself is the output size in the app.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // whole sizes
        let stage_px = UVec2::new((w - dock) as u32, h as u32);
        // Long enough that the slow software renders between polls can not outlast the script before "tracking" is seen.
        let app = live(tracker(40.0), stage_px, &meter);
        let runner = app.runner();
        let mut harness = workspace(app, &meter, size);

        // Idle: the app just opened, no camera yet: the life layer moves the character.
        settle(&mut harness, "the first picture", |world| world.shown);
        assert!(stage_shows_a_character(&mut harness, dock), "idle: the character is on the stage");
        save(&mut harness, dir.as_deref(), "idle", (w, h));

        // Calibrating: the camera is on, the face is found and a calibration runs over it.
        track_my_face(&runner);
        settle(&mut harness, "tracking", |_| run(&runner, "perform.status", &[])["tracking"] == "tracking");
        run(&runner, "perform.calibrate", &[("frames", Value::from(300))]);
        settle(&mut harness, "a measured latency", |_| meter.reading().is_some());
        assert!(stage_shows_a_character(&mut harness, dock), "calibrating: the character is on the stage");
        save(&mut harness, dir.as_deref(), "calibrating", (w, h));

        // Face lost: the script ends, the source goes quiet and the life layer takes the face over.
        settle(&mut harness, "face lost", |_| run(&runner, "perform.status", &[])["tracking"] == "lost");
        assert!(stage_shows_a_character(&mut harness, dock), "face lost: the character is on the stage");
        save(&mut harness, dir.as_deref(), "face-lost", (w, h));
    }
}
