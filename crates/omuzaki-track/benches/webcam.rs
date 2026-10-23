//! The camera thread's glue cost per 1280x720 NV12 frame: pixel conversion, a scripted tracker, `to_track_frame` and
//! the mailbox publish, and again with the preview tap's box filter on top. The networks come on top
//! (`benches/pipeline.rs`). The budget is 3 ms at p95 (issue #378) for the glue; the criterion mean is printed with the
//! measured p95 and worst case below it. The glue cases time the work inline on one thread; `worker_path` runs the real
//! `WebcamTracker`, whose camera thread hands each frame to the tracker's thread and waits for the answer, and reports
//! the tracker's own capture-to-publish latency, so the hand-off is in the number. A last case times a 720p MJPG
//! decode.

// A bench that cannot run its frames should stop loudly; the casts only build synthetic data and times.
#![allow(clippy::expect_used, clippy::cast_possible_truncation, clippy::cast_precision_loss)]

#[path = "../../omuzaki-pixfmt/tests/support/jpeg_encoder.rs"]
mod jpeg_encoder;

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use jpeg_encoder::{Options, encode, picture};
use omuzaki_anim_api::{ARKIT_COUNT, ChannelMask};
use omuzaki_core::time::{Seconds, SystemClock};
use omuzaki_track::webcam::{FrameConverter, FrameTracker, PreviewFrame, WebcamConfig, WebcamTracker};
use omuzaki_track::{FaceSample, FaceTrackError, NormRect, RgbFrame, StageTimings, to_track_frame};
use omuzaki_track_api::camera::CameraFormat;
use omuzaki_track_api::camera::{CameraFrame, DeviceTime, PixelFormat};
use omuzaki_track_api::mailbox::mailbox;
use omuzaki_track_api::tracker::Tracker;
use omuzaki_track_mock::FakeCamera;

const W: u32 = 1280;
const H: u32 = 720;

/// Always finds the same face.
struct Scripted(FaceSample);

impl FrameTracker for Scripted {
    fn track(&mut self, _: &RgbFrame<'_>, _: Seconds) -> Result<(Option<FaceSample>, StageTimings), FaceTrackError> {
        Ok((Some(self.0.clone()), StageTimings::default()))
    }

    fn reset(&mut self) {}
}

fn sample() -> FaceSample {
    let mut pose = [0.0; 16];
    for i in 0..4 {
        pose[i * 5] = 1.0;
    }
    FaceSample {
        blendshapes: [0.0; ARKIT_COUNT],
        present: ChannelMask::EMPTY,
        pose,
        landmarks: [[0.5; 3]; 478],
        presence: 1.0,
        roi: NormRect { x_center: 0.5, y_center: 0.5, width: 0.5, height: 0.5, rotation: 0.0 },
    }
}

/// p95 and worst case of `frames` calls of `f`, timed one by one.
fn percentiles(frames: usize, mut f: impl FnMut()) -> (Duration, Duration) {
    let mut times: Vec<Duration> = (0..frames)
        .map(|_| {
            let started = Instant::now();
            f();
            started.elapsed()
        })
        .collect();
    times.sort_unstable();
    (times[times.len() * 95 / 100], times[times.len() - 1])
}

fn bench_glue(c: &mut Criterion) {
    let (y_len, uv_len) = (W as usize * H as usize, W as usize * H as usize / 2);
    let data: Vec<u8> = (0..y_len + uv_len).map(|i| (i % 251) as u8).collect();
    let (mut converter, mut tracker) = (FrameConverter::with_size(W, H), Scripted(sample()));
    let (mut writer, _reader) = mailbox();
    let mut preview = PreviewFrame::default();
    let mut seq = 0_u64;
    // The camera thread's per-frame work; with `previewing` it also makes the preview, as it does while the tap is on.
    let mut glue = |previewing: bool| {
        seq += 1;
        let frame = CameraFrame::new(&data, W, H, W as usize, PixelFormat::Nv12, DeviceTime(seq), seq);
        let rgb = converter.convert(&frame).expect("a valid NV12 frame converts");
        let time = Seconds(seq as f64 / 30.0);
        let (face, _) = tracker.track(&rgb, time).expect("the scripted tracker answers");
        writer.publish(&to_track_frame(face.as_ref(), seq, time));
        if previewing {
            assert!(preview.fill_from(&rgb), "a converted frame is valid");
        }
    };
    c.bench_function("glue_nv12_720p", |b| b.iter(|| glue(false)));
    c.bench_function("glue_nv12_720p_preview_on", |b| b.iter(|| glue(true)));
    for (name, previewing) in [("glue_nv12_720p", false), ("glue_nv12_720p_preview_on", true)] {
        let (p95, max) = percentiles(300, || glue(previewing));
        println!("{name}: p95 {p95:?}, max {max:?} over 300 frames");
    }

    // MJPG: a 1280x720 4:2:2 frame without Huffman tables, as webcams send it, from the pixfmt test encoder.
    let (w, h) = (1280, 720);
    let jpeg = encode(&picture(w, h, 1), w, h, Options::default());
    let mut mjpg = FrameConverter::with_size(1280, 720);
    let frame = CameraFrame::new(&jpeg, 1280, 720, 0, PixelFormat::Mjpg, DeviceTime(0), 0);
    c.bench_function("convert_mjpg_720p", |b| {
        b.iter(|| mjpg.convert(&frame).expect("the frame decodes").data.len());
    });
    let (p95, max) = percentiles(300, || {
        mjpg.convert(&frame).expect("the frame decodes");
    });
    println!("convert_mjpg_720p: p95 {p95:?}, max {max:?} over 300 frames");
}

/// Frames the worker-path case runs: enough for a stable p95 at the histogram's 0.1 ms resolution.
const WORKER_FRAMES: u64 = 2000;

/// The real `WebcamTracker` over a fake camera that delivers 1280x720 NV12 frames as fast as they are taken, and the
/// scripted tracker: what its own `latency()` reports is the time from a frame's capture to its publication, with the
/// camera thread's conversion and publish and the hand-off to the tracker's thread and back all in it. The fake camera
/// stamps its frames 1/60 s apart but delivers them far faster, so the clock map reads each as arriving with no delay,
/// and the latency is that of the work.
fn bench_worker_path(_: &mut Criterion) {
    for previewing in [false, true] {
        let camera = FakeCamera::synthetic(CameraFormat::new(W, H, 60, PixelFormat::Nv12), WORKER_FRAMES);
        let config = WebcamConfig::default().with_mode(W, H, 60);
        let mut tracker =
            WebcamTracker::new(Box::new(camera), Box::new(Scripted(sample())), config, Arc::new(SystemClock::new()));
        tracker.set_preview(previewing);
        let started = tracker.tracker_config();
        let _source = tracker.start(&FakeCamera::id(), &started).expect("the fake camera opens");
        // The fake camera runs dry after its frames, which ends the session with an error.
        while tracker.error().is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        tracker.stop();
        let latency = tracker.latency();
        println!(
            "worker_path_nv12_720p{}: p50 {:?}, p95 {:?}, max {:?} over {} frames (capture to publish, hand-off included)",
            if previewing { "_preview_on" } else { "" },
            latency.p50,
            latency.p95,
            latency.max,
            latency.frames,
        );
    }
}

criterion_group!(benches, bench_glue, bench_worker_path);
criterion_main!(benches);
