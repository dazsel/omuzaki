//! Benchmarks for the spring solver at the size of a heavily-haired character: 30 chains of 11 joints, which is 300
//! swinging joints (the last joint of a chain is only a tail), 10 of the chains simulated in the head's space, all
//! colliding with a sphere and a capsule. Run with `cargo bench -p omuzaki-rig --bench spring`.
//!
//! Spec §10 budgets the whole CPU side at under 3% of 8 cores at 1080p30, so well over 1 ms a frame is available.

#![allow(clippy::expect_used)] // a rig that fails to build should stop the run with its reason

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::{SpringSolver, Tick};
use omuzaki_rig::spring::VrmSpringSolver;

#[path = "../tests/support/spring_rig.rs"]
mod spring_rig;

fn solver(c: &mut Criterion) {
    let rig = spring_rig::hair_rig(30, 11);
    let mut solver = VrmSpringSolver::new(&rig.set, &rig.skeleton).expect("the rig's springs are valid");
    let mut pose = rig.skeleton.rest_pose();

    // At 60 fps a frame is two 120 Hz sub-steps; at 30 fps, four.
    for (name, fps) in [("60 fps (2 sub-steps)", 60.0), ("30 fps (4 sub-steps)", 30.0)] {
        let mut frame = 0_u64;
        c.bench_function(&format!("step, 300 joints, {name}"), |b| {
            b.iter(|| {
                frame += 1;
                rig.animate(&mut pose, frame);
                solver.step(Tick::at_rate(frame, fps), &rig.skeleton, black_box(&mut pose));
            });
        });
    }

    // The prototype settled hair with 45 frames before the first one is shown: 0.75 s, 90 sub-steps.
    c.bench_function("warm_up 0.75 s, 300 joints", |b| {
        b.iter(|| solver.warm_up(&rig.skeleton, black_box(&mut pose), 0.75));
    });

    // The same hair with an inside sphere, an inside capsule and a plane added to every chain's colliders.
    let extended = spring_rig::extended_hair_rig(30, 11);
    let mut solver = VrmSpringSolver::new(&extended.set, &extended.skeleton).expect("the rig's springs are valid");
    let mut frame = 0_u64;
    c.bench_function("step, 300 joints, 60 fps (2 sub-steps), inside and plane colliders", |b| {
        b.iter(|| {
            frame += 1;
            extended.animate(&mut pose, frame);
            solver.step(Tick::at_rate(frame, 60.0), &extended.skeleton, black_box(&mut pose));
        });
    });

    c.bench_function("construct, 300 joints", |b| {
        b.iter(|| VrmSpringSolver::new(black_box(&rig.set), &rig.skeleton));
    });
}

criterion_group!(benches, solver);
criterion_main!(benches);
