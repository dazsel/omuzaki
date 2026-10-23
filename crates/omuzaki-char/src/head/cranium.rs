//! The head cranium and the analytic head surface (port of `headRadius`, `headSurface` and the traced blend in the
//! prototype's `head.js`, lines 17 to 48).
//!
//! The cranium is an ellipsoid whose lower front is longer than its back (the chin side), with fuller cheeks and a
//! slightly flattened face plane for the painted features. Everything here works in the head-local frame: the origin is
//! the head centre, so moving the head changes no radius.
//!
//! [`surface`] is the cranium *before* the face-shape outline and the chin stretch ([`super::outline`]) reshape the
//! lower face, exactly as the prototype's `headSurface` is. The head part publishes the head *as built* instead
//! ([`super::mesh::build_head`]), so hair, hats and ears sit on the reshaped face rather than float off it.

use std::fmt::Debug;
use std::sync::Arc;

use omuzaki_char_api::{Cranium, HeadParams, HeadSurface};
use omuzaki_core::glam::{DVec3, Vec3};

use crate::kit::curve::{lerp, sstep};

/// A radius map traced from a reference (CB-11) that is blended into the analytic cranium where it has data.
/// Implemented by the traced head (`trace::sample`); the analytic head needs nothing but this trait.
pub trait TraceSample: Debug + Send + Sync {
    /// The traced radius in metres and its weight in `0..=1` along the unit direction `dir` from the head centre. The
    /// weight is 0 where the trace had no data, so the analytic cranium shows through there.
    fn sample(&self, dir: Vec3) -> (f32, f32);
}

/// A radius map and how much of it replaces the analytic cranium.
#[derive(Clone, Copy)]
pub(crate) struct Trace<'a> {
    pub(crate) map: &'a dyn TraceSample,
    /// `TracedHead::mix`, `0..=1`.
    pub(crate) mix: f64,
}

impl<'a> Trace<'a> {
    /// The map of `params`, mixed as `params.traced` says.
    pub(crate) fn of(map: &'a dyn TraceSample, params: &HeadParams) -> Self {
        Self { map, mix: mix_of(params) }
    }

    /// `analytic` pulled toward the traced radius by its weight times the mix (the prototype skips the lookup when the
    /// mix is 0).
    fn blend(&self, analytic: f64, dir: DVec3) -> f64 {
        if self.mix <= 0.0 {
            return analytic;
        }
        let (traced, weight) = self.map.sample(dir.as_vec3());
        lerp(analytic, f64::from(traced), f64::from(weight) * self.mix)
    }
}

/// How much of the trace to use: `params.traced`'s mix, or all of it when the caller handed over a map without one.
fn mix_of(params: &HeadParams) -> f64 {
    params.traced.as_ref().map_or(1.0, |t| t.mix)
}

/// The analytic cranium's radius along the unit direction `dir` from the head centre.
///
/// `dir` must be a unit vector (the prototype's `headRadius(d)`); [`surface`] normalises for callers that cannot
/// promise it. The cranium must be valid ([`omuzaki_face_api::Validate`]): a zero half-extent has no radius.
pub fn radius(c: &Cranium, dir: DVec3) -> f64 {
    let DVec3 { x, y, z } = dir;
    let mut rx = c.half_width;
    let rz = if z > 0.0 { c.half_depth_front } else { c.half_depth_back };
    // The lower half drops lower in front (the chin side) than at the back of the skull, where it runs into the neck.
    let ry = if y >= 0.0 {
        c.half_height_up
    } else {
        lerp(c.half_height_down_back, c.half_height_down_front, sstep(-0.35, 0.55, z))
    };
    // Soft chibi cheeks: fuller at the lower front sides, fading out again toward the chin and the throat.
    rx += c.cheek_fullness * sstep(-0.05, 0.6, z) * sstep(0.12, 0.5, -y) * (1.0 - sstep(0.72, 0.97, -y));
    let inverse_square = (x * x) / (rx * rx) + (y * y) / (ry * ry) + (z * z) / (rz * rz);
    let r = 1.0 / inverse_square.sqrt();
    // A calm, slightly flatter face plane for the painted features.
    r * (1.0 - c.face_flatten * sstep(0.8, 1.0, z) * (1.0 - sstep(0.35, 0.65, y.abs())))
}

