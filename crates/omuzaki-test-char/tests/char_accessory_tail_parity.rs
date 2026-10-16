//! The `tail` accessory against the prototype and the real spring solver (CB-43, #196).
//!
//! Two things cross the crate boundary here. The prototype's dump is the parity target for the tail's size; the prototype's
//! sample character wears no tail, so today the comparison skips (see `reference_meshes`). And the spring chain the tail
//! contributes runs on `omuzaki-rig`'s VRM solver: a 90 degree hips turn makes it lag and settle, with no allocation per frame
//! once warm.
//!
//! **Lag** is how long the tail's tip takes to cover half of the hips' turn: the tip's yaw about the hips, from the moment
//! the hips are turned 90 degrees in one frame until the tip is 45 degrees round. A rigid tail has none; a limp one is slow.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

mod char_accessory_support;

use char_accessory_support::{FPS, SECOND, Sim, allocations_in, built, compare_extents, reference_meshes};
use omuzaki_anim_api::HumanoidBone;
use serde_json::{Value, json};

/// The whole lag, from the turn to the end of the run.
struct Response {
    /// Milliseconds until the tip has covered half the turn.
    lag_ms: f32,
    /// The most the tip has turned, degrees.
    peak: f32,
    /// How far it has turned at the end, degrees.
    settled: f32,
}

/// The yaw of the tip about the hips, degrees.
fn yaw(sim: &Sim, tip: omuzaki_anim_api::BoneId) -> f32 {
    let hips = sim.b.rig.skeleton.humanoid_bone(HumanoidBone::Hips).unwrap();
    let d = sim.world(tip) - sim.world(hips);
    d.x.atan2(-d.z).to_degrees()
}

/// Turns the hips 90 degrees in one frame and follows the tip for two seconds.
fn respond(params: Value) -> Response {
    let b = built("standard", "tail", params);
    let last = b.rig.springs.chains.iter().find(|c| c.name == "tail_acc").unwrap().joints.len() - 1;
    let mut sim = Sim::new(b);
    let tip = sim.bone(&format!("tail_acc_{last}"));
    for _ in 0..SECOND {
        sim.step();
    }
    let start = yaw(&sim, tip);
    sim.turn_hips(90.0);
    let mut response = Response { lag_ms: f32::NAN, peak: 0.0, settled: 0.0 };
    for frame in 1..=2 * SECOND {
        sim.step();
        let turned = (yaw(&sim, tip) - start).abs();
        response.peak = response.peak.max(turned);
        if response.lag_ms.is_nan() && turned >= 45.0 {
            response.lag_ms = frame as f32 * 1000.0 / FPS;
        }
        response.settled = turned;
    }
    response
}

#[test]
fn the_prototypes_tail_is_the_size_of_ours() {
    let Some(reference) = reference_meshes("tail", &["tail"]) else { return };
    compare_extents("tail", &built("standard", "tail", json!({})), &reference);
}

#[test]
fn a_90_degree_hips_turn_makes_the_fox_tail_lag_between_40_and_150_ms_then_settle() {
    let r = respond(json!({}));
    println!("fox tail: lag {} ms, peak {} degrees, settled at {} degrees", r.lag_ms, r.peak, r.settled);
    assert!((40.0..=150.0).contains(&r.lag_ms), "the tip took {} ms to cover half the turn", r.lag_ms);
    assert!(r.peak < 100.0, "overshoots to {} degrees", r.peak);
    assert!((r.settled - 90.0).abs() < 1.0, "settles at {} degrees, not 90", r.settled);
}

#[test]
fn every_preset_lags_a_little_and_settles() {
    for kind in ["fox", "cat", "dragon", "bunny"] {
        let r = respond(json!({ "kind": kind }));
        println!("{kind} tail: lag {} ms, peak {} degrees, settled at {} degrees", r.lag_ms, r.peak, r.settled);
        assert!(r.lag_ms.is_finite() && r.lag_ms < 300.0, "{kind}: lag {} ms", r.lag_ms);
        assert!((r.settled - 90.0).abs() < 2.0, "{kind}: settles at {} degrees", r.settled);
    }
}

#[test]
fn a_stiffer_tail_follows_faster() {
    let (limp, stiff) = (
        respond(json!({ "stiffness": 0.8, "stiffness_tip": 0.3 })),
        respond(json!({ "stiffness": 3.0, "stiffness_tip": 1.5 })),
    );
    assert!(stiff.lag_ms < limp.lag_ms, "{} ms stiff against {} ms limp", stiff.lag_ms, limp.lag_ms);
}

#[test]
fn a_tail_at_rest_stays_put() {
    let mut sim = Sim::new(built("standard", "tail", json!({})));
    let tip = sim.bone("tail_acc_4");
    for _ in 0..2 * SECOND {
        sim.step();
    }
    let at = sim.world(tip);
    for _ in 0..SECOND {
        sim.step();
    }
    assert!(sim.world(tip).distance(at) < 1e-3, "the tip drifts {} m in a second", sim.world(tip).distance(at));
}

#[test]
fn the_chain_runs_without_allocating_once_it_is_warm() {
    for kind in ["fox", "dragon"] {
        let mut sim = Sim::new(built("standard", "tail", json!({ "kind": kind })));
        for frame in 0..SECOND {
            sim.turn_hips(if frame % 40 < 20 { 30.0 } else { -30.0 });
            sim.step();
        }
        let count = allocations_in(|| {
            for frame in 0..2 * SECOND {
                sim.turn_hips(if frame % 40 < 20 { 30.0 } else { -30.0 });
                sim.step();
            }
        });
        assert_eq!(count, 0, "{kind}: {count} allocations in 240 warm frames");
    }
}
