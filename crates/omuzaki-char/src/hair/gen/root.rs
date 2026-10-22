//! What the tail and bun generators share: a root on the head surface, and keeping hair off the head.
//!
//! **The root.** A `[theta_deg, phi_deg, offset]` triple is the contract's surface point ([`GuidePoint::Surface`]).
//! The generators need the point and the direction the head surface faces there (`outward`, from the head centre), and
//! they read the triple as typed, so [`Root::new`] clamps it to the contract's ranges and replaces a non-finite value
//! with the default's: a slider can never change a clump count or make a rest point non-finite.
//!
//! **Clearance.** [`Root::keep_outside`] pushes a point radially out to [`CLEARANCE`] above the head. The tail
//! and the bun are curved lines that start on the head and leave it, so only their first points ever need it.

use omuzaki_char_api::HeadSurface;
use omuzaki_core::glam::DVec3;

/// How far every rest point of a tail or bun stands off the head, metres. The acceptance asks for 6 mm; the extra
/// leaves room for the tube's own half thickness.
pub const CLEARANCE: f64 = 0.010;

/// A root on the head surface.
#[derive(Clone, Copy, Debug)]
pub struct Root {
    /// The point, bind space.
    pub point: DVec3,
    /// The unit direction from the head centre to the point.
    pub outward: DVec3,
}

impl Root {
    /// The root at `spec = [theta_deg, phi_deg, offset]` on `head`; a coordinate that is not finite takes the matching
    /// one of `fallback`, then every coordinate is limited to the contract's range.
    pub fn new(spec: [f64; 3], fallback: [f64; 3], head: &dyn HeadSurface) -> Self {
        let pick = |i: usize, lo: f64, hi: f64| {
            let v = if spec[i].is_finite() { spec[i] } else { fallback[i] };
            v.clamp(lo, hi)
        };
        let (theta, phi) = (pick(0, 0.0, 180.0).to_radians(), pick(1, -360.0, 360.0).to_radians());
        let offset = pick(2, -0.05, 0.3);
        let outward = DVec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos());
        let centre = head.centre().as_dvec3();
        Self { point: centre + outward * (f64::from(head.radius(outward.as_vec3())) + offset), outward }
    }

    /// `p` pushed radially out of the head until it is [`CLEARANCE`] above the surface (unchanged if it already is).
    /// A point at the head centre has no direction and goes out along `outward`.
    pub fn keep_outside(&self, head: &dyn HeadSurface, p: DVec3) -> DVec3 {
        let centre = head.centre().as_dvec3();
        let dir = (p - centre).normalize_or(self.outward);
        let least = f64::from(head.radius(dir.as_vec3())) + CLEARANCE;
        if (p - centre).length() < least { centre + dir * least } else { p }
    }
}

/// `x` limited to `lo..=hi`, or `fallback` if it is not finite.
pub fn sane(x: f64, lo: f64, hi: f64, fallback: f64) -> f64 {
    if x.is_finite() { x.clamp(lo, hi) } else { fallback }
}

// ------------------------------------------------------------------------------------------------ gather locks

/// The crown radius of the head the tail and bun lengths are written for (the `standard` template's), metres.
pub const REFERENCE_HEAD_RADIUS: f64 = 0.1;
/// Points per gather lock.
pub const GATHER_POINTS: usize = 16;
/// How far a gather lock rides above the bare head at its start (below the cap's edge), metres.
pub const GATHER_LIFT: f64 = 0.008;
/// How far it rides above the head once it is over the cap's shell (16 mm at the hairline, more toward the crown), metres,
/// so it shows on top of the cap instead of under it.
pub const GATHER_CAP_LIFT: f64 = 0.024;
/// The fraction of a gather lock over which it rises from [`GATHER_LIFT`] to [`GATHER_CAP_LIFT`].
pub const GATHER_CAP_FROM: (f64, f64) = (0.04, 0.2);
/// Extra height of every other gather lock, so neighbours stack instead of fighting, metres.
pub const GATHER_STACK: f64 = 0.0015;
/// Polar angle (degrees from the top) where the gather locks start: the nape, below the cap's back hairline, so they
/// cover the bare band under the cap's edge.
pub const GATHER_START_DEG: f64 = 130.0;
/// How much higher every other gather lock starts, degrees.
pub const GATHER_STAGGER_DEG: f64 = 6.0;
/// The gather's half span round the head, degrees of azimuth either side of the tie's.
pub const GATHER_SPAN_DEG: f64 = 55.0;
/// The fraction of a gather lock after which it rises off the scalp to the tie.
pub const GATHER_RISE_FROM: f64 = 0.7;
/// Shade of the gather locks (under the tail's own).
pub const GATHER_SHADE: f64 = 0.95;
/// The gather's width profile for a head of the reference size: pointed at the nape, broad and flat over the head,
/// narrowing into the tie.
pub const GATHER_WIDTH: omuzaki_char_api::Taper = omuzaki_char_api::Taper {
    start: 0.016,
    mid: 0.050,
    tip: 0.012,
    tip_at: 0.5,
    shape: omuzaki_char_api::TaperShape::Linear,
};
/// The gather's thickness profile.
pub const GATHER_THICKNESS: omuzaki_char_api::Bulge = omuzaki_char_api::Bulge { base: 0.005, swell: 0.002 };

