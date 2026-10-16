//! Benchmark for the pose path an animator runs per frame: blend two postures, layer an overlay on a mask, mirror, and
//! resolve the hand shapes into a `HumanoidPose`. Run with `cargo bench -p omuzaki-anim --bench poses`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim::hands::{HandLibrary, HandRest};
use omuzaki_anim::poses::{ClipLibrary, PoseLibrary, PoseMask, Posture};
use omuzaki_anim_api::HumanoidBone;

fn poses(c: &mut Criterion) {
    let (Ok(library), Ok(hands)) = (PoseLibrary::builtin(), HandLibrary::builtin()) else {
        panic!("built-in libraries")
    };
    let posture = |id: &str| library.get(id).and_then(|p| p.posture(hands).ok()).unwrap_or_else(|| panic!("pose {id}"));
    let (a, b) = (posture("wave-right"), posture("wave-left"));
    let rest = HandRest::nominal();
    let mask = PoseMask::subtree(HumanoidBone::RightShoulder);
    c.bench_function("poses: blend, layer, mirror and resolve", |bench| {
        bench.iter(|| {
            let blended = Posture::blend(black_box(&a), black_box(&b), 0.4);
            let layered = Posture::layer(&blended, &a.mirrored(), mask, 0.7);
            black_box(layered.to_humanoid(&rest))
        });
    });
    c.bench_function("poses: blend only", |bench| bench.iter(|| Posture::blend(black_box(&a), black_box(&b), 0.4)));
}

/// `PoseClip::sample_posture`, the per-frame clip path: wrapping, segment lookup, easing and the blend.
fn clips(c: &mut Criterion) {
    let Ok(library) = ClipLibrary::builtin() else { panic!("built-in clips") };
    let clip = |id: &str| library.get(id).unwrap_or_else(|| panic!("clip {id}"));
    let (walk, yawn) = (clip("walk"), clip("yawn"));
    let mut t = 0.0_f32;
    c.bench_function("clips: sample a looped clip (walk)", |bench| {
        bench.iter(|| {
            t += 0.033;
            black_box(walk.sample_posture(black_box(t)))
        });
    });
    c.bench_function("clips: sample a one-shot clip (yawn)", |bench| {
        bench.iter(|| {
            t = (t + 0.033) % 4.2;
            black_box(yawn.sample_posture(black_box(t)))
        });
    });
}

criterion_group!(benches, poses, clips);
criterion_main!(benches);
