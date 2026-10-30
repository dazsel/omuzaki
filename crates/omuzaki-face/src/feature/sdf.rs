//! Signed distances: the forms a feature is sculpted from (spheres, ellipsoids, capsules, cones, tubes along a curve)
//! and the smooth blends that join and carve them. Negative inside, positive outside, in metres. The blends follow
//! Inigo Quilez's polynomial smooth minimum (MIT); the ellipsoid is his first-order bound, exact on the surface, which
//! is all the mesher and the normals need.

use omuzaki_core::glam::{Mat3, Vec3};

/// A sphere of radius `r` round `c`.
pub fn sphere(p: Vec3, c: Vec3, r: f32) -> f32 {
    (p - c).length() - r
}

/// An ellipsoid round `c` with semi-axes `radii` along x, y and z.
pub fn ellipsoid(p: Vec3, c: Vec3, radii: Vec3) -> f32 {
    let q = p - c;
    let k0 = (q / radii).length();
    let k1 = (q / (radii * radii)).length();
    if k1 <= f32::EPSILON {
        return -radii.min_element();
    }
    k0 * (k0 - 1.0) / k1
}

/// An ellipsoid round `c` whose semi-axes `radii` lie along the columns of `axes` (a rotation).
pub fn ellipsoid_turned(p: Vec3, c: Vec3, axes: Mat3, radii: Vec3) -> f32 {
    ellipsoid(axes.transpose() * (p - c), Vec3::ZERO, radii)
}

/// A capsule: the segment `a`–`b` thickened by `r`.
pub fn capsule(p: Vec3, a: Vec3, b: Vec3, r: f32) -> f32 {
    let (pa, ba) = (p - a, b - a);
    let h = (pa.dot(ba) / ba.length_squared().max(f32::EPSILON)).clamp(0.0, 1.0);
    (pa - ba * h).length() - r
}

/// A cone with round ends: a sphere of radius `ra` at `a` swept to one of `rb` at `b`.
pub fn round_cone(p: Vec3, a: Vec3, b: Vec3, ra: f32, rb: f32) -> f32 {
    let ba = b - a;
    let l2 = ba.length_squared();
    if l2 <= f32::EPSILON {
        return sphere(p, a, ra.max(rb));
    }
    let rr = ra - rb;
    let a2 = l2 - rr * rr;
    let il2 = 1.0 / l2;
    let pa = p - a;
    let y = pa.dot(ba);
    let z = y - l2;
    let x2 = (pa * l2 - ba * y).length_squared();
    let y2 = y * y * l2;
    let z2 = z * z * l2;
    let k = rr.signum() * rr * rr * x2;
    if z.signum() * a2 * z2 > k {
        return (x2 + z2).sqrt() * il2 - rb;
    }
    if y.signum() * a2 * y2 < k {
        return (x2 + y2).sqrt() * il2 - ra;
    }
    ((x2 * a2 * il2).sqrt() + y * rr) * il2 - ra
}

/// A tube along the polyline `points`, its radius running through `radii` (one per point): round cones joined end to
/// end, blended over `k` so the joints don't show.
pub fn tube(p: Vec3, points: &[Vec3], radii: &[f32], k: f32) -> f32 {
    let mut d = f32::INFINITY;
    for (seg, r) in points.windows(2).zip(radii.windows(2)) {
        d = smin(d, round_cone(p, seg[0], seg[1], r[0], r[1]), k);
    }
    d
}

/// The smooth union of two distances over `k` metres (the polynomial smooth minimum).
pub fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    if !a.is_finite() || !b.is_finite() {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

/// The smooth intersection of two distances over `k` metres.
pub fn smax(a: f32, b: f32, k: f32) -> f32 {
    -smin(-a, -b, k)
}

/// `a` with `b` carved out of it, the cut's edge rounded over `k` metres.
pub fn carve(a: f32, b: f32, k: f32) -> f32 {
    smax(a, -b, k)
}

/// A rotation whose columns are `x`, `y` and `z` turned to be square: `z` is kept, `y` made square to it.
pub fn frame(y: Vec3, z: Vec3) -> Mat3 {
    let z = z.normalize_or(Vec3::Z);
    let x = y.cross(z).normalize_or(Vec3::X);
    Mat3::from_cols(x, z.cross(x), z)
}

/// Smooth Hermite step from `e0` to `e1`.
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forms_are_signed_distances_on_their_surfaces() {
        let tol = 1e-5;
        assert!(sphere(Vec3::new(2.0, 0.0, 0.0), Vec3::ZERO, 1.0) - 1.0 < tol);
        assert!(ellipsoid(Vec3::new(0.0, 2.0, 0.0), Vec3::ZERO, Vec3::new(1.0, 2.0, 3.0)).abs() < tol);
        assert!(ellipsoid(Vec3::ZERO, Vec3::ZERO, Vec3::splat(1.0)) < 0.0);
        let (a, b) = (Vec3::ZERO, Vec3::new(0.0, 0.0, 2.0));
        assert!((capsule(Vec3::new(1.0, 0.0, 1.0), a, b, 0.5) - 0.5).abs() < tol);
        assert!((round_cone(Vec3::new(0.0, 0.0, -1.0), a, b, 0.5, 0.25) - 0.5).abs() < tol);
        assert!((round_cone(Vec3::new(0.0, 0.0, 3.0), a, b, 0.5, 0.25) - 0.75).abs() < tol);
        let turned = frame(Vec3::Y, Vec3::X);
        assert!(ellipsoid_turned(Vec3::new(3.0, 0.0, 0.0), Vec3::ZERO, turned, Vec3::new(1.0, 2.0, 3.0)).abs() < tol);
    }

    #[test]
    fn the_blends_round_only_within_their_reach() {
        assert_eq!(smin(0.0, 1.0, 0.5), 0.0);
        assert!(smin(0.0, 0.0, 0.5) < 0.0);
        assert_eq!(smax(0.0, -1.0, 0.5), 0.0);
        assert!(carve(0.0, 0.0, 0.5) > 0.0);
        assert_eq!(smin(f32::INFINITY, 1.0, 0.5), 1.0);
    }
}
