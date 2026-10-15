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
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_anim_api::{ARKIT_COUNT, ChannelMask, FrameSource, TrackFrame, TrackStatus};
use omuzaki_core::time::Seconds;
use omuzaki_track::webcam::{FrameTracker, PreviewFrame, WebcamConfig, WebcamTracker};
use omuzaki_track::{FaceSample, FaceTrackError, NormRect, RgbFrame, StageTimings};
use omuzaki_track_api::camera::{
    Camera, CameraFormat, CameraFormatRequest, CameraFrame, CameraId, CameraInfo, CameraStream, DeviceTime, PixelFormat,
};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::tracker::{Tracker, TrackerConfig};
use omuzaki_track_mock::FakeCamera;

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

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
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const LANDMARK: [f32; 3] = [0.5, 0.25, 0.0];

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
    calls: usize,
}

impl Scripted {
    fn new(faces: Vec<bool>) -> Self {
        Self { faces, gate: None, seen: None, resets: Arc::default(), measure: None, calls: 0 }
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
            gate.recv_timeout(Duration::from_secs(5)).ok();
        }
        if let Some(seen) = &self.seen {
            let row = frame.width as usize * 3;
            let pixels =
                (0..frame.height as usize).flat_map(|y| frame.data[y * frame.stride..][..row].to_vec()).collect();
            seen.lock().unwrap().push(pixels);
        }
        if let Some(range) = &self.measure {
            MEASURING.with(|m| m.set(range.contains(&self.calls)));
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
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(frame) = source.poll(Seconds(0.0)) {
            return frame;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("no frame within 10 s");
}

fn wait_for_error(tracker: &WebcamTracker) -> TrackError {
    let deadline = Instant::now() + Duration::from_secs(10);
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
    WebcamTracker::new(Box::new(camera), Box::new(tracker), config)
}

fn start(tracker: &mut WebcamTracker) -> Box<dyn FrameSource> {
    tracker.start(&FakeCamera::id(), &TrackerConfig::default()).expect("the fake camera opens")
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
    // The extra frame goes out, then the fake camera runs dry: a lost frame, and the reason is kept.
    tokens.send(()).unwrap();
    assert_eq!(next(&mut *source).status, TrackStatus::Lost);
    assert_eq!(tracker.error(), Some(TrackError::Lost));
    assert_eq!(tracker.latency().frames, faces.len() as u64 + 2);
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
    while next(&mut *source).status == TrackStatus::Tracking {}
    assert!(matches!(tracker.error(), Some(TrackError::Backend(_))), "{:?}", tracker.error());
    let stopped = Instant::now();
    tracker.stop();
    assert!(stopped.elapsed() < Duration::from_millis(200));
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
    let stopped = Instant::now();
    tracker.stop();
    assert!(stopped.elapsed() < Duration::from_millis(200), "stop took {:?}", stopped.elapsed());
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
    struct Stall(Vec<u8>);
    struct StallStream(Vec<u8>, bool);
    impl Camera for Stall {
        fn enumerate(&self) -> Vec<CameraInfo> {
            vec![CameraInfo::new(FakeCamera::id(), "stall", vec![nv12(64, 48)])]
        }

        fn open(&mut self, _: &CameraId, _: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
            Ok(Box::new(StallStream(self.0.clone(), false)))
        }
    }
    impl CameraStream for StallStream {
        fn format(&self) -> CameraFormat {
            nv12(64, 48)
        }

        fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
            if std::mem::replace(&mut self.1, true) {
                std::thread::sleep(timeout);
                return Ok(None);
            }
            Ok(Some(CameraFrame::new(&self.0, 64, 48, 64, PixelFormat::Nv12, DeviceTime(0), 0)))
        }
    }
    let mut tracker = WebcamTracker::new(
        Box::new(Stall(picture(PixelFormat::Nv12, (64, 48)))),
        Box::new(Scripted::new(vec![true])),
        WebcamConfig::default().with_mode(64, 48, 30),
    );
    let config = TrackerConfig::default().with_lost_after(Duration::from_millis(30));
    let mut source = tracker.start(&FakeCamera::id(), &config).unwrap();
    assert_eq!(next(&mut *source).status, TrackStatus::Tracking);
    assert_eq!(next(&mut *source).status, TrackStatus::Lost);
    std::thread::sleep(Duration::from_millis(150));
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
    let deadline = Instant::now() + Duration::from_secs(10);
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
}

#[test]
fn the_camera_thread_allocates_nothing_over_three_hundred_frames() {
    // Frames 20 to 319 are measured, with the preview on, after the buffers have grown.
    let scripted = Scripted { measure: Some(20..320), ..Scripted::new(vec![true]) };
    let camera = FakeCamera::synthetic(nv12(320, 240), 330);
    let mut tracker = webcam(camera, scripted, WebcamConfig::default().with_mode(320, 240, 30));
    tracker.set_preview(true);
    let _source = start(&mut tracker);
    assert_eq!(wait_for_error(&tracker), TrackError::Lost);
    tracker.stop();
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
