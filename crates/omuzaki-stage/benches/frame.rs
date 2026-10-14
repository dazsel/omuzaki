//! Benchmark: the stage's per-frame CPU cost with the renderer excluded (target <= 0.3 ms for a Mira-class
//! character, spec v3 §10). The mock character is far lighter than that, so this is the floor of the loop itself:
//! rig solve with springs, runtime steps, dynamic uploads, scene fill. The renderer is a double that answers a canned
//! report and does not copy uploaded vertices.

// Bench helpers may panic: a panic is a clear failure.
#![allow(clippy::expect_used)]

#[path = "../tests/frame/double.rs"]
mod double;

use criterion::{Criterion, criterion_group, criterion_main};
use double::{Recorder, built};
use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_render_api::{AlphaEncoding, Camera, Quality, Renderer, Scene, TargetDesc};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use std::hint::black_box;

fn frame(c: &mut Criterion) {
    let mut renderer = Recorder::new(false);
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::new(8, 8), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let mut stage = Stage::new(
        &mut renderer,
        built(),
        UploadOptions { face_px_per_m: 1200.0 },
        Scene::new(Camera::default()),
        target,
        Quality::live(),
        StageClock::fixed(60.0),
    )
    .expect("the stage starts");
    let mut input = FrameInput { pose: HumanoidPose::REST, ..FrameInput::default() };
    seed_base_pose(&stage.built().rig, &mut input.pose);
    let mut n = 0_u32;
    c.bench_function("stage_tick_mock_character", |b| {
        b.iter(|| {
            n = n.wrapping_add(1);
            input.wind = Vec3::new(f32::from(u8::try_from(n % 64).unwrap_or(0)) * 0.01, 0.0, 0.0);
            // Keep the log from growing without bound: the double only needs it for counting.
            renderer.updates.clear();
            black_box(stage.tick(&mut renderer, black_box(&input)).expect("frame"));
        });
    });
    stage.release(&mut renderer).expect("released");
}

criterion_group!(benches, frame);
criterion_main!(benches);
