//! Frames and elliptical sweeps along a polyline, tubes, and the placement matrix (port of `uniform-geo.js`
//! `frames`, `sweep`, `tube` and `place`, lines 216 to 273).
//!
//! A sweep carries an elliptical ring along a path. Its orientation comes from [`frames`], which turns a reference
//! "up" direction along the path by parallel transport (each frame is the previous one with the new tangent projected
//! out), so the ring does not spin about the path as it bends. Rings are the rows of a [`Grid`] closed along `j`:
//! build it with `wrap_j` and `inside: |i, _| sweep.centres[i]` in [`GridOptions`](super::grid::GridOptions).

use omuzaki_core::glam::{DMat4, DVec3, DVec4};

use super::KitError;
use super::curve::{TAU, fraction};
use super::grid::Grid;

/// The reference direction a path's first normal is cut from unless the caller picks one: world +Z, the front of the
/// character.
pub const DEFAULT_UP: DVec3 = DVec3::Z;

/// A first normal shorter than this (squared) means "up" is nearly parallel to the path, and the next candidate takes
/// over. The prototype's threshold.
const PARALLEL_UP: f64 = 1e-8;

/// A carried normal shorter than this (squared) after the tangent is projected out has collapsed onto the tangent
/// (the path turned sharply back along it) and is cut afresh. Far below anything a real turn reaches.
const COLLAPSED_NORMAL: f64 = 1e-12;

/// A point of a path with its orthonormal frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The path point.
    pub p: DVec3,
    /// The unit tangent.
    pub t: DVec3,
    /// The unit normal, carried along the path by parallel transport.
    pub n: DVec3,
    /// The unit binormal, `t x n`.
    pub b: DVec3,
}

/// `v` scaled to unit length, or zero for a zero or non-finite length (three's `normalize`, which never divides by 0).
fn unit(v: DVec3) -> DVec3 {
    let length = v.length();
    if length > 0.0 && length.is_finite() { v / length } else { DVec3::ZERO }
}

/// `v` with its component along the unit vector `t` removed and the rest normalised, if more than `min_length_sq`
/// of it (squared) is left.
fn perpendicular(v: DVec3, t: DVec3, min_length_sq: f64) -> Option<DVec3> {
    let cut = v + t * -v.dot(t);
    (cut.length_squared() >= min_length_sq).then(|| unit(cut))
}

/// The first normal: `up` cut square to the tangent, or, if `up` runs along the path, +X, or, if the path runs along
/// +X as well, +Y. (The prototype stops at +X; +Y keeps a path along X from collapsing.) A unit tangent cannot run
/// along both X and Y, so a candidate is always found and the error is unreachable.
fn first_normal(up: DVec3, t: DVec3) -> Result<DVec3, KitError> {
    [up, DVec3::X, DVec3::Y]
        .into_iter()
        .find_map(|reference| perpendicular(reference, t, PARALLEL_UP))
        .ok_or(KitError::Degenerate { what: "the direction to start a path's frames from" })
}

