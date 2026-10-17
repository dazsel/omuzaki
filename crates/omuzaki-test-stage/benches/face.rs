//! Benchmark: the live stage drawing its face through `PaintTarget::native` on the GPU (#2074), against the face-paint
//! budget (`budget::FACE_PAINT_GPU`, 0.5 ms, spec v3 §10 `gpu.face_paint`).
//!
//! The mock character's stage on the machine's best GPU, the production rig (`FaceRig::standard`'s painter and
//! densities, the face textures at the live default `DEFAULT_PX_PER_M`, so the Base layer is 1,440 px wide), a
//! 1920 x 1080 frame. It runs as the spec's budget rows are read: paced (30 fps by default), warmed up (20 s), then
//! measured over 3,000 frames, in two phases: the face changing every frame (talking, blinking, glancing, blushing),
//! then still. Per frame it records:
//!
//! - **GPU face paint**: the renderer's own timestamps around the face's commands and the mip rebuild, reported under
//!   `PaintTextures` (they arrive a frame or two late, so each is matched to the frame it measures), over the frames that
//!   repainted the face;
//! - **GPU frame**: every other pass (`gpu.frame`);
//! - **CPU tick**: `Stage::tick` on the calling thread, recording and submitting included.
//!
//! `OMUZAKI_FACE_BENCH_FPS` (0 runs unpaced), `OMUZAKI_FACE_BENCH_WARMUP_S` and `OMUZAKI_FACE_BENCH_FRAMES` change the
//! defaults. The output is a markdown table; it names the graphics backend only, never the adapter (the repository is
//! public). Without a GPU, or on a software adapter, it prints why its numbers are not a timing.

// Bench helpers may panic: a panic is a clear failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../tests/support/mod.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use omuzaki_face_api::DEFAULT_PX_PER_M;
use omuzaki_render_api::{GpuTimings, Pass, Renderer, budget};
use omuzaki_stage::frame::Stage;
use support::{Counted, expression, face_stage, gpu_rig, hardware_renderer};

/// The frame's size: the budget scene's output.
const SIZE: u32 = 1080;

/// A setting from the environment, or `default`.
fn setting<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// One phase's samples, in milliseconds.
#[derive(Default)]
struct Samples {
    face_gpu: Vec<f64>,
    frame_gpu: Vec<f64>,
    tick_cpu: Vec<f64>,
}

/// p50 / p95 / p99 / max of `values` (sorted in place), or a dash for none.
fn stats(values: &mut [f64]) -> String {
    if values.is_empty() {
        return "-".to_owned();
    }
    values.sort_by(f64::total_cmp);
    let at = |p: usize| values[(values.len() * p / 100).min(values.len() - 1)];
    format!("{:.3} / {:.3} / {:.3} / {:.3}", at(50), at(95), at(99), values[values.len() - 1])
}

/// The GPU face paint and the rest of the frame, in milliseconds, from a frame's timings.
fn split(timings: &GpuTimings) -> (Option<f64>, f64) {
    let face = timings.passes.get(Pass::PaintTextures).map(|s| s.0 * 1e3);
    let frame = Pass::ALL
        .into_iter()
        .filter(|&p| p != Pass::PaintTextures)
        .filter_map(|p| timings.passes.get(p))
        .map(|s| s.0 * 1e3)
        .sum();
    (face, frame)
}

