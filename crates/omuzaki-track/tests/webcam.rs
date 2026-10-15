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
use omuzaki_core::time::{Clock, FakeClock, Seconds, SystemClock};
use omuzaki_track::webcam::{Failures, FrameConverter, FrameTracker, PreviewFrame, WebcamConfig, WebcamTracker};
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
    calls: usize,
}

impl Scripted {
    fn new(faces: Vec<bool>) -> Self {
        Self { faces, gate: None, seen: None, resets: Arc::default(), measure: None, signal_at: None, calls: 0 }
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
        if let Some(gate) = &self.gate {
            // A token per frame; a test that fails early must not hang the join in `Drop`.
            gate.recv_timeout(BACKSTOP / 2).ok();
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

fn webcam(camera: FakeCamera, tracker: Scripted, config: WebcamConfig) -> WebcamTracker {
    webcam_on(camera, tracker, config, Arc::new(SystemClock::new()))
}

fn webcam_on(camera: FakeCamera, tracker: Scripted, config: WebcamConfig, clock: Arc<dyn Clock>) -> WebcamTracker {
    WebcamTracker::new(Box::new(camera), Box::new(tracker), config, clock)
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
    // 200 ms budget at compile time; a stopwatch here would only measure the machine's load.
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

#[test]
fn a_camera_that_goes_quiet_gives_one_lost_frame_after_lost_after() {
    /// One frame, then silence that costs no wall time: once `quiet` is set, each empty wait moves the stage clock on
    /// by the timeout. Before that the camera just has nothing yet, and time stands still.
    struct Stall {
        data: Vec<u8>,
        clock: Arc<FakeClock>,
        idle: Arc<AtomicUsize>,
        quiet: Arc<AtomicBool>,
    }
    struct StallStream(Stall, bool);
    impl Camera for Stall {
        fn enumerate(&self) -> Vec<CameraInfo> {
            vec![CameraInfo::new(FakeCamera::id(), "stall", vec![nv12(64, 48)])]
        }

        fn open(&mut self, _: &CameraId, _: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
            let (clock, idle, quiet) = (Arc::clone(&self.clock), Arc::clone(&self.idle), Arc::clone(&self.quiet));
            Ok(Box::new(StallStream(Stall { data: self.data.clone(), clock, idle, quiet }, false)))
        }
    }
    impl CameraStream for StallStream {
        fn format(&self) -> CameraFormat {
            nv12(64, 48)
        }

        fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
            if std::mem::replace(&mut self.1, true) {
                if self.0.quiet.load(Ordering::Acquire) {
                    self.0.clock.advance(Seconds(timeout.as_secs_f64()));
                    self.0.idle.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::yield_now();
                return Ok(None);
            }
            Ok(Some(CameraFrame::new(&self.0.data, 64, 48, 64, PixelFormat::Nv12, DeviceTime(0), 0)))
        }
    }
    let (clock, idle) = (Arc::new(FakeClock::new(Seconds(10.0))), Arc::new(AtomicUsize::new(0)));
    let quiet = Arc::new(AtomicBool::new(false));
    let camera = Stall {
        data: picture(PixelFormat::Nv12, (64, 48)),
        clock: Arc::clone(&clock),
        idle: Arc::clone(&idle),
        quiet: Arc::clone(&quiet),
    };
    // Gated, and quiet only once the consumer has the first frame, which a lost frame would otherwise overwrite.
    let (scripted, tokens) = Scripted::gated(vec![true]);
    let mut tracker =
        WebcamTracker::new(Box::new(camera), Box::new(scripted), WebcamConfig::default().with_mode(64, 48, 30), clock);
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

#[test]
fn the_preview_costs_nothing_while_off_and_scales_the_landmarks_while_on() {
    let (scripted, tokens) = Scripted::gated(vec![true, true, true, false]);
    let camera = FakeCamera::synthetic(nv12(1280, 720), 100);
    let mut tracker = webcam(camera, scripted, WebcamConfig::default());
    let mut source = start(&mut tracker);
    let mut out = PreviewFrame::default();

    // Off: two frames through, and nothing was ever copied.
    for _ in 0..2 {
        tokens.send(()).unwrap();
        next(&mut *source);
    }
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
    tokens.send(()).unwrap();
    next(&mut *source);
    while !tracker.preview(&mut out) {
        assert!(Instant::now() < deadline, "no preview");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(out.landmarks.is_none());
    // The thread is waiting for the next token; with the sender gone the scripted tracker stops waiting.
    drop(tokens);
}

#[test]
fn the_camera_thread_allocates_nothing_over_three_hundred_frames() {
    // Frames 20 to 319 are measured, with the preview on, after the buffers have grown. The face comes and goes in
    // runs of two frames and three, so the landmark buffers are lost and reacquired over and over.
    let faces: Vec<bool> = (0..330).map(|i| i % 5 < 2).collect();
    let (done, finished) = channel();
    let scripted = Scripted { measure: Some(20..320), signal_at: Some((320, done)), ..Scripted::new(faces) };
    // The camera never runs dry: the session ends on frame 320, which the tracker reports, so no timer decides when.
    let camera = FakeCamera::synthetic(nv12(160, 120), u64::MAX);
    let mut tracker = webcam(camera, scripted, WebcamConfig::default().with_mode(160, 120, 30));
    tracker.set_preview(true);
    let _source = start(&mut tracker);
    finished.recv_timeout(BACKSTOP).expect("the camera thread reached frame 320");
    tracker.stop();
    assert_eq!(CAMERA_ALLOCATIONS.load(Ordering::Relaxed), 0);
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
        // The thread is waiting in the scripted tracker for the next frame's token: release it so `stop` is quick.
        tokens.send(()).unwrap();
        tracker.stop();
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
    let tracker =
        WebcamTracker::new(Box::new(camera), Box::new(Scripted::new(vec![true])), config, Arc::new(SystemClock::new()));
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

    // One quality tier only: others are refused before the camera is touched, and the tracker stays usable.
    let (mut tracker, requests) = spy(WebcamConfig::default());
    for tier in [QualityTier::Balanced, QualityTier::Light] {
        let refused = tracker.start(&FakeCamera::id(), &TrackerConfig::default().with_tier(tier));
        assert!(matches!(refused, Err(TrackError::Backend(_))), "{tier:?}");
    }
    assert!(requests.lock().unwrap().is_empty());
    assert!(tracker.start(&FakeCamera::id(), &TrackerConfig::default()).is_ok());
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