/// The frames along a polyline by parallel transport: the tangent at each point (forward and backward differences at the
/// ends, central inside), a normal cut from `up` at the start and carried along, and the binormal. This is the
/// prototype's `frames(pts, up)`; the prototype's default `up` is [`DEFAULT_UP`].
///
/// If the path turns sharply back along the carried normal, so that nothing of it is left once the tangent is
/// projected out, the normal is cut afresh from `up` instead of collapsing to zero as the prototype's does.
///
/// # Errors
/// - [`KitError::TooFew`] for fewer than 2 points.
/// - [`KitError::NonFinite`] for a NaN or infinite coordinate in `pts` or `up`.
/// - [`KitError::Degenerate`] if a tangent has no length: neighbouring points coincide, or the points either side of
///   an interior one do.
pub fn frames(pts: &[DVec3], up: DVec3) -> Result<Vec<Frame>, KitError> {
    let count = pts.len();
    if count < 2 {
        return Err(KitError::TooFew { what: "points of a path", need: 2, got: count });
    }
    if pts.iter().any(|p| !p.is_finite()) {
        return Err(KitError::NonFinite { what: "a path's points" });
    }
    if !up.is_finite() {
        return Err(KitError::NonFinite { what: "an up vector" });
    }
    let mut out: Vec<Frame> = Vec::with_capacity(count);
    for (k, &p) in pts.iter().enumerate() {
        let (from, to) = if k == 0 {
            (pts[0], pts[1])
        } else if k == count - 1 {
            (pts[k - 1], pts[k])
        } else {
            (pts[k - 1], pts[k + 1])
        };
        let t = unit(to - from);
        if t == DVec3::ZERO {
            return Err(KitError::Degenerate { what: "a path tangent (zero or overflowing length)" });
        }
        let carried = out.last().and_then(|previous| perpendicular(previous.n, t, COLLAPSED_NORMAL));
        let n = match carried {
            Some(n) => n,
            None => first_normal(up, t)?,
        };
        out.push(Frame { p, t, n, b: unit(t.cross(n)) });
    }
    Ok(out)
}

/// How a sweep is oriented and shaped beyond its path and radius.
#[derive(Clone)]
pub struct SweepOptions<'a> {
    /// The direction the first ring's normal is cut from. Default: [`DEFAULT_UP`].
    pub up: DVec3,
    /// A multiplier of the ring's two semi-axes at path parameter `t` (0 to 1) and ring angle `phi`, for superelliptic
    /// and other custom sections. Default: none (an ellipse).
    pub shape: Option<&'a dyn Fn(f64, f64) -> (f64, f64)>,
    /// A turn of the ring about the path at parameter `t`, in radians. Default: none.
    pub twist: Option<&'a dyn Fn(f64) -> f64>,
}

impl Default for SweepOptions<'_> {
    fn default() -> Self {
        Self { up: DEFAULT_UP, shape: None, twist: None }
    }
}

/// A swept surface: its rings as the rows of a grid, and the path points they are centred on.
#[derive(Clone, Debug, PartialEq)]
pub struct Sweep {
    /// One row per path point, each a ring of `seg + 1` points whose last repeats the first. Closed along `j`.
    pub grid: Grid,
    /// The centre of each ring: the path points, for the interior reference of the grid's normals.
    pub centres: Vec<DVec3>,
}

/// Sweeps an elliptical section along the polyline `pts`. `rad(t)` gives the two semi-axes `(along the normal, along
/// the binormal)` at path parameter `t` (the point's index over the last index, so 0 to 1), and `seg` is the number of
/// segments round the ring (the prototype's default is 16). A radius that goes to 0 at the ends gives rounded caps.
/// This is the prototype's `sweep(pts, rad, seg, { up, shape, twist })`.
///
/// # Errors
/// - [`KitError::TooFew`] for fewer than 2 points or `seg == 0`.
/// - [`KitError::Overflow`] if `seg + 1` or the point count overflows `usize`.
/// - [`KitError::NonFinite`] for a NaN or infinite coordinate, or a radius, shape or twist that makes a point non-finite.
/// - [`KitError::Degenerate`] as [`frames`].
pub fn sweep(
    pts: &[DVec3],
    rad: impl Fn(f64) -> (f64, f64),
    seg: usize,
    options: &SweepOptions<'_>,
) -> Result<Sweep, KitError> {
    if seg == 0 {
        return Err(KitError::TooFew { what: "segments round a ring", need: 1, got: 0 });
    }
    let cols = seg.checked_add(1).ok_or(KitError::Overflow { what: "columns of a ring (segments + 1)" })?;
    let path = frames(pts, options.up)?;
    let last = path.len() - 1;
    // The prototype evaluates the radius and the twist once per ring, not once per point.
    let rings: Vec<Ring> = path
        .into_iter()
        .enumerate()
        .map(|(k, frame)| {
            let t = fraction(k, last);
            Ring { frame, t, radii: rad(t), twist: options.twist.map_or(0.0, |twist| twist(t)) }
        })
        .collect();
    let grid = Grid::from_fn(rings.len(), cols, |k, j| {
        let Ring { frame, t, radii: (r0, r1), twist } = rings[k];
        let phi = fraction(j, seg) * TAU + twist;
        let (mut a, mut b) = (phi.cos(), phi.sin());
        if let Some(shape) = options.shape {
            let (da, db) = shape(t, phi);
            a *= da;
            b *= db;
        }
        frame.p + frame.n * a * r0 + frame.b * b * r1
    })?;
    Ok(Sweep { grid, centres: pts.to_vec() })
}