/// [`radius`] with the optional traced map blended in.
pub(crate) fn radius_traced(c: &Cranium, dir: DVec3, trace: Option<Trace<'_>>) -> f64 {
    let analytic = radius(c, dir);
    trace.map_or(analytic, |t| t.blend(analytic, dir))
}

/// The cranium as a head surface (CB-10, CB-11): its radius, with the traced map blended in when there is one, *not*
/// reshaped by the face-shape outline or the chin stretch, so changing the face shape never moves it. The prototype's
/// hair and ears anchored here; the head part now publishes the built head ([`super::mesh::build_head`]), whose side
/// the reshaped lower face pulls up to about 1 cm inside this one.
///
/// Takes the map by `Arc` because the surface outlives the build (hair and accessories keep it); the mix comes from
/// `params.traced` (1 without one). `params` must be valid.
pub fn surface(params: &HeadParams, trace: Option<Arc<dyn TraceSample>>) -> Arc<dyn HeadSurface> {
    Arc::new(Cranial::new(params, trace))
}

/// The cranium, with its traced map, as a radius along any direction from the head centre.
#[derive(Debug)]
pub(crate) struct Cranial {
    cranium: Cranium,
    centre: Vec3,
    trace: Option<(Arc<dyn TraceSample>, f64)>,
}

impl Cranial {
    /// The cranium of `params`, with `trace` mixed in as `params.traced` says.
    pub(crate) fn new(params: &HeadParams, trace: Option<Arc<dyn TraceSample>>) -> Self {
        let mix = mix_of(params);
        Self {
            cranium: params.cranium.clone(),
            centre: DVec3::from_array(params.cranium.centre()).as_vec3(),
            trace: trace.map(|map| (map, mix)),
        }
    }

    /// The radius along the unit direction `dir`, head-local, in `f64`.
    pub(crate) fn radius_at(&self, dir: DVec3) -> f64 {
        let trace = self.trace.as_ref().map(|(map, mix)| Trace { map: map.as_ref(), mix: *mix });
        radius_traced(&self.cranium, dir, trace)
    }

    /// The head centre, bind space.
    pub(crate) fn centre(&self) -> Vec3 {
        self.centre
    }
}

/// `dir` as a unit vector in `f64`: a zero or non-finite direction has no radius, so it answers for the front rather
/// than hand back NaN.
pub(crate) fn unit(dir: Vec3) -> DVec3 {
    let d = dir.as_dvec3();
    let length = d.length();
    if length > 0.0 && length.is_finite() { d / length } else { DVec3::Z }
}

impl HeadSurface for Cranial {
    fn centre(&self) -> Vec3 {
        self.centre
    }

