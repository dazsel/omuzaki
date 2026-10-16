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
