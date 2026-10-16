//! Benchmark: the hair `cap` shell (`fill_shell`, 4 753 vertices), the `part_sweep` locks (`rebuild`, 52 locks of
//! 14 points) and the `curtain` (44 guides compiled and meshed as 5 columns of strips, 5 720 vertices) on an ellipsoid
//! head, the first two into reused buffers as an editor does on a slider drag. Run with
//! `cargo bench -p omuzaki-char --bench hair_cap_sweep`.
//!
//! Building runs on every parameter edit, not every frame, so the budget is interactivity: a few milliseconds a group.

#![allow(clippy::expect_used)] // a bench that cannot set up should stop the run

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_char::hair::compile::{Forked, GroupGeometry, HairGenCtx};
use omuzaki_char::hair::r#gen::cap::fill_shell;
use omuzaki_char::hair::r#gen::curtain::{self, Folds};
use omuzaki_char::hair::r#gen::part_sweep::{Lock, rebuild};
use omuzaki_char::hair::tube::OutwardFrame;
use omuzaki_char_api::{CapGen, CurtainGen, Dynamics, GroupId, HeadSurface, PartSweepGen, Quality};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::Palette;

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

/// Vertices of the curtain compiled with `ctx` and meshed (0 if the group is not a strip, which it always is).
fn curtain_vertices(g: &CurtainGen, ctx: &mut HairGenCtx<'_>, frame: &OutwardFrame) -> usize {
    let group = curtain::compile(g, ctx);
    let GroupGeometry::Strip { u, columns } = &group.geometry else { return 0 };
    curtain::group_mesh(&group.clumps, u, *columns, frame, Folds::Prototype).map_or(0, |m| m.positions.len())
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
    // The hair build at Live quality is the guides and the strips together; the budget is 30 ms (#177).
    let (curtain_gen, palette) = (CurtainGen::default(), Palette::default());
    let (id, dynamics) = (GroupId::new("curtain").expect("a valid group id"), Dynamics::default());
    let frame = OutwardFrame { head: Egg.centre(), body_axis: Vec2::new(0.0, -0.03) };
    c.bench_function("hair curtain compile and strips (44 guides x 5 columns)", |b| {
        b.iter(|| {
            let mut forked = Rng::new(7);
            let mut stream = Forked(&mut forked);
            let mut ctx = HairGenCtx::new(&Egg, &palette, Quality::Live, &id, &dynamics, &mut stream);
            black_box(curtain_vertices(&curtain_gen, &mut ctx, &frame))
        });
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
