//! Benchmarks of the perform loop with the real retarget and life layer (PF-24). Budget: at most 100 µs per step.
//!
//! - `perform_step`: one `Performer::step`.
//! - `control_advance_quiet`: one `PerformControl::advance` with nothing queued (the per-frame cost the control adds).
//! - `control_advance_4_configs` and `control_advance_4_calibrations`: one `advance` that takes a full share
//!   (`MAX_COMMANDS_PER_ADVANCE`) of settings requests or calibrations. Only the `advance` call is timed; the handle
//!   queues the requests outside it.

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim_api::{ChannelBuf, DeviceProfile, HumanoidPose, LifeConfig, Tick};
use omuzaki_anim_mock::animator::MockAnimator;
use omuzaki_anim_mock::track::{SyntheticSpec, TrackReplay, synthetic_track};
use omuzaki_core::time::Seconds;
use omuzaki_stage::perform::host::{
    MAX_COMMANDS_PER_ADVANCE, PerformControl, PerformHandle, PerformHost, RetargetChange, perform_channel,
};
use omuzaki_stage::perform::{Performer, PerformerParts, STEP_HZ};

fn performer() -> Performer {
    let track = synthetic_track(&SyntheticSpec { seconds: 30.0, ..SyntheticSpec::default() });
    let parts = PerformerParts {
        source: Box::new(TrackReplay::new(track).looping()),
        retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
        life: Box::new(Life::new(LifeConfig::new(7))),
        animator: Box::new(MockAnimator),
    };
    Performer::new(parts, ChannelBuf::NEUTRAL, HumanoidPose::REST)
}

/// The middle of step `i`, so each `advance` runs exactly one step.
#[allow(clippy::cast_precision_loss)] // bench step counts are small
fn mid_step(i: u64) -> Seconds {
    Seconds((i as f64 + 0.5) / f64::from(STEP_HZ))
}

/// Times `advance` alone: `queue` runs before each call, outside the clock.
fn timed_advance(
    b: &mut criterion::Bencher<'_>,
    handle: &PerformHandle,
    control: &mut PerformControl,
    performer: &mut Performer,
    index: &mut u64,
    queue: &dyn Fn(&PerformHandle, u64),
) {
    // `index` outlives the routine's calls, so every call still finds one step due.
    b.iter_custom(|iters| {
        let mut total = Duration::ZERO;
        for _ in 0..iters {
            *index += 1;
            queue(handle, *index);
            let start = Instant::now();
            black_box(control.advance(performer, mid_step(*index)).map(|out| out.tick.index));
            total += start.elapsed();
        }
        total
    });
}

fn control_advance(c: &mut Criterion) {
    let mut performer = performer();
    let (handle, mut control) = perform_channel(&performer);
    control.advance(&mut performer, mid_step(0));
    let calibration = handle.calibration();
    let mut index = 0_u64;
    c.bench_function("control_advance_quiet", |b| {
        timed_advance(b, &handle, &mut control, &mut performer, &mut index, &|_, _| {});
    });
    c.bench_function("control_advance_4_configs", |b| {
        timed_advance(b, &handle, &mut control, &mut performer, &mut index, &|handle, i| {
            for k in 0..MAX_COMMANDS_PER_ADVANCE {
                #[allow(clippy::cast_precision_loss)] // small
                let smoothing = Some(((i + k as u64) % 10) as f32 / 10.0);
                let change = RetargetChange { smoothing, ..RetargetChange::default() };
                black_box(handle.request_retarget(change).ok());
            }
        });
    });
    c.bench_function("control_advance_4_calibrations", |b| {
        timed_advance(b, &handle, &mut control, &mut performer, &mut index, &|handle, _| {
            for _ in 0..MAX_COMMANDS_PER_ADVANCE {
                black_box(handle.set_calibration(calibration).ok());
            }
        });
    });
}

fn perform_step(c: &mut Criterion) {
    let mut performer = performer();
    let mut index = 0_u64;
    c.bench_function("perform_step", |b| {
        b.iter(|| {
            index += 1;
            black_box(performer.step(Tick::at_rate(index, STEP_HZ)).bus.values.checksum())
        });
    });
}

criterion_group!(benches, perform_step, control_advance);
criterion_main!(benches);
