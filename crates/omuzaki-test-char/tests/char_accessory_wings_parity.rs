//! The `wings` accessory against the prototype and the real spring solver (CB-43, #196).
//!
//! The prototype's dump is the parity target for the wings' size; the prototype's sample character wears no wings, so today
//! the comparison skips (see `reference_meshes`). The wing's flutter bones run on `omuzaki-rig`'s VRM solver, with the fold
//! driver (`DriverPlan`) run after it as the frame loop does: a 90 degree hips turn makes the wing's tip lag and settle, a
//! folded wing and a spread one are both stable, and nothing allocates per frame once warm.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

mod char_accessory_support;

use char_accessory_support::{SECOND, Sim, allocations_in, built, compare_extents, reference_meshes};
use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_char_api::drivers::DriverPlan;
use serde_json::json;

/// A simulation with the fold driver and the channel value it runs at.
struct Rig {
    sim: Sim,
    plan: DriverPlan,
    channels: ChannelBuf,
}

impl Rig {
    fn new(params: serde_json::Value, fold: f32) -> Self {
        let sim = Sim::new(built("standard", "wings", params));
        let plan = sim.b.rig.driver_plan().unwrap();
        let mut channels = ChannelBuf::NEUTRAL;
        channels.set(Channel::custom(0).unwrap(), fold);
        Self { sim, plan, channels }
    }

    /// One frame: the springs, then the drivers, as the frame loop runs them.
    fn step(&mut self) {
        self.sim.step();
        self.plan.apply(&self.channels, &mut self.sim.pose);
    }
}

#[test]
fn the_prototypes_wings_are_the_size_of_ours() {
    let Some(reference) = reference_meshes("wings", &["wing"]) else { return };
    compare_extents("wings", &built("standard", "wings", json!({})), &reference);
}

/// How far the flutter segment (`flap_0` to `flap_1`) is from where the wrist would carry it rigidly, degrees: the angle between
/// its direction now and the direction it had at rest, turned by the wrist's rotation.
fn flutter(sim: &Sim, rest_in_wrist: omuzaki_core::glam::Vec3) -> f32 {
    let (a, b) = (sim.bone("wings_acc_l_flap_0"), sim.bone("wings_acc_l_flap_1"));
    let wrist = sim.bone("wings_acc_l_wrist");
    let now = (sim.world(b) - sim.world(a)).normalize();
    let rigid = sim.world_rotation(wrist) * rest_in_wrist;
    now.angle_between(rigid).to_degrees()
}

#[test]
fn the_flutter_bones_swing_when_the_hips_turn_and_settle_spread_or_folded() {
    for fold in [1.0_f32, 0.0] {
        let mut rig = Rig::new(json!({}), fold);
        for _ in 0..SECOND {
            rig.step();
        }
        let (a, b) = (rig.sim.bone("wings_acc_l_flap_0"), rig.sim.bone("wings_acc_l_flap_1"));
        let wrist = rig.sim.bone("wings_acc_l_wrist");
        let rest = rig.sim.world_rotation(wrist).inverse() * (rig.sim.world(b) - rig.sim.world(a)).normalize();
        rig.sim.turn_hips(90.0);
        let mut peak = 0.0_f32;
        for _ in 0..SECOND {
            rig.step();
            peak = peak.max(flutter(&rig.sim, rest));
        }
        for _ in 0..SECOND {
            rig.step();
        }
        let settled = flutter(&rig.sim, rest);
        println!("wing fold {fold}: the flutter swings {peak} degrees and settles at {settled}");
        assert!(peak > 2.0, "fold {fold}: the flutter swings only {peak} degrees");
        assert!(settled < 1.0, "fold {fold}: settles {settled} degrees off its rest");
    }
}

#[test]
fn wings_at_rest_stay_put_and_the_frame_follows_the_fold_channel_exactly() {
    let (mut spread, mut folded) = (Rig::new(json!({}), 1.0), Rig::new(json!({}), 0.0));
    let wrist = spread.sim.bone("wings_acc_l_wrist");
    for _ in 0..SECOND {
        spread.step();
        folded.step();
    }
    let (a, b) = (spread.sim.world(wrist), folded.sim.world(wrist));
    assert!(a.distance(b) > 0.1, "the wrist is {} m apart folded and spread", a.distance(b));
    let flutter = spread.sim.bone("wings_acc_l_flap_1");
    let at = spread.sim.world(flutter);
    for _ in 0..SECOND {
        spread.step();
    }
    assert!(
        spread.sim.world(flutter).distance(at) < 1e-3,
        "the flutter tip drifts {} m",
        spread.sim.world(flutter).distance(at)
    );
}

#[test]
fn the_flutter_and_the_fold_run_without_allocating_once_they_are_warm() {
    for kind in ["feathered", "bat"] {
        let mut rig = Rig::new(json!({ "kind": kind }), 1.0);
        let fold = Channel::custom(0).unwrap();
        for frame in 0..SECOND {
            rig.sim.turn_hips(if frame % 40 < 20 { 30.0 } else { -30.0 });
            rig.channels.set(fold, if frame % 60 < 30 { 1.0 } else { 0.0 });
            rig.step();
        }
        let count = allocations_in(|| {
            for frame in 0..2 * SECOND {
                rig.sim.turn_hips(if frame % 40 < 20 { 30.0 } else { -30.0 });
                rig.channels.set(fold, if frame % 60 < 30 { 1.0 } else { 0.0 });
                rig.step();
            }
        });
        assert_eq!(count, 0, "{kind}: {count} allocations in 240 warm frames");
    }
}
