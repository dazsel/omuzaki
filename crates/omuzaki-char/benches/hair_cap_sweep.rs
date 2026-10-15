//! Benchmark: the hair `cap` shell (`fill_shell`, 4 753 vertices) and the `part_sweep` locks (`rebuild`, 52 locks of
//! 14 points) on an ellipsoid head, each into reused buffers as an editor does on a slider drag. Run with
//! `cargo bench -p omuzaki-char --bench hair_cap_sweep`.
//!
//! Building runs on every parameter edit, not every frame, so the budget is interactivity: a few milliseconds a group.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_char::hair::r#gen::cap::fill_shell;
use omuzaki_char::hair::r#gen::part_sweep::{Lock, rebuild};
use omuzaki_char_api::{CapGen, HeadSurface, PartSweepGen};
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;

/// A synthetic ellipsoid head.
#[derive(Debug)]
struct Egg;

impl HeadSurface for Egg {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 0.2, 0.0)
    }
    fn radius(&self, dir: Vec3) -> f32 {
        1.0 / (dir / Vec3::new(0.08, 0.11, 0.095)).length()
    }
}

fn bench(c: &mut Criterion) {
    let (cap, sweep) = (CapGen::default(), PartSweepGen::default());
    let mut shell = MeshData::default();
    fill_shell(&cap, &Egg, &mut shell);
    c.bench_function("hair cap fill_shell (97 x 49)", |b| b.iter(|| fill_shell(black_box(&cap), &Egg, &mut shell)));
    let mut locks: Vec<Lock> = Vec::new();
    rebuild(&sweep, &Egg, &mut locks);
    c.bench_function("hair part_sweep rebuild (2 x 26 locks)", |b| {
        b.iter(|| rebuild(black_box(&sweep), &Egg, &mut locks));
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
