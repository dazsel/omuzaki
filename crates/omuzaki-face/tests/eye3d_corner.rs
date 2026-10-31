//! The inner corner of the 3D eye: the caruncle is as wide, as tall and as raised as its design says, sits where its
//! inset and depth put it, the corner's shadow under the upper lid deepens as its design asks, and numbers out of range
//! are refused with their field.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::glam::Vec3;
use omuzaki_face::eye3d::{EyeDesign, meshes};
use omuzaki_face_api::Validate;
use omuzaki_face_api::eye3d::Caruncle;

/// The caruncle's vertices: the region's vertices that move when only the caruncle changes.
fn caruncle(design: &EyeDesign) -> Vec<Vec3> {
    let mut other = design.clone();
    other.skin.caruncle.width = if design.skin.caruncle.width > 0.002 { 0.001 } else { 0.004 };
    let build = |d: &EyeDesign| meshes(d, Vec3::new(0.0, 0.0, 0.07), Vec3::Z).region.positions;
    let (ours, theirs) = (build(design), build(&other));
    assert_eq!(ours.len(), theirs.len(), "the caruncle's size never changes the region's layout");
    ours.iter().zip(&theirs).filter(|(a, b)| a != b).map(|(a, _)| *a).collect()
}

fn extent(points: &[Vec3]) -> (Vec3, Vec3) {
    let min = points.iter().fold(Vec3::splat(f32::INFINITY), |m, p| m.min(*p));
    let max = points.iter().fold(Vec3::splat(f32::NEG_INFINITY), |m, p| m.max(*p));
    (min, max)
}

#[test]
fn the_caruncle_is_the_size_and_place_its_design_says() {
    let small = EyeDesign::default();
    let mut large = EyeDesign::default();
    large.skin.caruncle =
        Caruncle { width: 0.004, height: 0.003, rise: 0.0012, depth: 0.0004, inset: 0.0018, ..Caruncle::default() };

    let (lo, hi) = extent(&caruncle(&small));
    let width = hi.x - lo.x;
    assert!((width - 0.003).abs() < 0.000_4, "the default caruncle is 3 mm wide: {width}");
    let (big_lo, big_hi) = extent(&caruncle(&large));
    let (w, h) = (big_hi.x - big_lo.x, big_hi.y - big_lo.y);
    assert!((w - 0.004).abs() < 0.000_5 && (h - 0.003).abs() < 0.000_5, "4 by 3 mm: {w} by {h}");
    assert!(big_hi.z > hi.z + 0.000_5, "raised and nearer the surface, it stands further out: {} {}", big_hi.z, hi.z);
    let centre = |lo: Vec3, hi: Vec3| f32::midpoint(lo.x, hi.x);
    let inner_x = small.aperture.inner_x;
    assert!((centre(lo, hi) - (inner_x - 0.001_3)).abs() < 0.000_2, "it sits its inset in from the corner");
    assert!((centre(big_lo, big_hi) - (inner_x - 0.001_8)).abs() < 0.000_3);
}

/// The darkest cavity on the skin in the inner corner's upper quarter (x past the opening's middle, y above the corner).
fn corner_cavity(design: &EyeDesign) -> f32 {
    let region = meshes(design, Vec3::new(0.0, 0.0, 0.07), Vec3::Z).region;
    let info = region.extra.iter().find(|e| e.name.as_str() == "skin-info").expect("the skin's info");
    let a = &design.aperture;
    region
        .positions
        .iter()
        .zip(info.data.chunks(4))
        .filter(|(p, _)| p.x > a.inner_x - 0.004 && p.x < a.inner_x && p.y > a.inner_y + 0.001)
        .map(|(_, i)| i[1])
        .fold(f32::INFINITY, f32::min)
}

#[test]
fn the_corner_shadow_darkens_the_inner_corner_under_the_lid() {
    let plain = EyeDesign::default();
    let mut shaded = EyeDesign::default();
    shaded.skin.corner_shadow = 1.0;
    let (before, after) = (corner_cavity(&plain), corner_cavity(&shaded));
    assert!(after < 0.5 * before, "a full corner shadow at least halves the light there: {before} to {after}");
    let lower = |d: &EyeDesign| {
        let region = meshes(d, Vec3::new(0.0, 0.0, 0.07), Vec3::Z).region;
        let info = region.extra.iter().find(|e| e.name.as_str() == "skin-info").expect("the skin's info");
        let a = &d.aperture;
        region
            .positions
            .iter()
            .zip(info.data.chunks(4))
            .filter(|(p, _)| p.x > a.inner_x - 0.004 && p.x < a.inner_x && p.y < a.inner_y - 0.001)
            .map(|(_, i)| i[1])
            .sum::<f32>()
    };
    assert!((lower(&plain) - lower(&shaded)).abs() < 1e-3, "the lower lid keeps its light");
}

#[test]
fn a_caruncle_out_of_range_is_refused_with_its_field() {
    let mut design = EyeDesign::default();
    design.skin.caruncle.rise = 0.01;
    let err = design.validate().unwrap_err().to_string();
    assert!(err.contains("caruncle") && err.contains("rise"), "{err}");
    design.skin.caruncle.rise = 0.001;
    design.skin.caruncle.flush = 1.5;
    assert!(design.validate().unwrap_err().to_string().contains("flush"));
}
