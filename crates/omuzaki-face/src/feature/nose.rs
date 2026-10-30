//! The nose, sculpted as a signed-distance field on a patch of face: the bony bridge running down from between the
//! eyes, the sidewalls falling to the cheeks, the tip's two domes (a soft double highlight), the alae (the wings round
//! the nostrils) with the crease where they meet the tip and the cheek, the columella between the nostrils, and the
//! nostrils themselves cut up into the base. Seen from below the nostrils open as two teardrops; from the front only
//! their rims show.
//!
//! Toward anime the same field morphs into a drawn nose: a soft low bridge and a small rounded point, no wings and no
//! nostrils, the way a cel-shaded face reads a nose from a highlight and one small shadow.
//!
//! Frame: x across the face (to the person's left), y up, z out of the face; metres. The face's front plane passes
//! through the origin near the base of the nose.

use omuzaki_core::glam::Vec3;
pub use omuzaki_face_api::sculpt::NoseDesign;

use super::sdf::{carve, ellipsoid, ellipsoid_turned, frame, round_cone, smax, smin, smoothstep, tube};

const MM: f32 = 0.001;

/// Where the nostrils open, as a field for the cavity term: negative inside a nostril.
pub fn nostril_depth(d: &NoseDesign, p: Vec3) -> f32 {
    let s = Shape::new(d);
    s.nostrils(p)
}

/// The nose's field, the face patch it sits on included.
pub fn field(d: &NoseDesign) -> impl Fn(Vec3) -> f32 + '_ {
    let shape = Shape::new(d);
    move |p| shape.distance(p)
}

/// The box the nose and its patch of face fill.
pub fn bounds(d: &NoseDesign) -> (Vec3, Vec3) {
    let half = (0.6 * d.width).max(0.026);
    (Vec3::new(-half, -0.034, -0.012), Vec3::new(half, 0.034, d.projection + 0.004))
}

/// How much of the surface at `p` is the nose's own skin (0 the face round it, 1 the tip and wings), for the flush.
pub fn redness(d: &NoseDesign, p: Vec3) -> f32 {
    let s = Shape::new(d);
    let tip = 1.0 - smoothstep(-2.0 * MM, 9.0 * MM, (p - s.tip_centre).length() - s.tip);
    let wings = [s.ala_centre, s.ala_centre * Vec3::new(-1.0, 1.0, 1.0)]
        .map(|c| 1.0 - smoothstep(-2.0 * MM, 8.0 * MM, (p - c).length() - 3.0 * MM));
    tip.max(wings[0]).max(wings[1]) * (1.0 - d.style)
}

/// The placed forms of one design.
struct Shape {
    style: f32,
    nasion: Vec3,
    supratip: Vec3,
    bridge_r: (f32, f32),
    hump: f32,
    tip_centre: Vec3,
    tip: f32,
    dome_gap: f32,
    columella: (Vec3, Vec3),
    ala_centre: Vec3,
    wing: [Vec3; 3],
    wing_r: [f32; 3],
    base_centre: Vec3,
    base_radii: Vec3,
    lower_centre: Vec3,
    lower_radii: Vec3,
    nostril_centre: Vec3,
    nostril_radii: Vec3,
    anime_tip: Vec3,
    anime_r: f32,
    anime_top: Vec3,
    anime_base: Vec3,
}

impl Shape {
    fn new(d: &NoseDesign) -> Self {
        let up = d.upturn.to_radians();
        let top = 0.52 * d.length;
        let bottom = top - d.length;
        // The tip sits a little above the base, turned up about the base by `upturn`.
        let tip_y = bottom + 8.5 * MM + (d.projection - 4.0 * MM) * up.sin();
        let tip_centre = Vec3::new(0.0, tip_y, d.projection - d.tip);
        let half_width = 0.5 * d.width;
        let flare = d.flare.max(0.2);
        Self {
            style: d.style.clamp(0.0, 1.0),
            nasion: Vec3::new(0.0, top, d.bridge - 4.6 * MM),
            supratip: Vec3::new(0.0, tip_y + 2.5 * MM, d.projection - d.tip - 3.2 * MM),
            bridge_r: (9.0 * MM, 13.0 * MM),
            hump: d.hump,
            tip_centre,
            tip: d.tip,
            dome_gap: d.bifid.clamp(0.0, 1.0) * 0.72 * d.tip,
            columella: (
                Vec3::new(0.0, tip_y - 0.55 * d.tip, d.projection - 1.6 * d.tip),
                Vec3::new(0.0, bottom + 0.6 * MM, 4.5 * MM),
            ),
            ala_centre: Vec3::new(half_width - 4.2 * MM, bottom + 4.4 * MM, 0.4 * d.projection),
            wing: [
                Vec3::new(half_width - 4.0 * MM * flare, bottom + 3.9 * MM, 0.2 * d.projection),
                Vec3::new(half_width - 4.0 * MM * flare, bottom + 4.6 * MM, 0.42 * d.projection),
                Vec3::new(half_width - 8.6 * MM, bottom + 5.2 * MM, 0.64 * d.projection),
            ],
            wing_r: [4.0 * MM * flare, 3.9 * MM * flare, 3.5 * MM * flare],
            base_centre: Vec3::new(0.0, bottom + 3.6 * MM, 0.5 * d.projection),
            base_radii: Vec3::new(half_width - 6.5 * MM, 4.6 * MM, 0.4 * d.projection),
            lower_centre: Vec3::new(0.0, bottom + 9.0 * MM, 0.3 * d.projection),
            lower_radii: Vec3::new(half_width - 4.5 * MM, 7.5 * MM, 0.48 * d.projection),
            nostril_centre: Vec3::new(5.2 * MM, bottom - 0.9 * MM, 0.48 * d.projection),
            nostril_radii: Vec3::new(2.9 * MM, 2.6 * MM, 0.29 * d.projection) * d.nostrils.max(0.1),
            anime_tip: Vec3::new(0.0, tip_y + 1.0 * MM, d.projection - 0.6 * d.tip),
            anime_r: 0.6 * d.tip,
            anime_top: Vec3::new(0.0, top - 4.0 * MM, d.bridge - 7.5 * MM),
            anime_base: Vec3::new(0.0, tip_y - 3.0 * MM, -4.0 * MM),
        }
    }

