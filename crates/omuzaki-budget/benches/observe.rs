//! `Scheduler::observe` runs once per frame next to a 1.5 ms GPU budget (spec §10): it must cost nanoseconds.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use omuzaki_budget::Scheduler;
use omuzaki_budget::scheduler::DOWN_SAMPLES;
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{FrameReport, GpuTimings, Pass, PassTimes, Quality, budget};

fn report(frame: u64, ms: f64) -> FrameReport {
    let mut passes = PassTimes::default();
    for pass in Pass::ALL {
        passes.set(pass, Seconds(ms / 8000.0));
    }
    FrameReport {
        frame,
        render_size: UVec2::new(1920, 1080),
        cpu: passes,
        gpu: Some(GpuTimings { frame, passes }),
        draw_calls: 100,
        triangles: 100_000,
    }
}

fn observe_benches(c: &mut Criterion) {
    // The common frame: inside the budget, nothing to decide.
    c.bench_function("observe_steady", |b| {
        let mut scheduler = Scheduler::new(Quality::high(), budget::HIGH_GPU);
        let mut frame = 0;
        b.iter(|| {
            frame += 1;
            black_box(scheduler.observe(black_box(&report(frame, 2.0))))
        });
    });
    // The worst frame: the sample that tips an overloaded scheduler over, so it walks the ladder and changes quality.
    // Each iteration starts from a clone of one primed scheduler, so the transition is what is measured.
    c.bench_function("observe_transition", |b| {
        let mut primed = Scheduler::new(Quality::high(), budget::HIGH_GPU);
        for frame in 1..DOWN_SAMPLES {
            primed.observe(&report(u64::from(frame), 50.0));
        }
        let tipping = report(u64::from(DOWN_SAMPLES), 50.0);
        b.iter_batched_ref(
            || primed.clone(),
            |scheduler| black_box(scheduler.observe(black_box(&tipping))),
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, observe_benches);
criterion_main!(benches);
