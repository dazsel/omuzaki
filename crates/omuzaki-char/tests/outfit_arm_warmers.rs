//! The `arm-warmers` garment: the conformance suite on every template and shape, plain and with a flared, banded cuff;
//! where the sleeve starts and ends on the arm; and the parameter checks.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::arm_warmers::{ArmWarmers, ArmWarmersParams, LAYER_ARM_WARMERS};
use omuzaki_char_api::BuildServices;
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

fn gauntlet() -> ArmWarmersParams {
    ArmWarmersParams { top: 0.1, past: 0.03, flare: 0.04, cuff: 0.03, ..ArmWarmersParams::default() }
}

#[test]
fn plain_sleeves_conform_on_every_template_and_shape() {
    conformance::assert_conforms::<ArmWarmers>(&ArmWarmersParams::default(), &services());
}

#[test]
fn flared_cuffed_sleeves_conform_on_every_template_and_shape() {
    conformance::assert_conforms::<ArmWarmers>(&gauntlet(), &services());
}

/// The lowest and highest x of the left sleeve's cloth (the character's left is +x).
fn reach(p: &ArmWarmersParams) -> (f32, f32) {
    let def = conformance::character(
        "standard",
        &omuzaki_char_api::BodyShape::default(),
        &[("sleeves", "arm-warmers", LAYER_ARM_WARMERS, serde_json::to_value(p).unwrap())],
    );
    let stack = conformance::build(def, &services()).unwrap();
    let xs = stack.output("sleeves").meshes.iter().flat_map(|m| m.geometry.positions.iter()).map(|q| q.x);
    xs.filter(|x| *x > 0.0).fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), x| (lo.min(x), hi.max(x)))
}

#[test]
fn the_top_starts_the_sleeve_further_down_the_arm_and_past_runs_it_over_the_hand() {
    let high = reach(&ArmWarmersParams { top: 0.1, ..ArmWarmersParams::default() });
    let gauntlet = reach(&ArmWarmersParams { top: 0.6, ..ArmWarmersParams::default() });
    assert!(gauntlet.0 > high.0 + 0.05, "a gauntlet starts lower down the arm: {gauntlet:?} vs {high:?}");
    let longer = reach(&ArmWarmersParams { past: 0.04, ..ArmWarmersParams::default() });
    assert!(longer.1 > high.1 + 0.008, "past runs it on over the hand: {longer:?} vs {high:?}");
}

#[test]
fn a_cuff_is_its_own_mesh_and_material() {
    let def = conformance::character(
        "standard",
        &omuzaki_char_api::BodyShape::default(),
        &[("sleeves", "arm-warmers", LAYER_ARM_WARMERS, serde_json::to_value(gauntlet()).unwrap())],
    );
    let stack = conformance::build(def, &services()).unwrap();
    let out = stack.output("sleeves");
    let names: Vec<&str> = out.meshes.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["sleeves-sleeves", "sleeves-cuffs"]);
    assert!(out.materials.iter().all(|(_, m)| m.double_sided), "the inside of a flared cuff shows");
}

#[test]
fn out_of_range_settings_are_refused() {
    for bad in [
        ArmWarmersParams { top: 0.95, ..ArmWarmersParams::default() },
        ArmWarmersParams { past: 0.2, ..ArmWarmersParams::default() },
        ArmWarmersParams { flare: -0.01, ..ArmWarmersParams::default() },
        ArmWarmersParams { cuff: 0.5, ..ArmWarmersParams::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
