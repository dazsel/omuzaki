//! Hair follows the head it grows on (#2262): widening, deepening or raising the cranium carries the cap, the fringe and
//! the locks with it, so no bare scalp shows past the hair at the sides, the back, the top or the temples.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)] // test helpers; vertex counts

mod common;

use common::at;
use omuzaki_char_api::BuiltCharacter;
use omuzaki_core::glam::Vec3;
use serde_json::json;

/// The kit on `template` with the cranium's `field` scaled by `scale`, built.
fn built(template: &str, field: &str, scale: f64) -> BuiltCharacter {
    let mut doc = omuzaki_doc_mock::MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": template}));
    let path = format!("cranium/{field}");
    let value = at(&doc, &format!("characters/kit/parts/head/params/{path}")).and_then(|v| v.as_f64()).unwrap();
    common::apply(
        &mut doc,
        "part.set_param",
        json!({"character": "kit", "part": "head", "path": path, "value": value * scale}),
    );
    common::build(&doc, "kit")
}

/// What covers the scalp: the head's upper vertices away from the face (every fourth), and how many no hair covers (the
/// ray from the vertex straight out from the head's middle meets no hair triangle); and the head's and the hair's
/// widest `|x|` up there.
struct Cover {
    scalp: usize,
    bare: usize,
    head_width: f32,
    hair_width: f32,
}

fn cover(built: &BuiltCharacter) -> Cover {
    let head = &built.meshes.iter().find(|m| m.mesh.name == "head-head").expect("the head").mesh.geometry.positions;
    let centre = head.iter().fold(Vec3::ZERO, |a, p| a + *p) / head.len() as f32;
    let (lo, hi) = head.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
    let above = lo + (hi - lo) * 0.45;
    let hair: Vec<Vec3> = built
        .meshes
        .iter()
        .filter(|m| m.part.as_str() == "hair")
        .flat_map(|m| m.mesh.geometry.positions.iter().copied())
        .collect();
    // The scalp: above 45% of the head's height (the tops of the ears) and not facing forward (the face and the forehead under the fringe).
    let scalp: Vec<Vec3> = head.iter().copied().filter(|p| p.y > above && (*p - centre).normalize().z < 0.5).collect();
    let triangles = hair_triangles(built);
    let bare = scalp.iter().step_by(4).filter(|v| !covered(&triangles, **v, (**v - centre).normalize())).count();
    let widest =
        |points: &mut dyn Iterator<Item = Vec3>| points.filter(|p| p.y > above).map(|p| p.x.abs()).fold(0.0, f32::max);
    Cover {
        scalp: scalp.len().div_ceil(4),
        bare,
        head_width: widest(&mut head.iter().copied()),
        hair_width: widest(&mut hair.iter().copied()),
    }
}

/// Every triangle of the hair.
fn hair_triangles(built: &BuiltCharacter) -> Vec<[Vec3; 3]> {
    built
        .meshes
        .iter()
        .filter(|m| m.part.as_str() == "hair")
        .flat_map(|m| {
            let g = &m.mesh.geometry;
            g.indices.as_chunks::<3>().0.iter().map(|t| t.map(|i| g.positions[i as usize]))
        })
        .collect()
}

#[test]
fn the_hair_covers_a_widened_deepened_or_raised_cranium() {
    for template in ["soft-sd", "standard"] {
        let base = cover(&built(template, "half_width", 1.0));
        for (field, scale) in
            [("half_width", 1.0), ("half_width", 1.5), ("half_depth_back", 1.3), ("half_height_up", 1.3)]
        {
            let c = cover(&built(template, field, scale));
            let share = c.bare as f32 / c.scalp.max(1) as f32;
            println!(
                "{template}, {field} x{scale}: {} of {} scalp vertices bare; head {:.3} m -> {:.3} m wide, hair {:.3} m -> {:.3} m",
                c.bare, c.scalp, base.head_width, c.head_width, base.hair_width, c.hair_width
            );
            assert!(share <= 0.005, "{template}, {field} x{scale}: {:.1}% of the scalp shows", share * 100.0);
            assert!(
                c.hair_width > c.head_width + 0.01,
                "{template}, {field} x{scale}: the hair ({} m) is no wider than the head ({} m)",
                c.hair_width,
                c.head_width
            );
        }
    }
}

/// How far outside a triangle, in barycentric terms, a ray still counts as meeting it.
const EDGE: f32 = 1e-3;

/// Whether the ray from `from` along `dir` meets any of `triangles` (Moller-Trumbore).
fn covered(triangles: &[[Vec3; 3]], from: Vec3, dir: Vec3) -> bool {
    triangles.iter().any(|[a, b, c]| {
        let (e1, e2) = (*b - *a, *c - *a);
        let across = dir.cross(e2);
        let det = e1.dot(across);
        if det.abs() < 1e-12 {
            return false;
        }
        let offset = from - *a;
        let u = offset.dot(across) / det;
        let q = offset.cross(e1);
        let v = dir.dot(q) / det;
        // A small tolerance: a ray down the midline runs along the seam where a shell's two sides meet.
        u >= -EDGE && v >= -EDGE && u + v <= 1.0 + EDGE && e2.dot(q) / det > 0.0
    })
}

/// The share of the head's upper vertices (within 80 degrees of the top, seen from the head's middle, all round, the
/// forehead and temples included; every other one) that no hair covers, cast straight out.
fn bare_above_the_brows(built: &BuiltCharacter) -> (usize, usize) {
    let head = &built.meshes.iter().find(|m| m.mesh.name == "head-head").expect("the head").mesh.geometry.positions;
    let centre = head.iter().fold(Vec3::ZERO, |a, p| a + *p) / head.len() as f32;
    let triangles = hair_triangles(built);
    let upper: Vec<Vec3> =
        head.iter().step_by(2).map(|v| *v - centre).filter(|d| d.normalize().y >= 80f32.to_radians().cos()).collect();
    let bare = upper.iter().filter(|d| !covered(&triangles, centre + **d, d.normalize())).count();
    (bare, upper.len())
}

#[test]
fn the_temples_stay_covered_as_the_head_widens() {
    for template in ["soft-sd", "standard"] {
        for scale in [1.0, 1.25, 1.5] {
            let (bare, upper) = bare_above_the_brows(&built(template, "half_width", scale));
            println!("{template}, half_width x{scale}: {bare} of {upper} vertices above the brows bare");
            assert!(bare as f32 <= upper as f32 * 0.005, "{template} x{scale}: {bare} of {upper} bare");
        }
    }
}
