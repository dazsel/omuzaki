//! The `straps` garment: the conformance suite on every template and shape, crossed and as a single sash over the
//! hips onto a thigh; the mirrored band lies over the first; and the parameter checks.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::straps::{LAYER_STRAPS, Strap, StrapPoint, StrapRegion, Straps, StrapsParams};
use omuzaki_char_api::BuildServices;
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

/// A sash from the left shoulder across the front, over the right hip and down the front of the right thigh.
fn sash() -> StrapsParams {
    StrapsParams {
        straps: vec![Strap {
            points: vec![
                StrapPoint::new(StrapRegion::Torso, 0.92, 0.6),
                StrapPoint::new(StrapRegion::Torso, 0.35, 0.0).lifted(0.02),
                StrapPoint::new(StrapRegion::LegRight, 0.25, -0.3),
            ],
            width: 0.04,
            mirror: false,
        }],
        edge: 0.2,
        thickness: 0.005,
        ..StrapsParams::default()
    }
}

#[test]
fn crossed_straps_conform_on_every_template_and_shape() {
    conformance::assert_conforms::<Straps>(&StrapsParams::default(), &services());
}

#[test]
fn a_sash_onto_a_thigh_conforms_on_every_template_and_shape() {
    conformance::assert_conforms::<Straps>(&sash(), &services());
}

#[test]
fn the_mirrored_band_crosses_over_the_first() {
    let p = StrapsParams::default();
    let def = conformance::character(
        "standard",
        &omuzaki_char_api::BodyShape::default(),
        &[("straps", "straps", LAYER_STRAPS, serde_json::to_value(&p).unwrap())],
    );
    let stack = conformance::build(def, &services()).unwrap();
    let mesh = &stack.output("straps").meshes[0].geometry;
    // Both bands cross the midline in front; the front-most vertex there is the second band's, a band's thickness out.
    let front = |side: f32| {
        mesh.positions
            .iter()
            .filter(|q| q.x.abs() < 0.004 && q.z > 0.0 && q.x * side >= 0.0)
            .map(|q| q.z)
            .fold(f32::MIN, f32::max)
    };
    let (left, right) = (front(1.0), front(-1.0));
    assert!(left > 0.0 && right > 0.0, "the bands reach the front midline: {left} {right}");
    let xs = mesh.positions.iter().map(|q| q.x);
    let (lo, hi) = xs.fold((f32::MAX, f32::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)));
    assert!(lo < -0.05 && hi > 0.05, "a band on each side: {lo} {hi}");
}

#[test]
fn bad_straps_are_refused() {
    let mut p = StrapsParams::default();
    p.straps.clear();
    assert!(p.validate().is_err());
    let mut p = StrapsParams::default();
    p.straps[0].points.truncate(1);
    assert!(p.validate().is_err());
    let mut p = StrapsParams::default();
    p.straps[0].width = 0.5;
    assert!(p.validate().is_err());
    let mut p = StrapsParams::default();
    p.straps[0].points[0].t = 1.2;
    assert!(p.validate().is_err());
    let mut p = StrapsParams::default();
    p.straps[0].points[0].lift = 0.5;
    assert!(p.validate().is_err());
    assert!(sash().validate().is_ok());
}