    #[allow(clippy::cast_possible_truncation)] // the trait speaks f32; the radius is a fraction of a metre
    fn radius(&self, dir: Vec3) -> f32 {
        self.radius_at(unit(dir)) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A radius map that is the same everywhere.
    #[derive(Debug)]
    struct Constant(f32, f32);

    impl TraceSample for Constant {
        fn sample(&self, _: Vec3) -> (f32, f32) {
            (self.0, self.1)
        }
    }

    fn at(c: &Cranium, x: f64, y: f64, z: f64) -> f64 {
        radius(c, DVec3::new(x, y, z).normalize())
    }

    #[test]
    fn the_axes_land_on_the_half_extents() {
        let c = Cranium { cheek_fullness: 0.0, face_flatten: 0.0, ..Cranium::default() };
        assert!((at(&c, 1.0, 0.0, 0.0) - c.half_width).abs() < 1e-12);
        assert!((at(&c, 0.0, 1.0, 0.0) - c.half_height_up).abs() < 1e-12);
        assert!((at(&c, 0.0, 0.0, 1.0) - c.half_depth_front).abs() < 1e-12);
        assert!((at(&c, 0.0, 0.0, -1.0) - c.half_depth_back).abs() < 1e-12);
    }

    #[test]
    fn the_underside_is_longer_in_front_than_behind() {
        let c = Cranium { cheek_fullness: 0.0, face_flatten: 0.0, ..Cranium::default() };
        let back = at(&c, 0.0, -1.0, -1.0);
        let front = at(&c, 0.0, -1.0, 1.0);
        assert!(front > back, "the chin side drops lower: {front} against {back}");
        // At the far ends of the blend the lower half reaches the configured depths (the ellipse through them).
        assert!((at(&c, 0.0, -1e-9, -1.0) - c.half_depth_back).abs() < 1e-6);
    }

    #[test]
    fn cheeks_are_fuller_and_the_face_plane_is_flatter_only_where_asked() {
        let plain = Cranium { cheek_fullness: 0.0, face_flatten: 0.0, ..Cranium::default() };
        let full = Cranium { cheek_fullness: 0.03, ..plain.clone() };
        let flat = Cranium { face_flatten: 0.1, ..plain.clone() };
        let cheek = DVec3::new(0.6, -0.5, 0.62).normalize();
        assert!(radius(&full, cheek) > radius(&plain, cheek) + 0.005, "cheeks bulge at the lower front sides");
        assert!((radius(&full, DVec3::Y) - radius(&plain, DVec3::Y)).abs() < 1e-12, "and not at the crown");
        assert!(radius(&flat, DVec3::Z) < radius(&plain, DVec3::Z) * 0.91, "the face plane flattens by its factor");
        assert!((radius(&flat, DVec3::Y) - radius(&plain, DVec3::Y)).abs() < 1e-12, "away from the face it does not");
    }

    #[test]
    fn the_trace_replaces_the_cranium_by_its_weight_times_the_mix() {
        let params = HeadParams::default();
        let analytic = surface(&params, None);
        let dir = Vec3::new(0.3, 0.2, 0.9).normalize();
        let base = analytic.radius(dir);
        let blended = |weight: f32, mix: f64| {
            let traced = Some(omuzaki_char_api::TracedHead {
                asset: omuzaki_store_api::ContentHash::of(b"unused"),
                theta_steps: 64,
                phi_steps: 128,
                mix,
            });
            let params = HeadParams { traced, ..HeadParams::default() };
            surface(&params, Some(Arc::new(Constant(0.25, weight)))).radius(dir)
        };
        assert!((blended(1.0, 1.0) - 0.25).abs() < 1e-6, "full weight and mix is the trace");
        assert!((blended(0.0, 1.0) - base).abs() < 1e-7, "no data keeps the cranium");
        assert!((blended(1.0, 0.0) - base).abs() < 1e-7, "mix 0 keeps the cranium");
        assert!((blended(0.5, 0.5) - (base + (0.25 - base) * 0.25)).abs() < 1e-6, "weight times mix");
    }

    #[test]
    fn the_surface_is_centred_normalises_its_direction_and_survives_a_zero_one() {
        let params = HeadParams::default();
        let s = surface(&params, None);
        assert_eq!(s.centre(), Vec3::new(0.0, 0.78, -0.02));
        let unit = Vec3::new(0.2, -0.4, 0.8).normalize();
        assert!((s.radius(unit * 7.0) - s.radius(unit)).abs() < 1e-7, "a longer direction means the same one");
        for bad in [Vec3::ZERO, Vec3::NAN, Vec3::INFINITY] {
            assert!(s.radius(bad).is_finite(), "{bad:?}");
        }
        let top = s.point(Vec3::Y, 0.01);
        assert!((top.y - (0.78 + 0.186 + 0.01)).abs() < 1e-6, "point() pushes outward from the centre");
    }
}
