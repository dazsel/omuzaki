//! Benchmark: one perform-loop step with the real retarget and life layer (PF-24). Budget: at most 100 µs.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim_api::{ChannelBuf, DeviceProfile, HumanoidPose, LifeConfig, Tick};
use omuzaki_anim_mock::animator::MockAnimator;
use omuzaki_anim_mock::track::{SyntheticSpec, TrackReplay, synthetic_track};
use omuzaki_stage::perform::{Performer, PerformerParts, STEP_HZ};

fn perform_step(c: &mut Criterion) {
    let track = synthetic_track(&SyntheticSpec { seconds: 30.0, ..SyntheticSpec::default() });
    let parts = PerformerParts {
        source: Box::new(TrackReplay::new(track).looping()),
        retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
        life: Box::new(Life::new(LifeConfig::new(7))),
        animator: Box::new(MockAnimator),
    };
    let mut performer = Performer::new(parts, ChannelBuf::NEUTRAL, HumanoidPose::REST);
    let mut index = 0_u64;
    c.bench_function("perform_step", |b| {
        b.iter(|| {
            index += 1;
            black_box(performer.step(Tick::at_rate(index, STEP_HZ)).bus.values.checksum())
        });
    });
}

criterion_group!(benches, perform_step);
criterion_main!(benches);