/// One ring of a sweep, with everything that does not change round it.
#[derive(Clone, Copy)]
struct Ring {
    frame: Frame,
    /// The ring's path parameter, 0 to 1.
    t: f64,
    /// The semi-axes along the normal and the binormal.
    radii: (f64, f64),
    twist: f64,
}

/// How a tube closes at its ends: the radius is scaled by `sqrt(TAPER_RATE * e + TAPER_TIP)`, at most 1, for `e` the
/// path parameter's distance from the nearer end. That is `sqrt(TAPER_TIP)` = one hundredth at the very end, and full
/// size from `e` of about 0.025.
const TAPER_RATE: f64 = 40.0;
const TAPER_TIP: f64 = 0.0001;

/// A thin round tube along `pts` (piping, a seam, a stitch line): a [`sweep`] whose radius is `r(t)` shrunk towards both
/// ends to a rounded cap of one hundredth of it, with `seg` segments round (the prototype's default is 6). This is the
/// prototype's `tube(pts, r, seg)`.
///
/// # Errors
/// As [`sweep`].
pub fn tube(pts: &[DVec3], r: impl Fn(f64) -> f64, seg: usize) -> Result<Sweep, KitError> {
    let radius = |t: f64| {
        let end_distance = t.min(1.0 - t);
        let taper = (end_distance.max(0.0) * TAPER_RATE + TAPER_TIP).sqrt().min(1.0);
        let k = r(t) * taper;
        (k, k)
    };
    sweep(pts, radius, seg, &SweepOptions::default())
}