/// Runs `stage` paced at `fps` (unpaced at 0) for `warm_up`, then `frames` frames, the face changing every frame if
/// `talking`, and returns what it measured.
fn phase(
    stage: &mut Stage,
    renderer: &mut dyn Renderer,
    fps: f64,
    warm_up: Duration,
    frames: usize,
    talking: bool,
) -> Samples {
    let period = if fps > 0.0 { Duration::from_secs_f64(1.0 / fps) } else { Duration::ZERO };
    let mut samples = Samples::default();
    let started = Instant::now();
    let mut next = Instant::now();
    let (mut n, mut measured) = (0_u32, 0_usize);
    // The CPU time of each measured frame, by frame index, until its GPU timings arrive.
    let mut pending: Vec<(u64, f64)> = Vec::with_capacity(8);
    while measured < frames {
        let input = expression(if talking { n } else { 0 });
        let start = Instant::now();
        let out = stage.tick(renderer, &input).expect("a frame");
        let cpu = start.elapsed().as_secs_f64() * 1e3;
        let warm = started.elapsed() >= warm_up;
        if warm {
            pending.push((out.tick.index, cpu));
            measured += 1;
        }
        if let Some(timings) = out.report.gpu.as_ref()
            && let Some(at) = pending.iter().position(|(index, _)| *index == timings.frame)
        {
            let (_, cpu) = pending.swap_remove(at);
            let (face, frame) = split(timings);
            samples.tick_cpu.push(cpu);
            samples.frame_gpu.push(frame);
            samples.face_gpu.extend(face);
        }
        pending.retain(|(index, _)| out.tick.index.saturating_sub(*index) < 8);
        n += 1;
        next += period;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        } else {
            next = Instant::now();
        }
    }
    samples
}

fn main() {
    let mut renderer = match hardware_renderer() {
        Ok(renderer) => renderer,
        Err(e) => {
            println!("face: no GPU for the renderer ({e}); nothing to measure");
            return;
        }
    };
    let backend = renderer.capabilities().backend.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    if !renderer.capabilities().gpu_timings {
        println!("face: {backend} has no timestamp queries inside encoders, so the GPU face paint can't be measured");
        return;
    }
    let fps: f64 = setting("OMUZAKI_FACE_BENCH_FPS", 30.0);
    let warm_up = Duration::from_secs_f64(setting("OMUZAKI_FACE_BENCH_WARMUP_S", 20.0));
    let frames: usize = setting("OMUZAKI_FACE_BENCH_FRAMES", 3000);
    let drawn = Arc::new(AtomicUsize::new(0));
    let mut stage = face_stage(&mut renderer, gpu_rig(Box::new(Counted::new(&drawn))), SIZE, DEFAULT_PX_PER_M);
    let base = stage.character().face_layers().next().map(|l| l.spec.size_px()).unwrap_or_default();

    let mut talking = phase(&mut stage, &mut renderer, fps, warm_up, frames, true);
    let gpu_paints = drawn.load(Ordering::Relaxed);
    let mut still = phase(&mut stage, &mut renderer, fps, Duration::from_secs(2), frames.min(300), false);
    stage.release(&mut renderer).expect("released");

    println!(
        "Graphics backend: {backend}. Face layer {} x {} px; frame {SIZE} x {SIZE}; {} fps; {:.0} s warm-up; {frames} frames \
         (still: {}). The face's paints drawn on the GPU: {gpu_paints}. Times in ms, p50 / p95 / p99 / max.",
        base.0,
        base.1,
        if fps > 0.0 { format!("{fps:.0}") } else { "unpaced".to_owned() },
        warm_up.as_secs_f64(),
        frames.min(300),
    );
    println!();
    println!(
        "| phase | GPU face paint (`PaintTextures`) | repainted frames | GPU frame without it | CPU `Stage::tick` |"
    );
    println!("|---|---:|---:|---:|---:|");
    let repainted = talking.face_gpu.len();
    println!(
        "| face changing every frame | {} | {repainted} | {} | {} |",
        stats(&mut talking.face_gpu),
        stats(&mut talking.frame_gpu),
        stats(&mut talking.tick_cpu)
    );
    println!(
        "| face still | {} | {} | {} | {} |",
        stats(&mut still.face_gpu),
        still.face_gpu.len(),
        stats(&mut still.frame_gpu),
        stats(&mut still.tick_cpu)
    );
    let p95 = talking.face_gpu.get((repainted * 95 / 100).min(repainted.saturating_sub(1))).copied();
    let budget_ms = budget::FACE_PAINT_GPU.0 * 1e3;
    println!();
    match p95 {
        Some(p95) => println!(
            "GPU face paint p95 {p95:.3} ms against the {budget_ms} ms budget: {}",
            if p95 <= budget_ms { "within" } else { "over" }
        ),
        None => println!("no frame reported a GPU face paint"),
    }
}