/// The head's size against [`REFERENCE_HEAD_RADIUS`], limited to `0.5..=2`.
pub fn head_scale(head: &dyn HeadSurface) -> f64 {
    (f64::from(head.radius(omuzaki_core::glam::Vec3::Y)) / REFERENCE_HEAD_RADIUS).clamp(0.5, 2.0)
}

/// [`GATHER_WIDTH`] scaled to a head of size `scale`.
pub fn gather_width(scale: f64) -> omuzaki_char_api::Taper {
    let w = GATHER_WIDTH;
    omuzaki_char_api::Taper { start: w.start * scale, mid: w.mid * scale, tip: w.tip * scale, ..w }
}

/// `count` gather locks: each starts at the nape, spread over [`GATHER_SPAN_DEG`] either side of the root's azimuth,
/// follows the head (the great circle to the root's direction) a few millimetres off the bare scalp and
/// [`GATHER_CAP_LIFT`] over the cap, and rises over its last third to end exactly at `end`, the middle of the tie or of the bun's base. Root first.
#[allow(clippy::cast_precision_loss)] // indices are small
pub fn gather_locks(root: &Root, head: &dyn HeadSurface, count: u8, end: DVec3) -> Vec<Vec<omuzaki_core::glam::Vec3>> {
    use omuzaki_core::glam::DQuat;
    let centre = head.centre().as_dvec3();
    let target = root.outward;
    let theta_root = target.y.clamp(-1.0, 1.0).acos().to_degrees();
    let theta_start = GATHER_START_DEG.max(theta_root + 20.0).min(150.0).to_radians();
    let phi_root = target.x.atan2(target.z);
    let end_offset = (end - centre).length() - f64::from(head.radius((end - centre).normalize_or(target).as_vec3()));
    (0..count)
        .map(|i| {
            let u = if count > 1 { 2.0 * f64::from(i) / f64::from(count - 1) - 1.0 } else { 0.0 };
            let phi = phi_root + GATHER_SPAN_DEG.to_radians() * u;
            // Every other lock starts a little higher, so the tips at the nape are ragged rather than a straight edge.
            let theta = theta_start - if i % 2 == 0 { 0.0 } else { GATHER_STAGGER_DEG.to_radians() };
            let start = DVec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos());
            let turn = DQuat::from_rotation_arc(start, target);
            let stack = if i % 2 == 0 { 0.0 } else { GATHER_STACK };
            (0..GATHER_POINTS)
                .map(|k| {
                    let t = k as f64 / (GATHER_POINTS - 1) as f64;
                    if k == GATHER_POINTS - 1 {
                        return end.as_vec3();
                    }
                    let dir = DQuat::IDENTITY.slerp(turn, t) * start;
                    let rise = smoothstep(t, GATHER_RISE_FROM, 1.0);
                    let over = smoothstep(t, GATHER_CAP_FROM.0, GATHER_CAP_FROM.1);
                    let along_scalp = (GATHER_LIFT + stack) * (1.0 - over) + (GATHER_CAP_LIFT + stack) * over;
                    let lift = along_scalp * (1.0 - rise) + end_offset * rise;
                    let p = centre + dir * (f64::from(head.radius(dir.as_vec3())) + lift);
                    root.keep_outside(head, p).as_vec3()
                })
                .collect()
        })
        .collect()
}

/// A smoothstep: 0 at or below `lo`, 1 at or above `hi`.
fn smoothstep(x: f64, lo: f64, hi: f64) -> f64 {
    let u = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}
