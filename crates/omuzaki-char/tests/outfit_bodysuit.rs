//! The bodysuit garment (#2447): the conformance suite for its necks and sleeves, how close it follows the body, and its
//! parameters.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::bodysuit::{Bodysuit, BodysuitParams, LAYER_BODYSUIT};
use omuzaki_char::outfit::coat::NeckShape;
use omuzaki_char::outfit::dress::DressSleeves;
use omuzaki_char_api::{BodyRegion, BodyShape, BuildServices, FitFact};
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

#[test]
fn the_default_bodysuit_passes_the_suite() {
    let rows = conformance::assert_conforms::<Bodysuit>(&BodysuitParams::default(), &services());
    assert_eq!(rows.len(), 12);
}

#[test]
fn every_neck_and_sleeve_passes_the_suite() {
    for shape in [NeckShape::High, NeckShape::Halter, NeckShape::Scoop, NeckShape::Round] {
        let mut p = BodysuitParams { sleeves: DressSleeves::Straps, ..BodysuitParams::default() };
        p.neckline.shape = shape;
        if !shape.stands() {
            p.neckline.depth = 0.08;
            p.neckline.rise = -0.08;
        }
        conformance::run::<Bodysuit>(&p, &services()).unwrap_or_else(|e| panic!("{shape:?}: {e}"));
    }
    for sleeves in [DressSleeves::Short, DressSleeves::Puff] {
        let p = BodysuitParams { sleeves, ..BodysuitParams::default() };
        conformance::run::<Bodysuit>(&p, &services()).unwrap_or_else(|e| panic!("{sleeves:?}: {e}"));
    }
}

/// The distance from `p` to the triangle `abc` (Ericson's closest point).
fn to_triangle(p: DVec3, [a, b, c]: [DVec3; 3]) -> f64 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return ap.length();
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return bp.length();
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return (p - (a + ab * (d1 / (d1 - d3)))).length();
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return cp.length();
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return (p - (a + ac * (d2 / (d2 - d6)))).length();
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        return (p - (b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))))).length();
    }
    let denom = 1.0 / (va + vb + vc);
    (p - (a + ab * (vb * denom) + ac * (vc * denom))).length()
}

/// The waist's and the bust's heights, as fractions of the torso.
const BANDS: (std::ops::RangeInclusive<f64>, std::ops::RangeInclusive<f64>) = (0.35..=0.55, 0.65..=0.8);

/// How far the bodysuit's inside stands off the torso's own mesh, at most, over the bust and the waist: for each
/// height and each of the front, the side and the back, the nearest of its vertices to the mesh there.
fn worst_gap(shape: &BodyShape) -> (f64, String) {
    let value = serde_json::to_value(BodysuitParams::default()).unwrap();
    let def = conformance::character("tall", shape, &[(conformance::PART, "bodysuit", LAYER_BODYSUIT, value)]);
    let stack = conformance::build(def, &services()).unwrap();
    let fit = stack.before(conformance::PART).get::<FitFact>().fits[0].clone();
    let name = omuzaki_char::body::mesh::name_of(BodyRegion::Torso);
    let mut triangles = Vec::new();
    for mesh in stack.output(conformance::BODY).meshes.iter().filter(|m| m.name == name) {
        let g = &mesh.geometry;
        for t in g.indices.as_chunks::<3>().0 {
            triangles.push(t.map(|k| g.positions[k as usize].as_dvec3()));
        }
    }
    let section = |t: f64| fit.section(BodyRegion::Torso, t as f32).unwrap();
    let (bottom, top) = (f64::from(section(0.0).centre.y), f64::from(section(1.0).centre.y));
    let mut nearest = std::collections::BTreeMap::<(i32, usize), f64>::new();
    // The body of it, not its sleeves: an arm hangs beside the torso's side.
    for mesh in stack.output(conformance::PART).meshes.iter().filter(|m| m.name.ends_with("-shell")) {
        for v in &mesh.geometry.positions {
            let v = v.as_dvec3();
            let t = (v.y - bottom) / (top - bottom);
            // The waist and the bust: below the shoulder blades, where the fit stands off the back by its own gap.
            if !(BANDS.0.contains(&t) || BANDS.1.contains(&t)) {
                continue;
            }
            let c = section(t).centre.as_dvec3();
            let angle = (v.x - c.x).atan2(v.z - c.z).to_degrees().abs();
            let Some(side) = [0.0, 90.0, 180.0].iter().position(|a: &f64| (angle - a).abs() < 8.0) else { continue };
            let d = triangles.iter().map(|&tri| to_triangle(v, tri)).fold(f64::INFINITY, f64::min);
            let at = nearest.entry(((t * 40.0).round() as i32, side)).or_insert(f64::INFINITY);
            *at = at.min(d);
        }
    }
    assert!(nearest.len() > 20, "the bodysuit covers the bust and the waist front, side and back");
    let ((height, side), gap) =
        nearest.iter().map(|(k, d)| (*k, *d)).fold(((0, 0_usize), 0.0), |w, x| if x.1 > w.1 { x } else { w });
    (gap, format!("t = {:.3}, {}", f64::from(height) / 40.0, ["front", "side", "back"][side]))
}

#[test]
fn it_follows_the_bust_and_the_waist_within_4_mm() {
    for (label, shape) in conformance::shapes() {
        let (gap, at) = worst_gap(&shape);
        assert!(gap <= 0.004, "{label}: its inside stands {:.1} mm off the body at {at}", gap * 1e3);
    }
}

#[test]
fn the_parameters_are_checked() {
    assert!(BodysuitParams::default().validate().is_ok());
    let mut halter = BodysuitParams::default();
    halter.neckline.shape = NeckShape::Halter;
    for bad in [
        halter,
        BodysuitParams { length: 0.5, ..BodysuitParams::default() },
        BodysuitParams { thickness: 0.05, ..BodysuitParams::default() },
        BodysuitParams { strap_width: 0.2, ..BodysuitParams::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
