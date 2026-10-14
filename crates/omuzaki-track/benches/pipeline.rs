//! The tracker's own cost per frame, with scripted sessions standing in for the networks: the crop warps, the anchor
//! decode and merge, the landmark projection, smoothing and the head-pose fit. The networks (about 8 ms on the
//! reference CPU) come on top; `track.face` is budgeted at 5 ms p95 in all (spec v3 §10), measured with the real
//! models.
//!
//! `image` runs the detector on every frame, as a frame where the face is new does; `video` is the steady state, where
//! the previous frame's landmarks seed the crop.

// A bench that cannot run its scripted frames should stop loudly.
#![allow(clippy::expect_used)]

#[path = "../tests/common/mod.rs"]
mod common;

use common::{
    FACE_ROI, Pose, SIDE, blendshapes_script, canonical_face, detector_script, frame, geometry, graphs, grey_pixels,
    landmarks_script, project,
};
use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::time::Seconds;
use omuzaki_track::{FaceTracker, FaceTrackerConfig, Mode};

fn bench_track(c: &mut Criterion) {
    let face = project(&canonical_face(), &Pose::facing(0.2, -0.1, 0.05), SIDE, SIDE);
    let pixels = grey_pixels();
    let mut group = c.benchmark_group("track");
    for (name, mode) in [("image", Mode::Image), ("video", Mode::Video)] {
        let sessions = graphs(detector_script(true), landmarks_script(&face, FACE_ROI, 10.0), blendshapes_script());
        let mut tracker =
            FaceTracker::new(sessions, &geometry(), FaceTrackerConfig { mode, ..FaceTrackerConfig::default() })
                .expect("scripted sessions fit the signatures");
        let mut frame_index = 0_u32;
        group.bench_function(name, |b| {
            b.iter(|| {
                frame_index += 1;
                tracker.track(&frame(&pixels), Seconds(f64::from(frame_index) / 30.0)).expect("scripted frames track")
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_track);
criterion_main!(benches);