/// The transform that puts a primitive built along +Y onto the axis `dir` at `pos`: local +Y goes to `dir`, the origin
/// to `pos`, then the primitive is optionally turned by `spin` radians about `dir` and scaled uniformly by `scale`.
/// The two other axes are cut from +X (or +Z when `dir` is nearly along X). This is the prototype's
/// `place(pos, dir, spin, scale)`.
///
/// # Errors
/// - [`KitError::NonFinite`] for a NaN or infinite input.
/// - [`KitError::Degenerate`] if `dir` has no length.
pub fn place(pos: DVec3, dir: DVec3, spin: f64, scale: f64) -> Result<DMat4, KitError> {
    if !(pos.is_finite() && dir.is_finite() && spin.is_finite() && scale.is_finite()) {
        return Err(KitError::NonFinite { what: "a placement" });
    }
    let y = unit(dir);
    if y == DVec3::ZERO {
        return Err(KitError::Degenerate { what: "a placement direction" });
    }
    let reference = if y.x.abs() < 0.9 { DVec3::X } else { DVec3::Z };
    let x = unit(reference + y * -reference.dot(y));
    let z = x.cross(y);
    let mut m = DMat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), DVec4::W);
    // Exact comparisons: the prototype skips the identity turn and scale (`if (spin)`, `if (scale !== 1)`).
    #[allow(clippy::float_cmp)]
    if spin != 0.0 {
        m *= DMat4::from_rotation_y(spin);
    }
    #[allow(clippy::float_cmp)]
    if scale != 1.0 {
        m *= DMat4::from_scale(DVec3::splat(scale));
    }
    m.w_axis = pos.extend(1.0);
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A helix of `count` points about the Z axis, `radius` out and `rise` up per point.
    fn helix(count: usize, radius: f64, turn: f64, rise: f64) -> Vec<DVec3> {
        (0..count)
            .map(|i| {
                let a = turn * fraction(i, 1);
                DVec3::new(radius * a.cos(), radius * a.sin(), rise * fraction(i, 1))
            })
            .collect()
    }

    fn angle(a: DVec3, b: DVec3) -> f64 {
        a.cross(b).length().atan2(a.dot(b)).to_degrees()
    }

    #[test]
    fn frames_are_orthonormal_and_the_normal_starts_square_to_up() {
        let pts = [DVec3::ZERO, DVec3::new(1.0, 0.0, 0.5), DVec3::new(2.0, 1.0, 0.5), DVec3::new(2.0, 2.0, 1.5)];
        let f = frames(&pts, DEFAULT_UP).unwrap();
        for frame in &f {
            assert!((frame.t.length() - 1.0).abs() < 1e-12 && (frame.n.length() - 1.0).abs() < 1e-12);
            assert!((frame.b.length() - 1.0).abs() < 1e-12);
            assert!(frame.t.dot(frame.n).abs() < 1e-12 && frame.t.dot(frame.b).abs() < 1e-12);
            assert!(frame.n.dot(frame.b).abs() < 1e-12);
            assert!((frame.t.cross(frame.n) - frame.b).length() < 1e-12, "right-handed");
        }
        assert!((f[0].t - DVec3::new(1.0, 0.0, 0.5).normalize()).length() < 1e-12, "forward difference at the start");
        let up_in_plane = DEFAULT_UP - f[0].t * f[0].t.z;
        assert!(angle(f[0].n, up_in_plane) < 1e-9, "the first normal is up with the tangent cut out");
    }

    #[test]
    fn up_along_the_path_falls_back_to_x_and_then_to_y() {
        let upward = [DVec3::ZERO, DVec3::new(0.0, 0.0, 1.0), DVec3::new(0.0, 0.0, 2.0)];
        assert_eq!(frames(&upward, DEFAULT_UP).unwrap()[0].n, DVec3::X);
        let along_x = [DVec3::ZERO, DVec3::X, DVec3::new(2.0, 0.0, 0.0)];
        assert_eq!(
            frames(&along_x, DVec3::X).unwrap()[0].n,
            DVec3::Y,
            "up and the +X fallback both run along the path"
        );
        assert_eq!(frames(&along_x, DVec3::ZERO).unwrap()[0].n, DVec3::Y, "a zero up is parallel to everything");
    }

    #[test]
    fn a_helix_keeps_its_frame_turning_smoothly() {
        // 3 degrees round the axis per point: a frame that keeps up with the path turns by no more than that.
        let pts = helix(400, 0.1, 0.05, 0.0005);
        let f = frames(&pts, DEFAULT_UP).unwrap();
        for pair in f.windows(2) {
            assert!(
                angle(pair[0].n, pair[1].n) < 5.0,
                "consecutive normals differ by {} degrees",
                angle(pair[0].n, pair[1].n)
            );
        }
    }

    #[test]
    fn a_path_that_turns_back_along_its_normal_cuts_a_new_one_instead_of_collapsing() {
        // The first normal is +Z; the tangent at the second point is +Z too, so nothing of the normal is left to carry.
        let pts = [DVec3::ZERO, DVec3::X, DVec3::new(0.0, 0.0, 2.0), DVec3::new(0.0, 0.0, 3.0)];
        let f = frames(&pts, DEFAULT_UP).unwrap();
        assert_eq!(f[0].n, DVec3::Z);
        assert!((f[1].n - DVec3::X).length() < 1e-12, "cut afresh from up, which is along the tangent, so +X");
        for frame in &f {
            assert!((frame.n.length() - 1.0).abs() < 1e-12 && frame.n.dot(frame.t).abs() < 1e-12);
            assert!((frame.b.length() - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn bad_paths_are_typed_errors() {
        assert!(matches!(frames(&[], DEFAULT_UP), Err(KitError::TooFew { need: 2, got: 0, .. })));
        assert!(matches!(frames(&[DVec3::X], DEFAULT_UP), Err(KitError::TooFew { need: 2, got: 1, .. })));
        let nan = DVec3::new(f64::NAN, 0.0, 0.0);
        assert!(matches!(frames(&[DVec3::ZERO, nan], DEFAULT_UP), Err(KitError::NonFinite { .. })));
        assert!(matches!(frames(&[DVec3::ZERO, DVec3::X], nan), Err(KitError::NonFinite { .. })));
        assert!(
            matches!(frames(&[DVec3::X, DVec3::X], DEFAULT_UP), Err(KitError::Degenerate { .. })),
            "coincident ends"
        );
        let doubled_back = [DVec3::ZERO, DVec3::X, DVec3::ZERO];
        assert!(
            matches!(frames(&doubled_back, DEFAULT_UP), Err(KitError::Degenerate { .. })),
            "the middle tangent vanishes"
        );
        let huge = [DVec3::splat(-1e308), DVec3::splat(1e308)];
        assert!(
            matches!(frames(&huge, DEFAULT_UP), Err(KitError::Degenerate { .. })),
            "a tangent whose length overflows"
        );
    }

    #[test]
    fn a_round_sweep_is_a_ring_of_the_radius_round_each_point() {
        let pts = [DVec3::ZERO, DVec3::new(0.0, 0.0, 1.0), DVec3::new(0.0, 0.0, 2.0)];
        let s = sweep(&pts, |_| (0.25, 0.25), 8, &SweepOptions { up: DVec3::X, ..SweepOptions::default() }).unwrap();
        assert_eq!((s.grid.rows(), s.grid.cols()), (3, 9));
        assert_eq!(s.centres, pts);
        for i in 0..3 {
            for j in 0..9 {
                let p = s.grid.at(i, j);
                assert!((p.z - fraction(i, 1)).abs() < 1e-12, "the ring lies across the path");
                assert!((p.truncate().length() - 0.25).abs() < 1e-12, "at the radius");
            }
            assert!((s.grid.at(i, 8) - s.grid.at(i, 0)).length() < 1e-12, "and closes on itself");
        }
        assert!((s.grid.at(0, 0) - DVec3::new(0.25, 0.0, 0.0)).length() < 1e-12, "starting along the normal");
    }

    #[test]
    fn the_two_semi_axes_shape_and_twist_apply_per_ring() {
        let pts = [DVec3::ZERO, DVec3::new(0.0, 0.0, 1.0)];
        let shape = |t: f64, _: f64| (2.0 - t, 1.0);
        let twist = |t: f64| t * std::f64::consts::FRAC_PI_2;
        let options = SweepOptions { up: DVec3::X, shape: Some(&shape), twist: Some(&twist) };
        let s = sweep(&pts, |_| (0.5, 0.1), 4, &options).unwrap();
        // Ring 0: semi-axes 0.5 along X and 0.1 along Y, doubled along the normal; no twist. Ring 1: shape (1, 1) and a
        // quarter turn, so the start (angle 0) points along the binormal.
        assert!((s.grid.at(0, 0) - DVec3::new(1.0, 0.0, 0.0)).length() < 1e-12);
        assert!((s.grid.at(0, 1) - DVec3::new(0.0, 0.1, 0.0)).length() < 1e-12);
        assert!((s.grid.at(1, 0) - DVec3::new(0.0, 0.1, 1.0)).length() < 1e-12);
    }

    #[test]
    fn a_sweep_rejects_bad_sizes_radii_and_paths() {
        let pts = [DVec3::ZERO, DVec3::X];
        let round = |_: f64| (0.1, 0.1);
        assert!(matches!(
            sweep(&pts, round, 0, &SweepOptions::default()),
            Err(KitError::TooFew { need: 1, got: 0, .. })
        ));
        assert!(matches!(sweep(&pts, round, usize::MAX, &SweepOptions::default()), Err(KitError::Overflow { .. })));
        assert!(matches!(sweep(&pts[..1], round, 8, &SweepOptions::default()), Err(KitError::TooFew { .. })));
        assert!(matches!(
            sweep(&pts, |_| (f64::NAN, 0.1), 8, &SweepOptions::default()),
            Err(KitError::NonFinite { .. })
        ));
        let twist = |_: f64| f64::INFINITY;
        let options = SweepOptions { twist: Some(&twist), ..SweepOptions::default() };
        assert!(matches!(sweep(&pts, round, 8, &options), Err(KitError::NonFinite { .. })));
    }

    #[test]
    fn a_tube_tapers_to_a_hundredth_of_its_radius_at_both_ends() {
        let pts = helix(40, 0.1, 0.2, 0.01);
        let s = tube(&pts, |_| 0.01, 6).unwrap();
        let ring_radius = |i: usize| (s.grid.at(i, 0) - s.centres[i]).length();
        assert!((ring_radius(0) - 0.0001).abs() < 1e-12 && (ring_radius(39) - 0.0001).abs() < 1e-12);
        assert!((ring_radius(20) - 0.01).abs() < 1e-12, "full radius in the middle");
        assert!(ring_radius(1) > ring_radius(0), "growing away from the end");
    }

    #[test]
    fn place_maps_local_y_to_the_direction_and_the_origin_to_the_position() {
        let pos = DVec3::new(0.1, -0.2, 0.3);
        let dir = DVec3::new(0.3, 0.8, 0.5);
        let m = place(pos, dir, 0.0, 1.0).unwrap();
        assert!((m.transform_point3(DVec3::ZERO) - pos).length() < 1e-12);
        assert!((m.transform_vector3(DVec3::Y) - dir.normalize()).length() < 1e-12);
        assert!((m.x_axis.truncate().dot(m.z_axis.truncate())).abs() < 1e-12 && m.determinant() > 0.0, "a rotation");
        let scaled = place(pos, dir, 0.0, 2.0).unwrap();
        assert!((scaled.transform_vector3(DVec3::Y) - dir.normalize() * 2.0).length() < 1e-12);
    }

    #[test]
    fn spin_turns_about_the_direction_and_a_near_x_direction_uses_z_as_its_reference() {
        let plain = place(DVec3::ZERO, DVec3::Y, 0.0, 1.0).unwrap();
        assert_eq!((plain.x_axis.truncate(), plain.z_axis.truncate()), (DVec3::X, DVec3::Z));
        let spun = place(DVec3::ZERO, DVec3::Y, std::f64::consts::FRAC_PI_2, 1.0).unwrap();
        assert!(
            (spun.transform_vector3(DVec3::X) - DVec3::NEG_Z).length() < 1e-12,
            "a quarter turn about +Y takes +X to -Z"
        );
        assert!((spun.transform_vector3(DVec3::Y) - DVec3::Y).length() < 1e-12);
        let sideways = place(DVec3::ZERO, DVec3::X, 0.0, 1.0).unwrap();
        assert!((sideways.transform_vector3(DVec3::Y) - DVec3::X).length() < 1e-12);
        assert!((sideways.x_axis.truncate() - DVec3::Z).length() < 1e-12, "the reference is +Z, already square to +X");
    }

    #[test]
    fn place_rejects_a_zero_direction_and_non_finite_inputs() {
        assert!(matches!(place(DVec3::ZERO, DVec3::ZERO, 0.0, 1.0), Err(KitError::Degenerate { .. })));
        assert!(matches!(
            place(DVec3::ZERO, DVec3::new(f64::NAN, 1.0, 0.0), 0.0, 1.0),
            Err(KitError::NonFinite { .. })
        ));
        assert!(matches!(place(DVec3::splat(f64::INFINITY), DVec3::Y, 0.0, 1.0), Err(KitError::NonFinite { .. })));
        assert!(matches!(place(DVec3::ZERO, DVec3::Y, f64::NAN, 1.0), Err(KitError::NonFinite { .. })));
        assert!(matches!(place(DVec3::ZERO, DVec3::Y, 0.0, f64::INFINITY), Err(KitError::NonFinite { .. })));
    }
}
