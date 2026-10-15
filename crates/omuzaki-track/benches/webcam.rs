//! The camera thread's glue cost per 1280x720 NV12 frame: pixel conversion, a scripted tracker, `to_track_frame` and
//! the mailbox publish, and again with the preview tap's box filter on top. The networks come on top
//! (`benches/pipeline.rs`). The budget is 3 ms at p95 (issue #378) for the glue; the criterion mean is printed with the
//! measured p95 and worst case below it. A last case times an MJPG decode, whose library allocates per frame.

// A bench that cannot run its frames should stop loudly; the casts only build synthetic data and times.
#![allow(clippy::expect_used, clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::{ARKIT_COUNT, ChannelMask};
use omuzaki_core::time::Seconds;
use omuzaki_track::webcam::{FrameConverter, FrameTracker, PreviewFrame};
use omuzaki_track::{FaceSample, FaceTrackError, NormRect, RgbFrame, StageTimings, to_track_frame};
use omuzaki_track_api::camera::{CameraFrame, DeviceTime, PixelFormat};
use omuzaki_track_api::mailbox::mailbox;

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

    // MJPG: the 64x48 fixture of `omuzaki-pixfmt`, the only JPEG in the repository. zune-jpeg builds a decoder per
    // image, so this is the per-frame cost of that, not of a 720p frame.
    let jpeg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../omuzaki-pixfmt/tests/fixtures/64x48_420.jpg"))
        .expect("the pixfmt fixture is in the repository");
    let mut mjpg = FrameConverter::with_size(64, 48);
    let frame = CameraFrame::new(&jpeg, 64, 48, 0, PixelFormat::Mjpg, DeviceTime(0), 0);
    c.bench_function("convert_mjpg_64x48", |b| {
        b.iter(|| mjpg.convert(&frame).expect("the fixture decodes").data.len());
    });
    let (p95, max) = percentiles(1000, || {
        mjpg.convert(&frame).expect("the fixture decodes");
    });
    println!("convert_mjpg_64x48: p95 {p95:?}, max {max:?} over 1000 frames");
}

criterion_group!(benches, bench_glue);
criterion_main!(benches);
