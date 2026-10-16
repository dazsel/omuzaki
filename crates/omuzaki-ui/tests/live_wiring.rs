//! The live slice's wiring, headless (#312): `FakeCamera`'s tracker → the perform thread → the newest-wins mailbox →
//! the stage's `Scene` on the render mock, in real time; the ops changing the running loop through the real executor;
//! and the rig's camera lifecycle and quit order.
//!
//! Set `OMUZAKI_LIVE_WIRING_REPORT=/some/file.txt` to write the 60 Hz run's numbers (the PR's evidence).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
// The counting allocator is the one unsafe impl: it forwards to `System` unchanged (the other hot-loop tests do the same).
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use omuzaki_anim_api::{Channel, ChannelBuf, FrameSource, HumanoidPose};
use omuzaki_anim_mock::track::{SyntheticSpec, synthetic_track};
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::Value;
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::ExpressionSet;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner, ServicesBuilder, find};
use omuzaki_render_api::{AlphaEncoding, Camera, Projection, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::{Stage, StageClock};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{CalibrationState, PerformHost};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::tracker::{Tracker, TrackerConfig};
use omuzaki_track_mock::{FakeCamera, FakeTracker};
use omuzaki_ui::aux_window::Transparency;
use omuzaki_ui::live::wiring::{
    FrameReader, LiveApp, LiveError, LiveFrame, LiveRig, LiveScene, LoopReport, NoCamera, NoTracker, PerformClock,
    PerformThread, RealClock, THREAD_NAME, live_performer,
};

struct Counting;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
/// Set once the perform thread turned counting on, so a zero count can't come from counting never starting.
static ARMED: AtomicBool = AtomicBool::new(false);
thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The wall clock, which turns allocation counting on for the thread it runs on (the perform thread) once `warm_up`
/// has passed. Counting stays on to the end of the loop, including the passes that wait.
struct CountingClock {
    inner: RealClock,
    warm_up: f64,
}

impl PerformClock for CountingClock {
    fn now(&mut self) -> Seconds {
        let now = self.inner.now();
        if now.0 >= self.warm_up && !MEASURING.with(Cell::get) {
            MEASURING.with(|m| m.set(true));
            ARMED.store(true, Ordering::Release);
        }
        now
    }

    fn wait_until(&mut self, at: Seconds) {
        self.inner.wait_until(at);
    }
}

/// The tracker's frames for `seconds`: the synthetic track at 30 fps, replayed by `FakeTracker` on `FakeCamera`'s id.
fn tracker(seconds: f32) -> FakeTracker {
    let track = synthetic_track(&SyntheticSpec { seconds, ..SyntheticSpec::default() });
    FakeTracker::scripted(track.frames.iter().map(|f| (Duration::from_nanos((f.time.0 * 1e9) as u64), *f)).collect())
}

fn camera(seconds: f32) -> Box<dyn FrameSource> {
    tracker(seconds).start(&FakeCamera::id(), &TrackerConfig::default()).expect("the fake camera starts")
}

fn character() -> BuiltCharacter {
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("mock").expect("a valid id"), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

/// The mock character on `renderer`, with a small target and the live clock.
fn stage(renderer: &mut MockRenderer) -> Stage {
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.8, 0.95),
        Vec3::new(0.0, 0.78, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    );
    Stage::new(
        renderer,
        character(),
        UploadOptions { face_px_per_m: 600.0 },
        Scene::new(camera),
        target,
        Quality::offline(),
        StageClock::live(),
    )
    .expect("the stage starts")
}

/// Waits up to `limit` for `check` to hold, looking every few milliseconds.
fn eventually(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    check()
}

const RUN: Duration = Duration::from_secs(10);
const WARM_UP_S: f64 = 1.0;

#[test]
fn fake_camera_to_scenes_at_60_hz_for_10_s_without_a_drop_storm_or_an_allocation() {
    omuzaki_ui::link();
    let mut renderer = MockRenderer::new();
    let stage = stage(&mut renderer);
    let base = LiveScene::base_pose(&stage);
    let performer = live_performer(camera(12.0), ChannelBuf::NEUTRAL, base);
    let clock = CountingClock { inner: RealClock::new(), warm_up: WARM_UP_S };
    let (perform, reader) = PerformThread::spawn(performer, Box::new(clock)).expect("the perform thread starts");
    let mut scene = LiveScene::new(stage, reader);

    // The render thread: draw whenever a new frame is there, as a 60 Hz (or faster) display would.
    let (start, mut scenes, mut tracked, mut last_index) = (Instant::now(), 0_u64, 0_u64, None);
    let mut last_render = start;
    let mut warm_allocations = None;
    while start.elapsed() < RUN {
        if warm_allocations.is_none() && start.elapsed().as_secs_f64() > WARM_UP_S + 0.5 {
            warm_allocations = Some(ALLOCATIONS.load(Ordering::Relaxed));
        }
        let dt = last_render.elapsed().as_secs_f32();
        if let Some(out) = scene.frame(&mut renderer, dt).expect("the frame renders") {
            last_render = Instant::now();
            scenes += 1;
            let frame = scene.last_frame().expect("a frame was rendered");
            assert!(last_index.is_none_or(|last| frame.tick.index > last), "frames arrive in order");
            last_index = Some(frame.tick.index);
            tracked += u64::from(frame.tracking == Tracking::Tracking);
            assert_eq!(scene.scene().frame.index, out.tick.index, "the scene names the frame it rendered");
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let report = perform.stop().expect("the perform thread stops cleanly");
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    scene.release(&mut renderer).expect("the stage releases");

    let expected = RUN.as_secs_f64() * 60.0;
    let summary = format!(
        "60 Hz run, {} s: perform passes that stepped {}, steps skipped {}, scenes rendered {}, scenes with a tracked \
         face {}, allocations on the perform thread after {} s of warm-up: {}",
        RUN.as_secs(),
        report.published,
        report.skipped,
        scenes,
        tracked,
        WARM_UP_S,
        after - warm_allocations.unwrap_or(after),
    );
    println!("{summary}");
    if let Some(path) = std::env::var_os("OMUZAKI_LIVE_WIRING_REPORT") {
        // The evidence file the PR quotes; a test writes it only when asked.
        #[allow(clippy::disallowed_methods)]
        std::fs::write(path, &summary).expect("the report writes");
    }
    // The perform loop kept its 60 Hz: every step ran or was counted as skipped, and skips are rare (no storm).
    assert!((report.published + report.skipped) as f64 >= expected * 0.97, "{summary}");
    assert!(report.skipped as f64 <= expected * 0.01, "{summary}");
    assert!(report.published as f64 >= expected * 0.9, "{summary}");
    // The render side drew a new scene for nearly every step: newest-wins drops only what a slow render misses.
    assert!(scenes as f64 >= expected * 0.8, "{summary}");
    assert!(tracked as f64 >= expected * 0.7, "the tracked face reached the scene: {summary}");
    assert!(ARMED.load(Ordering::Acquire), "the perform thread counted its allocations");
    assert_eq!(warm_allocations.map(|before| after - before), Some(0), "{summary}");
}

#[test]
fn the_counter_sees_an_allocation_on_a_measuring_thread() {
    let counted = std::thread::spawn(|| {
        MEASURING.with(|m| m.set(true));
        let before = ALLOCATIONS.load(Ordering::Relaxed);
        let boxed = std::hint::black_box(Box::new([0_u8; 64]));
        drop(boxed);
        MEASURING.with(|m| m.set(false));
        ALLOCATIONS.load(Ordering::Relaxed) - before
    });
    assert!(counted.join().expect("the thread finishes") >= 1);
}

/// The perform thread plus the real executor with the thread's `PerformHost` in its services, as the app wires them.
struct Live {
    perform: PerformThread,
    reader: FrameReader,
    runner: Executor,
}

impl Live {
    fn start(source: Box<dyn FrameSource>) -> Self {
        omuzaki_ui::link();
        let performer = live_performer(source, ChannelBuf::NEUTRAL, HumanoidPose::REST);
        let (perform, reader) = PerformThread::spawn(performer, Box::new(RealClock::new())).expect("it starts");
        let services = ServicesBuilder::new().insert::<dyn PerformHost>(perform.host()).build();
        let ops = [
            "perform.calibrate",
            "perform.set_retarget",
            "perform.status",
            "trigger.bind",
            "trigger.press",
            "trigger.list",
        ]
        .map(|op| find(op).expect("linked"));
        let runner = Executor::with_registrations(Box::new(MemDoc::new()), Arc::new(FakeClock::new(Seconds(0.0))), ops)
            .expect("an executor")
            .with_services(services);
        Self { perform, reader, runner }
    }

    fn run(&self, op: &str, args: Value) -> Value {
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
        self.runner.run(request).unwrap_or_else(|e| panic!("{op}: {e:?}")).plan.output.unwrap_or(Value::Null)
    }

    fn status(&self) -> Value {
        self.run("perform.status", args(&[]))
    }
}

fn args(pairs: &[(&str, Value)]) -> Value {
    Value::Object(pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect())
}

#[test]
fn smoothing_calibrate_and_a_trigger_each_change_the_running_loop() {
    let mut live = Live::start(camera(30.0));
    assert!(eventually(Duration::from_secs(3), || live.status()["tracking"] == "tracking"), "the face is tracked");

    // Smoothing: the op queues it, and the running performer applies it.
    let receipt = live.run("perform.set_retarget", args(&[("smoothing", Value::from(0.875))]));
    let seq = receipt["seq"].as_u64().expect("a receipt");
    assert!(
        eventually(Duration::from_secs(2), || {
            let status = live.status();
            status["retarget_seq"].as_u64() >= Some(seq) && status["retarget"]["smoothing"] == 0.875
        }),
        "the performer applied the smoothing: {}",
        live.status()
    );

    // Calibrate: a 30-frame neutral capture over the tracked face finishes on the running loop.
    live.run("perform.calibrate", args(&[("frames", Value::from(30))]));
    assert!(
        eventually(Duration::from_secs(5), || {
            serde_json_state(&live.status()["calibration"]) == Some(CalibrationState::Done { frames: 30 })
        }),
        "the capture finished: {}",
        live.status()
    );

    // A trigger, through the trigger ops: `F1` held shows the starter set's `angry` (brows down 0.8, which the synthetic
    // track never drives) on the published frames; released, it fades out.
    live.perform.host().set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).expect("queued");
    let bind = [("key", Value::from("F1")), ("expression", Value::from("angry")), ("mode", Value::from("hold"))];
    live.run("trigger.bind", args(&bind));
    let brow = |frame: LiveFrame| frame.channels[Channel::BROW_DOWN_LEFT];
    let reader = &mut live.reader;
    assert!(
        eventually(Duration::from_secs(1), || reader.take_new().is_some_and(|f| brow(f) < 0.1)),
        "the brows are not down before the press"
    );
    live.run("trigger.press", args(&[("key", Value::from("F1")), ("down", Value::from(true))]));
    let reader = &mut live.reader;
    assert!(
        eventually(Duration::from_secs(2), || reader.take_new().is_some_and(|f| (brow(f) - 0.8).abs() < 0.05)),
        "the held trigger reached the published frames"
    );
    assert_eq!(live.run("trigger.list", args(&[]))["triggers"][0]["active"], true, "the loop shows it as active");
    live.run("trigger.press", args(&[("key", Value::from("F1")), ("down", Value::from(false))]));
    let reader = &mut live.reader;
    assert!(
        eventually(Duration::from_secs(2), || reader.take_new().is_some_and(|f| brow(f) < 0.1)),
        "the released trigger faded out"
    );

    let report = live.perform.stop().expect("a clean stop");
    assert!(report.published > 0);
    // The ops see the stopped performer as unavailable, not as a hang.
    let request = Request {
        op: "perform.status".to_owned(),
        args: args(&[]),
        mode: Mode::Apply,
        actor: Actor::Person { name: "performer".into() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    assert!(live.runner.run(request).is_ok(), "status still answers, from the last published state");
}

fn serde_json_state(value: &Value) -> Option<CalibrationState> {
    match value["state"].as_str()? {
        "done" => Some(CalibrationState::Done { frames: value["frames"].as_u64()? as usize }),
        "idle" => Some(CalibrationState::Idle),
        _ => None,
    }
}

#[test]
fn an_unplugged_camera_hands_the_face_to_life_and_the_loop_keeps_publishing() {
    // The camera's frames end after 1 s, as an unplug would end them.
    let mut live = Live::start(camera(1.0));
    let reader = &mut live.reader;
    assert!(
        eventually(Duration::from_secs(2), || reader.take_new().is_some_and(|f| f.tracking == Tracking::Tracking)),
        "tracked first"
    );
    assert!(
        eventually(Duration::from_secs(3), || reader.take_new().is_some_and(|f| f.tracking == Tracking::Lost)),
        "lost after the unplug"
    );
    let before = reader.published();
    std::thread::sleep(Duration::from_millis(300));
    assert!(reader.published() > before + 10, "the life layer keeps the loop publishing");
    live.perform.stop().expect("a clean stop");
}

#[test]
fn the_rig_starts_on_no_camera_switches_cameras_and_shuts_down_in_order() {
    omuzaki_ui::link();
    let performer = live_performer(Box::new(NoCamera), ChannelBuf::NEUTRAL, HumanoidPose::REST);
    let (mut rig, mut reader) =
        LiveRig::start(Box::new(tracker(30.0)), TrackerConfig::default(), performer, Box::new(RealClock::new()))
            .expect("the rig starts");
    // Before any camera, the life layer runs: frames arrive, with no face.
    assert!(eventually(Duration::from_secs(2), || reader.take_new().is_some_and(|f| f.tracking == Tracking::NoFrames)));
    assert_eq!(rig.cameras().len(), 1);

    // A camera the tracker doesn't have is refused and the loop stays on life.
    let missing = omuzaki_track_api::camera::CameraId("missing".into());
    assert!(rig.use_camera(&missing).is_err());
    assert_eq!(rig.camera(), None);

    rig.use_camera(&FakeCamera::id()).expect("the fake camera starts");
    assert_eq!(rig.camera(), Some(&FakeCamera::id()));
    assert!(eventually(Duration::from_secs(2), || reader.take_new().is_some_and(|f| f.tracking != Tracking::NoFrames)));

    rig.stop_camera().expect("stopped");
    assert_eq!(rig.camera(), None);
    let host = rig.host().expect("a host while running");
    let report = rig.shutdown().expect("a clean shutdown");
    assert!(report.published > 0);
    // After the quit order the ops' handle refuses instead of waiting on a thread that is gone.
    assert!(host.calibrate(30).is_err());
}

#[test]
fn the_perform_thread_is_named_and_stops_when_dropped() {
    omuzaki_ui::link();
    let performer = live_performer(Box::new(NoCamera), ChannelBuf::NEUTRAL, HumanoidPose::REST);
    let (perform, reader) = PerformThread::spawn(performer, Box::new(RealClock::new())).expect("it starts");
    assert!(perform.is_running());
    assert!(eventually(Duration::from_secs(1), || reader.published() > 0));
    let host = perform.host();
    drop(perform);
    let published = reader.published();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(reader.published(), published, "nothing publishes after the drop joined the thread");
    assert!(host.calibrate(30).is_err(), "the handle sees the performer gone");
    assert_eq!(THREAD_NAME, "omuzaki-perform");
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

/// What `main.rs` starts: every linked op over the app's runner, the starter expressions on the triggers, a camera
/// picked on the rig reaching the ops' status, and the quit order.
#[test]
fn the_app_starts_the_live_slice_with_its_ops_and_stops_it_in_order() {
    omuzaki_ui::link();
    let mut app =
        LiveApp::start(Box::new(tracker(30.0)), Box::new(RealClock::new()), Transparency::Fallback).expect("it starts");
    let runner = app.runner();
    let ops: Vec<String> = runner.ops().into_iter().map(|op| op.name.to_owned()).collect();
    for op in [
        "perform.calibrate",
        "perform.set_retarget",
        "perform.status",
        "trigger.bind",
        "trigger.press",
        "output.window.open",
        "output.window.close",
        "output.window.status",
    ] {
        assert!(ops.iter().any(|name| name == op), "{op} is linked into the app's runner");
    }
    // The output window's host is in the services: a dry-run plans the exact output size, and on a platform without a
    // verified transparent surface a transparent window falls back to the key and says so.
    let planned = runner
        .run(Request {
            mode: Mode::DryRun,
            ..request("output.window.open", args(&[("backdrop", Value::from("transparent"))]))
        })
        .expect("the host is wired")
        .plan
        .output
        .expect("an answer");
    assert_eq!(
        (planned["physical"].clone(), planned["fallback"].clone()),
        (serde_json::json!([1920, 1080]), Value::from(true))
    );
    let closed =
        runner.run(request("output.window.status", args(&[]))).expect("status").plan.output.expect("an answer");
    assert_eq!(closed["state"], "closed", "a dry run opens nothing");
    let bind = args(&[("key", Value::from("F1")), ("expression", Value::from("happy"))]);
    runner.run(request("trigger.bind", bind)).expect("the starter expressions are loaded");

    let rig = app.rig_mut().expect("running");
    rig.use_camera(&FakeCamera::id()).expect("the fake camera starts");
    let status = || runner.run(request("perform.status", args(&[]))).expect("status").plan.output.expect("an answer");
    assert!(eventually(Duration::from_secs(3), || status()["tracking"] == "tracking"), "the camera reaches the ops");
    assert!(app.reader_mut().expect("running").take_new().is_some(), "the render side gets frames");
    assert!(!app.output_mut().status().open, "the output window starts closed");

    let report = app.shutdown().expect("a clean shutdown");
    assert!(report.published > 0);
    assert!(app.rig_mut().is_none() && app.reader_mut().is_none(), "the threads are gone");
    assert_eq!(app.shutdown().expect("again"), LoopReport::default(), "a second shutdown does nothing");
    let refused = runner.run(request("trigger.press", args(&[("key", Value::from("F1"))])));
    assert!(refused.is_err(), "the stopped performer refuses requests instead of hanging");
}

#[test]
fn without_a_webcam_tracker_the_app_lists_no_cameras_and_says_so_on_a_pick() {
    let mut app =
        LiveApp::start(Box::new(NoTracker), Box::new(RealClock::new()), Transparency::Fallback).expect("it starts");
    let rig = app.rig_mut().expect("running");
    assert_eq!(rig.cameras(), Vec::new());
    let picked = rig.use_camera(&FakeCamera::id());
    assert!(matches!(picked, Err(LiveError::Track(TrackError::NoDevice(_)))), "{picked:?}");
    assert_eq!(rig.camera(), None);
}