    /// The face's front: a broad dome falling back toward the cheeks and the brow.
    fn face(p: Vec3) -> f32 {
        ellipsoid(p, Vec3::new(0.0, 0.004, -0.068), Vec3::new(0.062, 0.1, 0.068))
    }

    /// Both nostrils' cavities.
    fn nostrils(&self, p: Vec3) -> f32 {
        let mirrored = Vec3::new(p.x.abs(), p.y, p.z);
        // Teardrops pointing forward to the tip and a little in, tilted up into the nose.
        let axes = frame(Vec3::new(0.0, 1.0, 0.25), Vec3::new(-0.22, 0.32, 1.0));
        ellipsoid_turned(mirrored, self.nostril_centre, axes, self.nostril_radii)
    }

    fn real(&self, p: Vec3) -> f32 {
        let face = Self::face(p);
        // The body: a cone with an elliptical section, narrow across and deep, from between the eyes to above the tip.
        // Its top is the bridge; its sides sink into the face, which the blend turns into the slope down to the
        // cheeks. The hump lifts the bridge two thirds of the way down; below the supratip it gives way to the tip.
        let t = ((self.nasion.y - p.y) / (self.nasion.y - self.supratip.y)).clamp(0.0, 1.0);
        let bump = self.hump * (std::f32::consts::PI * (t / 0.66).min(1.0)).sin().max(0.0);
        let (ra, rb) = self.bridge_r;
        let body_p = Vec3::new(p.x / 0.62, p.y, p.z - bump);
        let top = Vec3::new(0.0, self.nasion.y, self.nasion.z + self.bridge_r.0 * 0.0 - ra + 4.4 * MM);
        let low = Vec3::new(0.0, self.supratip.y, self.supratip.z + 5.2 * MM - rb);
        let body = round_cone(body_p, top, low, ra, rb);
        let body = smax(body, self.supratip.y - 3.0 * MM - p.y, 5.0 * MM);
        // The tip's two domes.
        let dome = |side: f32| {
            ellipsoid(
                p,
                self.tip_centre + Vec3::new(side * self.dome_gap * 0.5, 0.0, 0.0),
                Vec3::new(self.tip * 0.95, self.tip, self.tip),
            )
        };
        let tip = smin(dome(1.0), dome(-1.0), 1.8 * MM);
        // The base: the mass the nostrils open into, from the columella out to the wings.
        let base = ellipsoid(p, self.base_centre, self.base_radii);
        let columella_p = Vec3::new(p.x / 0.8, p.y, p.z);
        let columella = round_cone(columella_p, self.columella.0, self.columella.1, 3.0 * MM, 2.8 * MM);
        // The wings: thick crescents from where they join the cheek, forward and in to the tip's sides, joined to the
        // base under a crease.
        let mirrored = Vec3::new(p.x.abs(), p.y, p.z);
        let wing = tube(mirrored, &self.wing, &self.wing_r, 1.5 * MM);
        // The lower sidewalls: the skin over the wings' tops between the bridge and the tip, so the wings meet the
        // nose in a shallow crease rather than under an overhang.
        let lower = ellipsoid(p, self.lower_centre, self.lower_radii);
        let mut nose = smin(body, tip, 3.5 * MM);
        nose = smin(nose, lower, 3.5 * MM);
        nose = smin(nose, base, 3.0 * MM);
        nose = smin(nose, columella, 1.6 * MM);
        // A tight blend: the wings keep the alar crease against the tip lobule.
        nose = smin(nose, wing, 1.6 * MM);
        let d = smin(face, nose, 5.0 * MM);
        carve(d, self.nostrils(p), 0.7 * MM)
    }

    fn anime(&self, p: Vec3) -> f32 {
        let face = Self::face(p);
        // A narrow wedge: the bridge sloping straight down from between the eyes to a small rounded point, and the
        // underside running back from the point into the face. Both sides sink into the face, which the blend
        // smooths, so from the front the nose is only a soft highlight and a small shadow under the point.
        let q = Vec3::new(p.x / 0.8, p.y, p.z);
        let bridge = round_cone(q, self.anime_top, self.anime_tip, 6.0 * MM, self.anime_r);
        let under = round_cone(q, self.anime_tip, self.anime_base, self.anime_r, 5.0 * MM);
        smin(face, smin(bridge, under, 3.0 * MM), 6.0 * MM)
    }

    fn distance(&self, p: Vec3) -> f32 {
        if self.style <= 0.0 {
            return self.real(p);
        }
        if self.style >= 1.0 {
            return self.anime(p);
        }
        self.real(p) + (self.anime(p) - self.real(p)) * self.style
    }
}
