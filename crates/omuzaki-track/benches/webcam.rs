//! The camera thread's glue cost per 1280x720 NV12 frame: pixel conversion, a scripted tracker, `to_track_frame` and
//! the mailbox publish. The networks come on top (`benches/pipeline.rs`). The budget is 3 ms at p95 (issue #378); the
//! criterion mean is printed with the measured p95 and worst case below it.

// A bench that cannot run its frames should stop loudly; the casts only build synthetic data and times.
#![allow(clippy::expect_used, clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::{ARKIT_COUNT, ChannelMask};
use omuzaki_core::time::Seconds;
use omuzaki_track::webcam::{FrameConverter, FrameTracker};
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

fn bench_glue(c: &mut Criterion) {
    let (y_len, uv_len) = (W as usize * H as usize, W as usize * H as usize / 2);
    let data: Vec<u8> = (0..y_len + uv_len).map(|i| (i % 251) as u8).collect();
    let (mut converter, mut tracker) = (FrameConverter::new(), Scripted(sample()));
    let (mut writer, _reader) = mailbox();
    let mut seq = 0_u64;
    let mut glue = || {
        seq += 1;
        let frame = CameraFrame::new(&data, W, H, W as usize, PixelFormat::Nv12, DeviceTime(seq), seq);
        let rgb = converter.convert(&frame).expect("a valid NV12 frame converts");
        let time = Seconds(seq as f64 / 30.0);
        let (face, _) = tracker.track(&rgb, time).expect("the scripted tracker answers");
        writer.publish(&to_track_frame(face.as_ref(), seq, time));
    };
    c.bench_function("glue_nv12_720p", |b| b.iter(&mut glue));

    let mut times: Vec<Duration> = (0..300)
        .map(|_| {
            let started = Instant::now();
            glue();
            started.elapsed()
        })
        .collect();
    times.sort_unstable();
    println!(
        "glue_nv12_720p: p95 {:?}, max {:?} over {} frames",
        times[times.len() * 95 / 100],
        times[times.len() - 1],
        times.len()
    );
}

criterion_group!(benches, bench_glue);
criterion_main!(benches);
