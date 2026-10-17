//! What the live stage allocates per steady-state frame with its face painted on the GPU (#2074, spec §8 "no allocation
//! in the hot loop"): a face that changes every frame (the mouth always, the eyes, brows and blush often) on the wgpu
//! renderer, the whole `Stage::tick` counted, rendering included.
//!
//! wgpu allocates inside its own calls (an encoder, a pass, a queue write, a submit), so the total can't be zero; what
//! the workspace controls is its own code. Every allocation is attributed to the first frame of its backtrace that is
//! not std plumbing (`omuzaki_render_mock::counting`, shared with the renderer's hot-loop tests): the stage, the face,
//! the renderer and Vello's scene encoding must make none, and wgpu's share is bounded so a per-frame object created by
//! mistake shows up.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

// The counting allocator is installed only in this test binary, so the other tests run on the plain allocator.
use omuzaki_render_mock::counting;

#[global_allocator]
static ALLOCATOR: counting::Counting = counting::Counting::new(env!("CARGO_PKG_NAME"));

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_stage::frame::face::FaceRig;
use support::{Counted, PROBE_PX_PER_M, cpu_rig, expression, face_stage, gpu_rig, renderer};

/// The frame's size in pixels.
const SIZE: u32 = 128;
/// Frames before counting: pipelines compile, buffers grow to the largest face, GPU timing slots cycle.
const WARM_UP: u32 = 60;
/// Frames counted.
const FRAMES: u32 = 24;
/// The most wgpu may allocate per frame for the face's GPU paints on top of a frame whose face is still. Measured with
/// wgpu 30: about 240 on lavapipe and on a hardware GPU, for the layers this face repaints, each about 30 for the
/// rasteriser's recording (the compute and render passes, three queue writes, the counters' copy and mapping), about 20
/// for the renderer's encoder, timestamps and submit, and the rest for the mip rebuild's passes. A bind group, buffer or
/// texture made per frame by mistake would push it past this.
const FACE_WGPU_PER_FRAME: usize = 300;

/// Each owner's allocations per frame over [`FRAMES`] steady-state frames of a stage with `rig`, whose face changes every
/// frame if `talking`, and how many of the face's paints the GPU drew.
fn per_frame(rig: FaceRig, talking: bool) -> Option<BTreeMap<&'static str, usize>> {
    let mut renderer = renderer()?;
    let mut stage = face_stage(&mut renderer, rig, SIZE, PROBE_PX_PER_M);
    let input = |n: u32| expression(if talking { n } else { 0 });
    for n in 0..WARM_UP {
        stage.tick(&mut renderer, &input(n)).unwrap();
        renderer.wait_idle().unwrap();
    }
    let totals = counting::measure(|| {
        for n in WARM_UP..WARM_UP + FRAMES {
            stage.tick(&mut renderer, &input(n)).unwrap();
        }
    });
    stage.release(&mut renderer).unwrap();
    let frames = usize::try_from(FRAMES).unwrap();
    Some(totals.into_iter().map(|(owner, n)| (owner, n.div_ceil(frames))).collect())
}

/// The owners that must allocate nothing per frame: everything but wgpu.
const OURS: [&str; 6] = [
    counting::OWN,
    counting::WORKSPACE,
    counting::TESTS,
    counting::VELLO,
    counting::DEPENDENCIES,
    counting::UNATTRIBUTED,
];

fn assert_only_wgpu_allocates(per_frame: &BTreeMap<&'static str, usize>, what: &str) {
    for owner in OURS {
        let n = per_frame.get(owner).copied().unwrap_or(0);
        assert_eq!(n, 0, "{what}: {owner} allocated {n} times per frame: {per_frame:?}");
    }
}

#[test]
fn a_talking_face_painted_on_the_gpu_allocates_nothing_outside_wgpu() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let drawn = Arc::new(AtomicUsize::new(0));
    let Some(per_frame) = per_frame(gpu_rig(Box::new(Counted::new(&drawn))), true) else { return };
    println!("GPU face, allocations per frame by owner: {per_frame:?}");
    assert!(drawn.load(Ordering::Relaxed) > 0, "the face was painted on the GPU, or this test aims at the CPU path");
    assert_only_wgpu_allocates(&per_frame, "the GPU face");
}

#[test]
fn a_talking_face_painted_on_the_cpu_allocates_nothing_outside_wgpu() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let Some(per_frame) = per_frame(cpu_rig(), true) else { return };
    println!("CPU face, allocations per frame by owner: {per_frame:?}");
    assert_only_wgpu_allocates(&per_frame, "the CPU fallback");
}

#[test]
fn the_gpu_faces_wgpu_allocations_per_frame_stay_bounded() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let wgpu = |per_frame: &BTreeMap<&'static str, usize>| per_frame.get(counting::WGPU).copied().unwrap_or(0);
    let drawn = Arc::new(AtomicUsize::new(0));
    let (Some(talking), Some(still)) = (
        per_frame(gpu_rig(Box::new(Counted::new(&drawn))), true),
        per_frame(gpu_rig(Box::new(Counted::new(&drawn))), false),
    ) else {
        return;
    };
    let face = wgpu(&talking).saturating_sub(wgpu(&still));
    println!(
        "wgpu per frame: {} with the face changing, {} with it still: {face} for the face's paints",
        wgpu(&talking),
        wgpu(&still)
    );
    assert!(
        face <= FACE_WGPU_PER_FRAME,
        "the face's paints cost wgpu {face} allocations per frame (bound {FACE_WGPU_PER_FRAME})"
    );
}
