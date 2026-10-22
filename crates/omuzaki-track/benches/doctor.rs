//! The tracking doctor's cost per frame (spec PF-28: at most 0.2 ms, off the main thread): `observe_frame` on
//! a 1280x720 and a 1920x1080 frame with a face, so the face, background and eye readings all run.

// A bench builds synthetic data; its casts are tiny.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_track::RgbFrame;
use omuzaki_track::doctor::{Doctor, ObserveFrame};
use omuzaki_track::landmarks::NUM_LANDMARKS;

/// Landmarks spread over the middle of the frame, the way a face fills it.
fn face() -> [[f32; 3]; NUM_LANDMARKS] {
    std::array::from_fn(|i| {
        let a = i as f32 * 2.399_963; // the golden angle: an even spread
        let r = (i as f32 / NUM_LANDMARKS as f32).sqrt() * 0.2;
        [0.5 + r * a.cos(), 0.5 + r * a.sin() * 1.3, 0.0]
    })
}

fn bench(c: &mut Criterion) {
    let points = face();
    for (w, h) in [(1280u32, 720u32), (1920, 1080)] {
        let pixels: Vec<u8> = (0..w * h * 3).map(|i| (i % 251) as u8).collect();
        let frame = RgbFrame { data: &pixels, width: w, height: h, stride: (w * 3) as usize };
        let mut doctor = Doctor::default();
        c.bench_function(&format!("doctor_observe_frame_{w}x{h}"), |b| {
            b.iter(|| doctor.observe_frame(&frame, Some(&points), 1.0 / 30.0));
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
