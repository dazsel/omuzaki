//! The webcam tracker over `omuzaki-track-mock`'s `FakeCamera` and a scripted `FrameTracker`: statuses and `seq` on the
//! consumer side, every pixel format, errors, stop and start, the preview tap, and zero allocations on the camera thread.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::float_cmp
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_anim_api::{ARKIT_COUNT, ChannelMask, FrameSource, TrackFrame, TrackStatus};
use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_track::webcam::{
    Failures, FrameConverter, FrameTracker, PreviewFrame, WebcamConfig, WebcamTracker, tier_camera, tier_stride,
};
use omuzaki_track::{FaceSample, FaceTrackError, NormRect, RgbFrame, StageTimings};
use omuzaki_track_api::camera::{
    Camera, CameraFormat, CameraFormatRequest, CameraFrame, CameraId, CameraInfo, CameraStream, DeviceTime, PixelFormat,
};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::tracker::{QualityTier, Tracker, TrackerConfig};
use omuzaki_track_mock::FakeCamera;

struct Counting;

/// What the camera thread allocated while a test marked it (`Scripted::measure`).
static CAMERA_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    // Set by the scripted tracker on the camera thread, so tests running side by side cannot count each other's.
    static ON_CAMERA: Cell<bool> = const { Cell::new(false) };
    // What the measuring thread itself allocated, for tests that measure their own thread.
    static THREAD_ALLOCS: Cell<usize> = const { Cell::new(0) };
    static THREAD_BYTES: Cell<usize> = const { Cell::new(0) };
    static THREAD_LARGEST: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ON_CAMERA.try_with(Cell::get).unwrap_or(false) {
            CAMERA_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            THREAD_ALLOCS.with(|c| c.set(c.get() + 1));
            THREAD_BYTES.with(|c| c.set(c.get() + layout.size()));
            THREAD_LARGEST.with(|c| c.set(c.get().max(layout.size())));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const LANDMARK: [f32; 3] = [0.5, 0.25, 0.0];

/// How long a test waits for something that happens as soon as the machine lets it. Only ever a backstop against a
/// hang: no test passes or fails by being faster or slower than this.
const BACKSTOP: Duration = Duration::from_secs(60);

fn face() -> FaceSample {
    let mut pose = [0.0; 16];
    pose[0] = 1.0;
    pose[5] = 1.0;
    pose[10] = 1.0;
    pose[15] = 1.0;
    FaceSample {
        blendshapes: [0.0; ARKIT_COUNT],
        present: ChannelMask::EMPTY,
        pose,
        landmarks: [LANDMARK; 478],
        presence: 1.0,
        roi: NormRect { x_center: 0.5, y_center: 0.5, width: 0.5, height: 0.5, rotation: 0.0 },
    }
}

/// A scripted tracker: `faces[i]` says whether frame `i` has a face (the last entry repeats). With a gate it waits for
/// a token before answering each frame, so a test steps the thread one frame at a time. It can record the RGB it was
/// given, count resets, and mark the camera thread as measuring for the counting allocator over a range of frames.
struct Scripted {
    faces: Vec<bool>,
    gate: Option<Receiver<()>>,
    seen: Option<Arc<Mutex<Vec<Vec<u8>>>>>,
    resets: Arc<AtomicU32>,
    measure: Option<std::ops::Range<usize>>,
    /// Sends on this when frame `.0` arrives, so a test can end the session on that frame, not on a timer.
    signal_at: Option<(usize, Sender<()>)>,
    /// Sends on this as each frame arrives, before waiting for its token: the previous frame is then done with.
    waiting: Option<Sender<()>>,
    /// Moves this clock on by this much inside every `track`, as a slow tracker would.
    slow: Option<(Arc<FakeClock>, f64)>,
    /// While set, the gate is open: frames pass without a token, so a test can end a session without a race.
    open: Arc<AtomicBool>,
    calls: usize,
}

impl Scripted {
    fn new(faces: Vec<bool>) -> Self {
        Self {
            faces,
            gate: None,
            seen: None,
            resets: Arc::default(),
            measure: None,
            signal_at: None,
            waiting: None,
            slow: None,
            open: Arc::default(),
            calls: 0,
        }
    }

    fn gated(faces: Vec<bool>) -> (Self, Sender<()>) {
        let (tx, rx) = channel();
        (Self { gate: Some(rx), ..Self::new(faces) }, tx)
    }
}

impl FrameTracker for Scripted {
    fn track(
        &mut self,
        frame: &RgbFrame<'_>,
        _: Seconds,
    ) -> Result<(Option<FaceSample>, StageTimings), FaceTrackError> {
        if let Some((clock, by)) = &self.slow {
            clock.advance(Seconds(*by));
        }
        if let Some(waiting) = &self.waiting {
            waiting.send(()).ok();
        }
        if let Some(gate) = &self.gate {
            // A token per frame; a test that fails early must not hang the join in `Drop`, so this gives up in time.
            let give_up = Instant::now() + BACKSTOP / 2;
            while !self.open.load(Ordering::Acquire) && Instant::now() < give_up {
                match gate.recv_timeout(Duration::from_millis(5)) {
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        }
        if let Some(seen) = &self.seen {
            let row = frame.width as usize * 3;
            let pixels =
                (0..frame.height as usize).flat_map(|y| frame.data[y * frame.stride..][..row].to_vec()).collect();
            seen.lock().unwrap().push(pixels);
        }
        if let Some(range) = &self.measure {
            ON_CAMERA.with(|m| m.set(range.contains(&self.calls)));
        }
        if let Some((_, signal)) = self.signal_at.as_ref().filter(|(at, _)| *at == self.calls) {
            signal.send(()).ok();
        }
        let has_face = self.faces.get(self.calls).or(self.faces.last()).copied().unwrap_or(false);
        self.calls += 1;
        Ok((has_face.then(face), StageTimings::default()))
    }

    fn reset(&mut self) {
        self.resets.fetch_add(1, Ordering::Relaxed);
    }
}

fn next(source: &mut dyn FrameSource) -> TrackFrame {
    let deadline = Instant::now() + BACKSTOP;
    while Instant::now() < deadline {
        if let Some(frame) = source.poll(Seconds(0.0)) {
            return frame;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("no frame within {BACKSTOP:?}");
}

fn wait_for_error(tracker: &WebcamTracker) -> TrackError {
    let deadline = Instant::now() + BACKSTOP;
    while Instant::now() < deadline {
        if let Some(error) = tracker.error() {
            return error;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the camera thread did not end");
}

fn nv12(width: u32, height: u32) -> CameraFormat {
    CameraFormat::new(width, height, 30, PixelFormat::Nv12)
}

/// A stage clock that stands still unless a test moves it. A test that steps the camera thread by hand takes as long
/// as the machine makes it, and on a real clock a slow one would see the watchdog's lost frame between its steps.
fn standing_clock() -> Arc<dyn Clock> {
    Arc::new(FakeClock::new(Seconds(100.0)))
}

fn webcam(camera: FakeCamera, tracker: Scripted, config: WebcamConfig) -> WebcamTracker {
    webcam_on(camera, tracker, config, standing_clock())
}

fn webcam_on(camera: FakeCamera, tracker: Scripted, config: WebcamConfig, clock: Arc<dyn Clock>) -> WebcamTracker {
    patient(Box::new(camera), Box::new(tracker), config, clock)
}

/// A tracker that waits `BACKSTOP` for its thread at `stop`: a thread that is merely slow, on a loaded machine, must
/// not be taken for a stuck one. Only the test of a tracker that never returns asks for a short wait.
fn patient(
    camera: Box<dyn Camera>,
    tracker: Box<dyn FrameTracker>,
    config: WebcamConfig,
    clock: Arc<dyn Clock>,
) -> WebcamTracker {
    let mut tracker = WebcamTracker::new(camera, tracker, config, clock);
    tracker.set_join_grace(BACKSTOP);
    tracker
}

/// Starts with the mode the tracker's own `WebcamConfig` describes.
fn start(tracker: &mut WebcamTracker) -> Box<dyn FrameSource> {
    let config = tracker.tracker_config();
    tracker.start(&FakeCamera::id(), &config).expect("the fake camera opens")
}

/// Polls until the stream's end or failure shows as a lost frame, skipping whatever came before it.
fn until_lost(source: &mut dyn FrameSource) -> TrackFrame {
    loop {
        let frame = next(source);
        if frame.status == TrackStatus::Lost {
            return frame;
        }
    }
}

#[test]
fn a_face_then_none_for_twenty_frames_then_a_face_is_tracking_lost_tracking() {
    let mut faces = vec![true];
    faces.extend([false; 20]);
    faces.push(true);
    let (scripted, tokens) = Scripted::gated(faces.clone());
    // One frame more than the script: the thread waits on it for a token, so the stream's end cannot overwrite the
    // last scripted frame before the consumer has polled it.
    let camera = FakeCamera::synthetic(nv12(64, 48), faces.len() as u64 + 1);
    let mut tracker = webcam(camera, scripted, WebcamConfig::default().with_mode(64, 48, 30));
    let mut source = start(&mut tracker);
    let mut seen = Vec::new();
    for _ in &faces {
        tokens.send(()).unwrap();
        seen.push(next(&mut *source));
    }
    let statuses: Vec<_> = seen.iter().map(|f| f.status).collect();
    assert_eq!(statuses[0], TrackStatus::Tracking);
    assert!(statuses[1..=20].iter().all(|s| *s == TrackStatus::Lost), "{statuses:?}");
    assert_eq!(statuses[21], TrackStatus::Tracking);
    assert!(seen.windows(2).all(|w| w[0].seq < w[1].seq && w[0].time < w[1].time));
    // The extra frame goes out, then the fake camera runs dry: a lost frame, and the reason is kept. The extra frame
    // may or may not be seen first, so wait for the Lost and join the thread before reading the counters.
    tokens.send(()).unwrap();
    until_lost(&mut *source);
    tracker.stop();
    assert_eq!(tracker.error(), Some(TrackError::Lost));
    let latency = tracker.latency();
    assert_eq!((latency.frames, latency.dropped), (faces.len() as u64 + 2, 0));
    assert_eq!(tracker.failures(), Failures::default());
}

/// Per-pixel luma and per-2x2-block chroma, so NV12 and YUY2 hold the same picture.
fn yuv_at(x: u32, y: u32) -> [u8; 3] {
    [(20 + (x * 3 + y * 5) % 200) as u8, (60 + (x / 2 * 7 + y / 2) % 120) as u8, (70 + (x / 2 + y / 2 * 5) % 110) as u8]
}

/// BT.601 limited range in floating point, independent of the converter's fixed-point maths.
fn rgb_of(yuv: [u8; 3]) -> [u8; 3] {
    let [y, u, v] = yuv.map(f32::from);
    let y = 1.164_383 * (y - 16.0);
    let (u, v) = (u - 128.0, v - 128.0);
    [y + 1.596_027 * v, y - 0.391_762 * u - 0.812_968 * v, y + 2.017_232 * u].map(|c| c.round().clamp(0.0, 255.0) as u8)
}

fn picture(pixel: PixelFormat, (w, h): (u32, u32)) -> Vec<u8> {
    let at = |x, y| yuv_at(x, y);
    let mut data = Vec::new();
    match pixel {
        PixelFormat::Nv12 => {
            data.extend((0..h).flat_map(|y| (0..w).map(move |x| at(x, y)[0])));
            data.extend(
                (0..h / 2).flat_map(|by| (0..w / 2).flat_map(move |bx| [at(bx * 2, by * 2)[1], at(bx * 2, by * 2)[2]])),
            );
        }
        PixelFormat::Yuy2 => {
            data.extend((0..h).flat_map(|y| {
                (0..w / 2).flat_map(move |bx| {
                    let (a, b) = (at(bx * 2, y), at(bx * 2 + 1, y));
                    [a[0], a[1], b[0], a[2]]
                })
            }));
        }
        PixelFormat::Rgb8 => data.extend((0..h).flat_map(|y| (0..w).flat_map(move |x| rgb_of(at(x, y))))),
        PixelFormat::Bgra8 => {
            data.extend((0..h).flat_map(|y| {
                (0..w).flat_map(move |x| {
                    let [r, g, b] = rgb_of(at(x, y));
                    [b, g, r, 255]
                })
            }));
        }
        other => panic!("no picture for {other:?}"),
    }
    data
}

#[test]
fn each_pixel_format_reaches_the_tracker_as_the_same_rgb() {
    let size = (64, 48);
    let fixture = |name: &str| {
        std::fs::read(format!("{}/../omuzaki-pixfmt/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    };
    let expected: Vec<u8> = (0..size.1).flat_map(|y| (0..size.0).flat_map(move |x| rgb_of(yuv_at(x, y)))).collect();
    let cases = [
        (PixelFormat::Nv12, picture(PixelFormat::Nv12, size), expected.clone(), 1),
        (PixelFormat::Yuy2, picture(PixelFormat::Yuy2, size), expected.clone(), 1),
        (PixelFormat::Rgb8, picture(PixelFormat::Rgb8, size), expected.clone(), 1),
        (PixelFormat::Bgra8, picture(PixelFormat::Bgra8, size), expected, 1),
        (PixelFormat::Mjpg, fixture("64x48_420.jpg"), fixture("64x48_420.rgb"), 3),
    ];
    for (pixel, data, want, tolerance) in cases {
        let format = CameraFormat::new(size.0, size.1, 30, pixel);
        let camera =
            FakeCamera::from_frames(format, vec![(DeviceTime(0), data.clone()), (DeviceTime(33_000_000), data)]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let scripted = Scripted { seen: Some(Arc::clone(&seen)), ..Scripted::new(vec![false]) };
        // The default preference lists NV12 first: the other formats are reached by falling through it.
        let mut tracker = webcam(camera, scripted, WebcamConfig::default().with_mode(size.0, size.1, 30));
        let _source = start(&mut tracker);
        assert_eq!(wait_for_error(&tracker), TrackError::Lost, "{pixel:?}");
        let frames = seen.lock().unwrap();
        assert_eq!(frames.len(), 2, "{pixel:?}");
        for got in frames.iter() {
            let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(got.len() == want.len() && worst <= tolerance, "{pixel:?}: off by {worst}");
        }
    }
}

#[test]
fn a_camera_error_mid_stream_ends_the_thread_with_a_lost_frame_and_the_error() {
    let format = nv12(64, 48);
    let good = picture(PixelFormat::Nv12, (64, 48));
    // The third frame is too short for its layout: the driver failed.
    let frames =
        vec![(DeviceTime(0), good.clone()), (DeviceTime(33_000_000), good), (DeviceTime(66_000_000), vec![0; 10])];
    let (scripted, tokens) = Scripted::gated(vec![true]);
    let mut tracker =
        webcam(FakeCamera::from_frames(format, frames), scripted, WebcamConfig::default().with_mode(64, 48, 30));
    let mut source = start(&mut tracker);
    tokens.send(()).unwrap();
    assert_eq!(next(&mut *source).status, TrackStatus::Tracking);
    tokens.send(()).unwrap();
    // The second frame and the failure follow each other at once, so the consumer may miss the first of the two.
    until_lost(&mut *source);
    assert!(matches!(tracker.error(), Some(TrackError::Backend(_))), "{:?}", tracker.error());
    tracker.stop();
}

#[test]
fn stop_then_start_works_and_the_old_source_goes_quiet() {
    let scripted = Scripted::new(vec![true]);
    let resets = Arc::clone(&scripted.resets);
    let camera = FakeCamera::synthetic(nv12(64, 48), 10_000_000);
    let mut tracker = webcam(camera, scripted, WebcamConfig::default().with_mode(64, 48, 30));
    assert_eq!(tracker.devices().len(), 1);
    assert_eq!(tracker.default_device(), Some(FakeCamera::id()));
    let mut first = start(&mut tracker);
    let before = next(&mut *first);
    // How long `stop` takes is bounded by the camera thread's poll interval, which `thread.rs` checks against the
    // 100 ms budget at compile time; a stopwatch here would only measure the machine's load.
    tracker.stop();
    assert!(first.poll(Seconds(0.0)).is_none());
    assert!(tracker.latency().frames > 0);
    tracker.stop(); // a no-op when stopped

    let mut second = start(&mut tracker);
    let after = next(&mut *second);
    assert_eq!(after.status, TrackStatus::Tracking);
    assert!(after.seq > before.seq, "{} then {}", before.seq, after.seq);
    // Starting again stops the session before it, and its source stays silent.
    let _third = start(&mut tracker);
    assert!(second.poll(Seconds(0.0)).is_none());
    drop(tracker);
    assert_eq!(resets.load(Ordering::Relaxed), 3);
}

/// One frame, then silence that costs no wall time: once `quiet` is set, each empty wait moves the stage clock on
/// by the timeout. Before that the camera just has nothing yet, and time stands still.
struct Stall {
    data: Vec<u8>,
    clock: Arc<FakeClock>,
    idle: Arc<AtomicUsize>,
    quiet: Arc<AtomicBool>,
    /// Frames before the silence.
    frames: u64,
}
struct StallStream(Stall, u64);
impl Camera for Stall {
    fn enumerate(&self) -> Vec<CameraInfo> {
        vec![CameraInfo::new(FakeCamera::id(), "stall", vec![nv12(64, 48)])]
    }

    fn open(&mut self, _: &CameraId, _: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
        let (clock, idle, quiet) = (Arc::clone(&self.clock), Arc::clone(&self.idle), Arc::clone(&self.quiet));
        Ok(Box::new(StallStream(Stall { data: self.data.clone(), clock, idle, quiet, frames: self.frames }, 0)))
    }
}
impl CameraStream for StallStream {
    fn format(&self) -> CameraFormat {
        nv12(64, 48)
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
        if self.1 >= self.0.frames {
            if self.0.quiet.load(Ordering::Acquire) {
                self.0.clock.advance(Seconds(timeout.as_secs_f64()));
                self.0.idle.fetch_add(1, Ordering::Relaxed);
            }
            std::thread::yield_now();
            return Ok(None);
        }
        self.1 += 1;
        let seq = self.1 - 1;
        Ok(Some(CameraFrame::new(&self.0.data, 64, 48, 64, PixelFormat::Nv12, DeviceTime(seq * 33_000_000), seq)))
    }
}

#[test]
fn a_camera_that_goes_quiet_gives_one_lost_frame_after_lost_after() {
    let (clock, idle) = (Arc::new(FakeClock::new(Seconds(10.0))), Arc::new(AtomicUsize::new(0)));
    let quiet = Arc::new(AtomicBool::new(false));
    let camera = Stall {
        data: picture(PixelFormat::Nv12, (64, 48)),
        clock: Arc::clone(&clock),
        idle: Arc::clone(&idle),
        quiet: Arc::clone(&quiet),
        frames: 1,
    };
    // Gated, and quiet only once the consumer has the first frame, which a lost frame would otherwise overwrite.
    let (scripted, tokens) = Scripted::gated(vec![true]);
    let mut tracker =
        patient(Box::new(camera), Box::new(scripted), WebcamConfig::default().with_mode(64, 48, 30), clock);
    let config = TrackerConfig::default().with_lost_after(Duration::from_millis(30));
    let mut source = tracker.start(&FakeCamera::id(), &config).unwrap();
    tokens.send(()).unwrap();
    assert_eq!(next(&mut *source).status, TrackStatus::Tracking);
    quiet.store(true, Ordering::Release);
    assert_eq!(next(&mut *source).status, TrackStatus::Lost);
    // Plenty more silence goes by, many times `lost_after`, and no second lost frame comes.
    let after = idle.load(Ordering::Relaxed) + 100;
    while idle.load(Ordering::Relaxed) < after {
        std::thread::yield_now();
    }
    assert!(source.poll(Seconds(0.0)).is_none(), "only one lost frame per silence");
}

/// The stall rule's `seq`s, from the fake tracker and from the real one over a camera that goes quiet after two frames.
#[test]
fn a_stall_repeats_the_last_seq_as_the_fake_tracker_does() {
    let lost_after = Duration::from_millis(30);
    let profile = omuzaki_anim_api::DeviceProfile::Webcam;
    let frame_at = |seq: u64, at: f64| TrackFrame::empty(seq, Seconds(at), profile);
    let script = vec![(Duration::ZERO, frame_at(0, 0.0)), (Duration::from_millis(33), frame_at(1, 0.033))];
    let mut oracle = omuzaki_track_mock::FakeTracker::scripted(script);
    let config = TrackerConfig::default().with_lost_after(lost_after);
    let mut oracle_source = oracle.start(&FakeCamera::id(), &config).expect("the fake tracker starts");
    let want: Vec<u64> = [0.0, 0.033, 0.033 + lost_after.as_secs_f64() + 0.001]
        .iter()
        .map(|now| oracle_source.poll(Seconds(*now)).expect("a frame is due").seq)
        .collect();
    assert_eq!(want, [0, 1, 1], "the fake tracker repeats the last seq for the stall");

    let (clock, quiet) = (Arc::new(FakeClock::new(Seconds(10.0))), Arc::new(AtomicBool::new(false)));
    let camera = Stall {
        data: picture(PixelFormat::Nv12, (64, 48)),
        clock: Arc::clone(&clock),
        idle: Arc::default(),
        quiet: Arc::clone(&quiet),
        frames: 2,
    };
    let (scripted, tokens) = Scripted::gated(vec![true]);
    let mut tracker = patient(Box::new(camera), Box::new(scripted), WebcamConfig::default(), clock);
    let mut source = tracker.start(&FakeCamera::id(), &config).unwrap();
    let mut got = Vec::new();
    for _ in 0..2 {
        tokens.send(()).unwrap();
        got.push(next(&mut *source).seq);
    }
    quiet.store(true, Ordering::Release);
    let lost = next(&mut *source);
    assert_eq!(lost.status, TrackStatus::Lost);
    got.push(lost.seq);
    assert_eq!(got, want, "the webcam tracker gives the same seqs for the same gap");
    drop(tokens);
}

/// A tracker that does not return for twice `lost_after`: one lost frame with the last seq, then it carries on.
#[test]
fn a_slow_tracker_gives_one_lost_frame_then_resumes() {
    let clock = Arc::new(FakeClock::new(Seconds(10.0)));
    let lost_after = Duration::from_millis(50);
    let (scripted, tokens) = Scripted::gated(vec![true]);
    let camera = FakeCamera::synthetic(nv12(64, 48), 10_000_000);
    let mut tracker = webcam_on(camera, scripted, WebcamConfig::default().with_mode(64, 48, 30), clock.clone());
    let config = tracker.tracker_config().with_lost_after(lost_after);
    let mut source = tracker.start(&FakeCamera::id(), &config).unwrap();
    tokens.send(()).unwrap();
    let first = next(&mut *source);
    assert_eq!(first.status, TrackStatus::Tracking);
    // The tracker is now inside `track` for the next frame, and stays there while the stage clock runs on.
    clock.advance(Seconds(lost_after.as_secs_f64() * 2.0));
    let lost = next(&mut *source);
    assert_eq!((lost.status, lost.seq), (TrackStatus::Lost, first.seq));
    // More silence, many times `lost_after`, gives no second notice.
    clock.advance(Seconds(5.0));
    std::thread::sleep(Duration::from_millis(100));
    assert!(source.poll(Seconds(0.0)).is_none(), "one notice per silence");
    // The tracker comes back: the frame it was on goes out, after the notice.
    tokens.send(()).unwrap();
    let resumed = next(&mut *source);
    assert_eq!((resumed.status, resumed.seq), (TrackStatus::Tracking, first.seq + 1));
    assert!(resumed.time > lost.time);
    drop(tokens);
    tracker.stop();
    assert_eq!(tracker.error(), None);
    // The frame was captured 5.1 s before it went out, give or take a frame time. Its output was ordered after the
    // lost frame, 5 s before; the latency is from the capture.
    assert!(tracker.latency().max >= Duration::from_millis(5050), "{:?}", tracker.latency());
}

/// A tracker that never returns: a lost frame, and `stop` gives up on it instead of hanging.
#[test]
fn a_tracker_that_never_returns_gives_a_lost_frame_and_stop_detaches_it() {
    let clock = Arc::new(FakeClock::new(Seconds(10.0)));
    let (scripted, tokens) = Scripted::gated(vec![true]);
    let camera = FakeCamera::synthetic(nv12(64, 48), 10_000_000);
    let mut tracker = webcam_on(camera, scripted, WebcamConfig::default().with_mode(64, 48, 30), clock.clone());
    tracker.set_join_grace(Duration::from_millis(50));
    let config = tracker.tracker_config().with_lost_after(Duration::from_millis(50));
    let mut source = tracker.start(&FakeCamera::id(), &config).unwrap();
    tokens.send(()).unwrap();
    assert_eq!(next(&mut *source).status, TrackStatus::Tracking);
    clock.advance(Seconds(1.0));
    assert_eq!(next(&mut *source).status, TrackStatus::Lost);
    // No token will come: the thread is stuck in the tracker. `stop` must come back all the same; the bound is
    // checked by construction, so the stopwatch is only a guard against a hang.
    let stopping = Instant::now();
    tracker.stop();
    assert!(stopping.elapsed() < BACKSTOP / 4, "stop took {:?}", stopping.elapsed());
    assert!(
        matches!(tracker.error(), Some(TrackError::Backend(ref why)) if why.contains("let go")),
        "{:?}",
        tracker.error()
    );
    let restart = tracker.start(&FakeCamera::id(), &config);
    assert!(
        matches!(restart, Err(TrackError::Backend(ref why)) if why.contains("lost")),
        "the tracker went with its thread"
    );
    // Let the detached thread go, so it ends and frees the camera.
    drop(tokens);
}

#[test]
fn the_preview_costs_nothing_while_off_and_scales_the_landmarks_while_on() {
    let (arrived, at_gate) = channel();
    let (scripted, tokens) = Scripted::gated(vec![true, true, true, false]);
    let scripted = Scripted { waiting: Some(arrived), ..scripted };
    let camera = FakeCamera::synthetic(nv12(1280, 720), 100);
    let mut tracker = webcam(camera, scripted, WebcamConfig::default());
    let mut source = start(&mut tracker);
    let mut out = PreviewFrame::default();
    // The thread reports each frame as it arrives at the tracker: once frame n has arrived, frame n - 1 is done with,
    // preview included, so what the reader sees below cannot depend on how fast the thread is.
    let wait = || at_gate.recv_timeout(BACKSTOP).expect("the camera thread reached the next frame");

    // Off: two frames through, and nothing was ever copied.
    for _ in 0..2 {
        wait();
        tokens.send(()).unwrap();
        next(&mut *source);
    }
    wait(); // frame 2 has arrived, so frame 1 is done with
    assert!(!tracker.preview(&mut out));

    tracker.set_preview(true);
    tokens.send(()).unwrap();
    let frame = next(&mut *source);
    let deadline = Instant::now() + BACKSTOP;
    while !tracker.preview(&mut out) {
        assert!(Instant::now() < deadline, "no preview");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!((out.width, out.height, out.seq), (640, 360, frame.seq));
    assert_eq!(out.rgb.len(), 640 * 360 * 3);
    let points = out.landmarks.as_ref().expect("a face was found");
    assert!(points.iter().all(|p| *p == [320.0, 90.0]), "landmarks are in preview pixels");
    assert!(!tracker.preview(&mut out), "nothing new since the last call");

    // A frame without a face has a preview without landmarks.
    wait();
    tokens.send(()).unwrap();
    let frame = next(&mut *source);
    while !tracker.preview(&mut out) {
        assert!(Instant::now() < deadline, "no preview");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(out.seq, frame.seq);
    assert!(out.landmarks.is_none());
    // The thread is waiting for the next token; with the sender gone the scripted tracker stops waiting.
    drop(tokens);
}

/// A fake camera whose stream marks the camera thread for the counting allocator over a range of frames, and
/// unmarks it at the end of the range.
struct Metered {
    inner: FakeCamera,
    frames: std::ops::Range<usize>,
}

struct MeteredStream {
    inner: Box<dyn CameraStream>,
    frames: std::ops::Range<usize>,
    calls: usize,
}

impl Camera for Metered {
    fn enumerate(&self) -> Vec<CameraInfo> {
        self.inner.enumerate()
    }

    fn open(&mut self, id: &CameraId, request: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
        let inner = self.inner.open(id, request)?;
        Ok(Box::new(MeteredStream { inner, frames: self.frames.clone(), calls: 0 }))
    }
}

impl CameraStream for MeteredStream {
    fn format(&self) -> CameraFormat {
        self.inner.format()
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
        ON_CAMERA.with(|m| m.set(self.frames.contains(&self.calls)));
        self.calls += 1;
        self.inner.next_frame(timeout)
    }
}

#[test]
fn the_camera_and_tracker_threads_allocate_nothing_over_three_hundred_frames() {
    // Frames 20 to 319 are measured, on the camera thread and on the tracker's, with the preview on, after the buffers
    // have grown. The face comes and goes in runs of two frames and three, so the landmark buffers are lost and
    // reacquired over and over.
    let faces: Vec<bool> = (0..330).map(|i| i % 5 < 2).collect();
    let (done, finished) = channel();
    let scripted = Scripted { measure: Some(20..320), signal_at: Some((320, done)), ..Scripted::new(faces) };
    // The camera never runs dry: the session ends on frame 320, which the tracker reports, so no timer decides when.
    let camera = Metered { inner: FakeCamera::synthetic(nv12(160, 120), u64::MAX), frames: 20..320 };
    let config = WebcamConfig::default().with_mode(160, 120, 30);
    let mut tracker = patient(Box::new(camera), Box::new(scripted), config, standing_clock());
    tracker.set_preview(true);
    let _source = start(&mut tracker);
    finished.recv_timeout(BACKSTOP).expect("the camera thread reached frame 320");
    tracker.stop();
    assert_eq!(CAMERA_ALLOCATIONS.load(Ordering::Relaxed), 0);
}

/// A tracker that takes a quarter of a second: the latency is from capture, so it shows.
#[test]
fn latency_is_measured_from_the_capture_time_whatever_the_output_was_ordered_at() {
    let clock = Arc::new(FakeClock::new(Seconds(100.0)));
    let slow = Scripted { slow: Some((Arc::clone(&clock), 0.25)), ..Scripted::new(vec![true]) };
    // One frame: with more, the clock map would read a clock that runs faster than the device's as growing delay.
    let camera = FakeCamera::synthetic(nv12(64, 48), 1);
    let mut tracker = webcam_on(camera, slow, WebcamConfig::default().with_mode(64, 48, 30), clock);
    let _source = start(&mut tracker);
    wait_for_error(&tracker);
    tracker.stop();
    let latency = tracker.latency();
    assert!(latency.p50 >= Duration::from_millis(250), "{latency:?}");
    assert!(latency.max >= Duration::from_millis(250), "{latency:?}");
}

#[test]
fn the_preview_reader_allocates_nothing_as_the_face_is_lost_and_found() {
    // Two frames with a face, three without, over and over; each preview is read once the camera thread has finished
    // with its frame, so no read can be skipped for finding the slot busy.
    let faces: Vec<bool> = (0..60).map(|i| i % 5 < 2).collect();
    let (arrived, at_gate) = channel();
    let (scripted, tokens) = Scripted::gated(faces.clone());
    let scripted = Scripted { waiting: Some(arrived), ..scripted };
    let camera = FakeCamera::synthetic(nv12(64, 48), 1_000);
    let mut tracker = webcam(camera, scripted, WebcamConfig::default().with_mode(64, 48, 30));
    tracker.set_preview(true);
    let mut source = start(&mut tracker);
    let mut out = PreviewFrame::default();
    let mut allocated = 0;
    for i in 0..=faces.len() {
        at_gate.recv_timeout(BACKSTOP).expect("the camera thread reached the next frame");
        if i > 0 {
            let frame = next(&mut *source);
            MEASURING.with(|m| m.set(true));
            let before = THREAD_ALLOCS.with(Cell::get);
            let fresh = tracker.preview(&mut out);
            let spent = THREAD_ALLOCS.with(Cell::get) - before;
            MEASURING.with(|m| m.set(false));
            assert!(fresh, "frame {}", i - 1);
            assert_eq!((out.seq, out.landmarks.is_some()), (frame.seq, faces[i - 1]), "frame {}", i - 1);
            // Frames 0 to 9 warm every buffer up: the thread's two, and ours.
            if i > 10 {
                allocated += spent;
            }
        }
        if i < faces.len() {
            tokens.send(()).unwrap();
        }
    }
    assert_eq!(allocated, 0);
    drop(tokens);
}

#[test]
fn reading_the_latency_allocates_nothing() {
    let camera = FakeCamera::synthetic(nv12(64, 48), 10_000_000);
    let mut tracker = webcam(camera, Scripted::new(vec![true]), WebcamConfig::default().with_mode(64, 48, 30));
    let _source = start(&mut tracker);
    while tracker.latency().frames < 50 {
        std::thread::sleep(Duration::from_millis(1));
    }
    MEASURING.with(|m| m.set(true));
    let before = THREAD_ALLOCS.with(Cell::get);
    for _ in 0..100 {
        std::hint::black_box(tracker.latency());
    }
    let allocated = THREAD_ALLOCS.with(Cell::get) - before;
    MEASURING.with(|m| m.set(false));
    assert_eq!(allocated, 0);
    let latency = tracker.latency();
    assert!(latency.p50 <= latency.p95 && latency.p95 <= latency.max.max(Duration::from_micros(100)), "{latency:?}");
}

#[test]
fn mjpg_allocates_nothing_frame_sized() {
    let jpeg = std::fs::read(format!("{}/../omuzaki-pixfmt/tests/fixtures/64x48_420.jpg", env!("CARGO_MANIFEST_DIR")))
        .unwrap();
    let frame = CameraFrame::new(&jpeg, 64, 48, 0, PixelFormat::Mjpg, DeviceTime(0), 0);
    let mut converter = FrameConverter::with_size(64, 48);
    converter.convert(&frame).unwrap();
    MEASURING.with(|m| m.set(true));
    let (calls, bytes) = (THREAD_ALLOCS.with(Cell::get), THREAD_BYTES.with(Cell::get));
    THREAD_LARGEST.with(|c| c.set(0));
    for _ in 0..100 {
        converter.convert(&frame).unwrap();
    }
    let (calls, bytes) = (THREAD_ALLOCS.with(Cell::get) - calls, THREAD_BYTES.with(Cell::get) - bytes);
    MEASURING.with(|m| m.set(false));
    let largest = THREAD_LARGEST.with(Cell::get);
    eprintln!("mjpg 64x48: {} allocations and {} bytes per frame, largest {largest} bytes", calls / 100, bytes / 100);
    // The pixels go into the converter's own buffer: nothing the size of a frame (9216 bytes here) is allocated.
    assert!(largest < 64 * 48 * 3, "largest allocation {largest} bytes");
}

#[test]
fn frames_are_timed_on_the_stage_clock_wherever_tracking_starts() {
    let clock = Arc::new(FakeClock::new(Seconds(1234.5)));
    let (scripted, tokens) = Scripted::gated(vec![true]);
    let open = Arc::clone(&scripted.open);
    let camera = FakeCamera::synthetic(nv12(64, 48), 10_000_000);
    let mut tracker = webcam_on(camera, scripted, WebcamConfig::default().with_mode(64, 48, 30), clock.clone());
    for start_time in [1234.5, 5000.0, 0.25] {
        clock.set(Seconds(start_time));
        let mut source = start(&mut tracker);
        for _ in 0..3 {
            tokens.send(()).unwrap();
            let frame = next(&mut *source);
            // A fixed clock gives the arrival time for every frame; the map only spaces them out by a microsecond.
            assert!((start_time - 1e-9..start_time + 1e-3).contains(&frame.time.0), "{} at {start_time}", frame.time.0);
        }
        // Open the gate for the end of the session, so that whichever frame the thread is on, `stop` is not waiting
        // for a token; close it again for the next.
        open.store(true, Ordering::Release);
        tracker.stop();
        open.store(false, Ordering::Release);
    }
}

/// A camera that records every mode it is asked to open, then behaves as the fake one.
struct Spy {
    inner: FakeCamera,
    requests: Arc<Mutex<Vec<CameraFormatRequest>>>,
}

impl Camera for Spy {
    fn enumerate(&self) -> Vec<CameraInfo> {
        self.inner.enumerate()
    }

    fn open(&mut self, id: &CameraId, request: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
        self.requests.lock().unwrap().push(*request);
        self.inner.open(id, request)
    }
}

fn spy(config: WebcamConfig) -> (WebcamTracker, Arc<Mutex<Vec<CameraFormatRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let inner = FakeCamera::synthetic(CameraFormat::new(64, 48, 30, PixelFormat::Yuy2), 10_000_000);
    let camera = Spy { inner, requests: Arc::clone(&requests) };
    let tracker = patient(Box::new(camera), Box::new(Scripted::new(vec![true])), config, standing_clock());
    (tracker, requests)
}

#[test]
fn start_forwards_the_camera_request_and_refuses_what_it_cannot_do() {
    // A named format is the only one asked for, with the size and rate as given.
    let asked = CameraFormatRequest::new(800, 600, 15).with_pixel(Some(PixelFormat::Yuy2));
    let (mut tracker, requests) = spy(WebcamConfig::default());
    tracker.start(&FakeCamera::id(), &TrackerConfig::default().with_camera(asked)).expect("the fake camera opens");
    assert_eq!(*requests.lock().unwrap(), [asked]);
    tracker.stop();

    // No format named: the preference is tried in order until the camera accepts one.
    let config = WebcamConfig::default().with_formats(vec![PixelFormat::Mjpg, PixelFormat::Yuy2]);
    let (mut tracker, requests) = spy(config);
    let any = CameraFormatRequest::new(800, 600, 15);
    tracker.start(&FakeCamera::id(), &TrackerConfig::default().with_camera(any)).expect("the second format opens");
    let wanted = [PixelFormat::Mjpg, PixelFormat::Yuy2].map(|p| any.with_pixel(Some(p)));
    assert_eq!(*requests.lock().unwrap(), wanted);
    tracker.stop();

    // The mode the WebcamConfig describes is what `tracker_config` asks for.
    let (mut tracker, requests) = spy(WebcamConfig::default().with_mode(64, 48, 25));
    let config = tracker.tracker_config();
    assert_eq!(config.camera, CameraFormatRequest::new(64, 48, 25));
    tracker.start(&FakeCamera::id(), &config).expect("the fake camera opens");
    assert_eq!(requests.lock().unwrap()[0].fps, 25);
    tracker.stop();
}

#[test]
fn each_quality_tier_maps_to_a_camera_size_and_a_tracking_stride() {
    let big = CameraFormatRequest::new(1920, 1080, 60).with_pixel(Some(PixelFormat::Yuy2));
    let small = CameraFormatRequest::new(320, 240, 30);
    // Full is as asked. Lowered tiers cap the size at 640x480, never enlarge it, and keep the rate and the format.
    assert_eq!(tier_camera(QualityTier::Full, &big), big);
    for tier in [QualityTier::Balanced, QualityTier::Light] {
        assert_eq!(tier_camera(tier, &big), CameraFormatRequest::new(640, 480, 60).with_pixel(Some(PixelFormat::Yuy2)));
        assert_eq!(tier_camera(tier, &small), small);
    }
    assert_eq!(tier_stride(QualityTier::Full), 1);
    assert_eq!(tier_stride(QualityTier::Balanced), 1);
    assert_eq!(tier_stride(QualityTier::Light), 2);

    // `start` forwards the camera size each tier asks for.
    for (tier, size) in
        [(QualityTier::Full, (1280, 720)), (QualityTier::Balanced, (640, 480)), (QualityTier::Light, (640, 480))]
    {
        let (mut tracker, requests) = spy(WebcamConfig::default());
        let asked = TrackerConfig::default().with_camera(CameraFormatRequest::new(1280, 720, 30)).with_tier(tier);
        tracker.start(&FakeCamera::id(), &asked).expect("the fake camera opens");
        let first = requests.lock().unwrap()[0];
        assert_eq!((first.width, first.height, first.fps), (size.0, size.1, 30), "{tier:?}");
    }
}

#[test]
fn a_light_tier_tracks_every_second_frame() {
    for (tier, tracked) in [(QualityTier::Full, 10), (QualityTier::Balanced, 10), (QualityTier::Light, 5)] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let scripted = Scripted { seen: Some(Arc::clone(&seen)), ..Scripted::new(vec![true]) };
        let mut tracker = webcam(FakeCamera::synthetic(nv12(64, 48), 10), scripted, WebcamConfig::default());
        let config = tracker.tracker_config().with_tier(tier);
        let mut source = tracker.start(&FakeCamera::id(), &config).expect("the fake camera opens");
        wait_for_error(&tracker);
        tracker.stop();
        assert_eq!(seen.lock().unwrap().len(), tracked, "{tier:?}");
        // The skipped frames are neither published nor counted as drops: the camera's `seq` has no gap.
        assert_eq!(tracker.latency().dropped, 0, "{tier:?}");
        assert_eq!(tracker.latency().frames, tracked as u64 + 1, "{tier:?} (and the end of the stream)");
        assert!(source.poll(Seconds(0.0)).is_none());
    }
}

#[test]
fn a_failed_thread_start_hands_the_tracker_back_and_start_can_be_retried() {
    let resets = Arc::new(AtomicU32::new(0));
    let scripted = Scripted { resets: Arc::clone(&resets), ..Scripted::new(vec![true]) };
    let camera = FakeCamera::synthetic(nv12(64, 48), 10_000_000);
    let mut tracker = webcam(camera, scripted, WebcamConfig::default().with_mode(64, 48, 30));
    // The tracker's thread refused; then the camera thread refused, with the tracker's thread already running, and
    // with no time to spare for it: the tracker must come back whatever the thread's pace.
    tracker.set_join_grace(Duration::ZERO);
    tracker.fail_spawns([true, false, true]);
    for _ in 0..2 {
        let config = tracker.tracker_config();
        let refused = tracker.start(&FakeCamera::id(), &config);
        assert!(matches!(refused, Err(TrackError::Backend(ref e)) if e.contains("thread")), "refused");
    }
    // The tracker came back each time, and the camera was released (the fake one answers `Busy` otherwise).
    tracker.set_join_grace(BACKSTOP);
    let mut source = start(&mut tracker);
    assert_eq!(next(&mut *source).status, TrackStatus::Tracking);
    tracker.stop();
    assert_eq!(tracker.devices().len(), 1);
    drop(tracker);
    assert_eq!(resets.load(Ordering::Relaxed), 3);
}

/// A stream whose camera is gone.
struct Gone;
impl CameraStream for Gone {
    fn format(&self) -> CameraFormat {
        nv12(64, 48)
    }

    fn next_frame(&mut self, _: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
        Err(TrackError::Lost)
    }
}

/// A camera whose first stream fails at once; the next ones are the fake's.
struct FailsFirst {
    inner: FakeCamera,
    failed: bool,
}

impl Camera for FailsFirst {
    fn enumerate(&self) -> Vec<CameraInfo> {
        self.inner.enumerate()
    }

    fn open(&mut self, id: &CameraId, request: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
        if std::mem::replace(&mut self.failed, true) {
            return self.inner.open(id, request);
        }
        Ok(Box::new(Gone))
    }
}

#[test]
fn a_notice_before_the_first_frame_takes_a_seq_so_the_next_session_starts_after_it() {
    let camera = FailsFirst { inner: FakeCamera::synthetic(nv12(64, 48), 10_000_000), failed: false };
    let mut tracker =
        patient(Box::new(camera), Box::new(Scripted::new(vec![true])), WebcamConfig::default(), standing_clock());
    // The camera is gone before its first frame: one lost frame, which is a publication of its own.
    let mut source = start(&mut tracker);
    let gone = next(&mut *source);
    assert_eq!(gone.status, TrackStatus::Lost);
    assert_eq!(wait_for_error(&tracker), TrackError::Lost);
    tracker.stop();
    // A second session: its first real frame must not look like the lost one.
    let mut source = start(&mut tracker);
    let first = next(&mut *source);
    assert_eq!(first.status, TrackStatus::Tracking);
    assert_ne!(first.seq, gone.seq, "two publications share seq {}", gone.seq);
    assert!(first.seq > gone.seq);
}

#[test]
fn a_backwards_device_time_drops_the_frame_and_the_stream_carries_on() {
    let good = picture(PixelFormat::Nv12, (64, 48));
    // The third frame is stamped before the second: dropped, with the fit kept, and the next ones are accepted.
    let ms = [0, 33, 20, 66, 99, 132];
    let frames = ms.iter().map(|ms| (DeviceTime(ms * 1_000_000), good.clone())).collect();
    let (scripted, tokens) = Scripted::gated(vec![true]);
    let camera = FakeCamera::from_frames(nv12(64, 48), frames);
    let mut tracker = webcam(camera, scripted, WebcamConfig::default().with_mode(64, 48, 30));
    let mut source = start(&mut tracker);
    let mut seen = Vec::new();
    for _ in 0..4 {
        tokens.send(()).unwrap();
        seen.push(next(&mut *source));
    }
    // The last frame waits for a token so the end of the script cannot hide the ones before it.
    tokens.send(()).unwrap();
    until_lost(&mut *source);
    tracker.stop();
    assert_eq!(seen.iter().map(|f| f.seq).collect::<Vec<_>>(), [0, 1, 2, 3], "the dropped frame takes no seq");
    assert!(seen.iter().all(|f| f.status == TrackStatus::Tracking));
    assert!(seen.windows(2).all(|w| w[0].time < w[1].time));
    assert_eq!(tracker.failures(), Failures::new(1, 0, 0));
    // A backwards frame is not a skipped one: the camera's own `seq` has no gap.
    assert_eq!(tracker.latency().dropped, 0);
}

/// One good frame, then, once `quiet` is set, nothing but frames stamped before it. Each of those moves the stage clock
/// on by 10 ms, so the silence costs no wall time.
struct Rejected {
    data: Vec<u8>,
    clock: Arc<FakeClock>,
    quiet: Arc<AtomicBool>,
}
struct RejectedStream(Rejected, u64);
impl Camera for Rejected {
    fn enumerate(&self) -> Vec<CameraInfo> {
        vec![CameraInfo::new(FakeCamera::id(), "rejected", vec![nv12(64, 48)])]
    }

    fn open(&mut self, _: &CameraId, _: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
        let (clock, quiet) = (Arc::clone(&self.clock), Arc::clone(&self.quiet));
        Ok(Box::new(RejectedStream(Rejected { data: self.data.clone(), clock, quiet }, 0)))
    }
}
impl CameraStream for RejectedStream {
    fn format(&self) -> CameraFormat {
        nv12(64, 48)
    }

    fn next_frame(&mut self, _: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
        let seq = self.1;
        let captured = match seq {
            0 => DeviceTime(1_000_000_000),
            _ if self.0.quiet.load(Ordering::Acquire) => {
                self.0.clock.advance(Seconds(0.010));
                DeviceTime(500_000_000)
            }
            _ => {
                std::thread::yield_now();
                return Ok(None);
            }
        };
        self.1 += 1;
        Ok(Some(CameraFrame::new(&self.0.data, 64, 48, 64, PixelFormat::Nv12, captured, seq)))
    }
}

#[test]
fn a_stream_of_rejected_timestamps_still_gives_the_one_lost_frame() {
    let clock = Arc::new(FakeClock::new(Seconds(10.0)));
    let quiet = Arc::new(AtomicBool::new(false));
    let camera =
        Rejected { data: picture(PixelFormat::Nv12, (64, 48)), clock: Arc::clone(&clock), quiet: Arc::clone(&quiet) };
    let mut tracker = patient(
        Box::new(camera),
        Box::new(Scripted::new(vec![true])),
        WebcamConfig::default().with_mode(64, 48, 30),
        clock,
    );
    let config = TrackerConfig::default().with_lost_after(Duration::from_millis(30));
    let mut source = tracker.start(&FakeCamera::id(), &config).unwrap();
    let tracked = next(&mut *source);
    assert_eq!(tracked.status, TrackStatus::Tracking);
    // From here on every frame the camera delivers is stamped before the first. None is a sign of life, and the camera
    // never goes quiet in the `next_frame` sense, so only the rule run on a rejected frame can say the face is gone.
    quiet.store(true, Ordering::Release);
    let lost = next(&mut *source);
    assert_eq!(lost.status, TrackStatus::Lost);
    assert_eq!(lost.seq, tracked.seq, "a notice repeats the last frame's seq");
    assert_eq!(tracker.error(), None, "the stream is still running: this is the stall rule, not the stream's end");
    // Plenty more rejected frames, many times `lost_after`, and still only the one lost frame.
    let rejected = tracker.failures().backwards;
    while tracker.failures().backwards < rejected + 100 {
        std::thread::yield_now();
    }
    assert!(source.poll(Seconds(0.0)).is_none(), "only one lost frame per silence");
    tracker.stop();
}

/// A stage clock that moves 1 ms on every time it is read, so work between two readings takes time.
struct TickingClock(AtomicU32);
impl Clock for TickingClock {
    fn now(&self) -> Seconds {
        Seconds(100.0 + f64::from(self.0.fetch_add(1, Ordering::Relaxed)) * 1e-3)
    }
}

#[test]
fn a_frame_that_cannot_be_converted_is_in_the_latency_numbers() {
    // Not a JPEG: the frame arrives, cannot be converted, and goes out as a lost frame.
    let frames = (0..5).map(|i| (DeviceTime(i * 33_000_000), vec![0xFF, 0xD8, 1, 2, 3])).collect();
    let camera = FakeCamera::from_frames(CameraFormat::new(64, 48, 30, PixelFormat::Mjpg), frames);
    let mut tracker = patient(
        Box::new(camera),
        Box::new(Scripted::new(vec![true])),
        WebcamConfig::default().with_mode(64, 48, 30),
        Arc::new(TickingClock(AtomicU32::new(0))),
    );
    let _source = start(&mut tracker);
    wait_for_error(&tracker);
    tracker.stop();
    assert_eq!(tracker.failures(), Failures::new(0, 5, 0));
    let latency = tracker.latency();
    // Five lost frames for the five bad ones, and one for the stream's end, which has no capture time to measure from.
    assert_eq!(latency.frames, 6);
    // Each of the five went out at least one clock reading after it arrived. Without them in the histogram the
    // worst case and the median would both read zero.
    assert!(latency.max >= Duration::from_millis(1), "{latency:?}");
    assert!(latency.p50 >= Duration::from_millis(1), "{latency:?}");
}

#[test]
fn a_start_that_fails_leaves_the_last_sessions_numbers_where_they_were() {
    let good = picture(PixelFormat::Nv12, (64, 48));
    let frames = [0_u64, 33, 20, 66].iter().map(|ms| (DeviceTime(ms * 1_000_000), good.clone())).collect();
    let camera = FakeCamera::from_frames(nv12(64, 48), frames);
    let mut tracker = webcam(camera, Scripted::new(vec![true]), WebcamConfig::default().with_mode(64, 48, 30));
    let _source = start(&mut tracker);
    assert_eq!(wait_for_error(&tracker), TrackError::Lost);
    tracker.stop();
    let (latency, failures, error) = (tracker.latency(), tracker.failures(), tracker.error());
    assert_eq!(failures, Failures::new(1, 0, 0));
    assert!(latency.frames > 0);
    // The tracker's thread refused, and then the camera thread did.
    for plan in [vec![true], vec![false, true]] {
        tracker.fail_spawns(plan);
        let config = tracker.tracker_config();
        assert!(tracker.start(&FakeCamera::id(), &config).is_err());
        assert_eq!((tracker.latency(), tracker.failures(), tracker.error()), (latency, failures, error.clone()));
    }
}

#[test]
fn the_default_preference_tries_the_cheapest_format_to_convert_first() {
    // The camera offers only YUY2, so each cheaper format is tried and refused on the way to it.
    let (mut tracker, requests) = spy(WebcamConfig::default());
    let config = tracker.tracker_config();
    tracker.start(&FakeCamera::id(), &config).expect("the fake camera opens");
    tracker.stop();
    let tried: Vec<_> = requests.lock().unwrap().iter().map(|r| r.pixel).collect();
    assert_eq!(
        tried,
        [Some(PixelFormat::Rgb8), Some(PixelFormat::Bgra8), Some(PixelFormat::Nv12), Some(PixelFormat::Yuy2)]
    );
}
