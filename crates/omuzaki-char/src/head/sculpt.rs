//! The face's forms sculpted into the head (#2697): the brow ridge, the eye sockets, the cheekbones, the temples, the
//! nose, the muzzle and lips, the chin and the jaw's angle, each a smooth bump of the head's surface along the ray from
//! its centre, as tall as [`Sculpt`] says.
//!
//! Every form is placed from the painted face's layout ([`Landmarks`]), so it sits under the drawn feature on any head
//! and any template, and sized in half-distances between the eyes, so a narrower face gets a narrower nose. Each bump
//! is a product of compact, twice-differentiable kernels, so the forms are smooth, local (a form never moves the back
//! of the head) and cheap to evaluate per vertex. The head mesh is denser at the front when a head is sculpted
//! ([`warp`]), so the nose and the lips have the vertices to show.

#![allow(clippy::many_single_char_names, reason = "geometry maths: x, y, p, n, e, s and a are the forms' names")]

use std::collections::HashMap;

use omuzaki_char_api::{Ears, HeadOutline, Sculpt};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::FaceLayout;

use crate::kit::curve::sstep;

/// Where the face's features are on the head, head-local metres: the painted face's layout, the chin and the ears.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Landmarks {
    /// The eye line's height.
    pub eye_y: f64,
    /// Half the distance between the eye centres: the unit every form is sized in.
    pub eye_spacing: f64,
    /// How far above the eye line the brows are painted.
    pub brow_gap: f64,
    /// The painted nose's height.
    pub nose_y: f64,
    /// The mouth line's height.
    pub mouth_y: f64,
    /// The chin's height.
    pub chin_y: f64,
    /// The left ear's place.
    pub ear: DVec3,
}

impl Landmarks {
    /// The landmarks of a face laid out as `layout` on a head with `outline` and `ears`.
    pub fn new(layout: &FaceLayout, outline: &HeadOutline, ears: &Ears) -> Self {
        Self {
            eye_y: layout.eye_y,
            eye_spacing: layout.eye_spacing.max(0.02),
            brow_gap: layout.brow_gap,
            nose_y: layout.nose_y,
            mouth_y: layout.mouth_y,
            chin_y: outline.chin_y(),
            ear: DVec3::from_array(ears.position),
        }
    }
}

/// A sculpt with its landmarks: the displacement of the head's surface at any point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sculptor {
    sculpt: Sculpt,
    at: Landmarks,
}

/// The compact kernel `(1 - t²)³` for `|t| < 1`, else 0: 1 at the centre, flat and smooth (C²) at its edge.
fn kernel(t: f64) -> f64 {
    let s = 1.0 - t * t;
    if s <= 0.0 { 0.0 } else { s * s * s }
}

/// A bump of half-widths `wx` and `wy` centred at `(cx, cy)` on the face.
fn bump(x: f64, y: f64, cx: f64, cy: f64, wx: f64, wy: f64) -> f64 {
    kernel((x - cx) / wx) * kernel((y - cy) / wy)
}

/// [`bump`] on both sides of the face, at `±cx`.
fn pair(x: f64, y: f64, cx: f64, cy: f64, wx: f64, wy: f64) -> f64 {
    bump(x, y, cx, cy, wx, wy) + bump(x, y, -cx, cy, wx, wy)
}

impl Sculptor {
    /// The sculptor for `sculpt` on a face at `at`, or `None` for a flat sculpt (nothing to move).
    pub fn new(sculpt: &Sculpt, at: Landmarks) -> Option<Self> {
        (!sculpt.is_flat()).then_some(Self { sculpt: *sculpt, at })
    }

    /// How far the surface point `p` (head-local) moves out along the ray from the centre, metres (negative is in).
    pub fn height(&self, p: DVec3) -> f64 {
        let length = p.length();
        if !(length > 0.0 && length.is_finite()) {
            return 0.0;
        }
        let n = p / length;
        let (s, a) = (&self.sculpt, &self.at);
        let (x, y) = (p.x, p.y);
        let e = a.eye_spacing;
        // How much of the face the point is on: the front, the front and sides, and the very front (the nose's line).
        let front = sstep(0.2, 0.7, n.z);
        let sides = sstep(-0.1, 0.45, n.z);
        let midline = sstep(0.55, 0.85, n.z);
        let mut h = 0.0;

        if s.brow > 0.0 {
            let brow_y = a.eye_y + 0.85 * a.brow_gap.max(0.4 * e);
            let ridge = bump(x, y, 0.0, brow_y, 2.0 * e, 0.42 * e);
            let glabella = 0.55 * bump(x, y, 0.0, a.eye_y + 0.35 * e, 0.4 * e, 0.45 * e);
            h += s.brow * (ridge + glabella) * front;
        }
        if s.sockets > 0.0 {
            // Deepest a little inside and above the eye's centre, under the brow, rising to the cheek below.
            let socket = pair(x, y, 0.92 * e, a.eye_y + 0.14 * e, 0.95 * e, 0.56 * e);
            h -= s.sockets * socket * front;
        }
        if s.cheekbones > 0.0 {
            let cheek = pair(x, y, 1.35 * e, a.eye_y - 0.62 * e, 0.9 * e, 0.55 * e);
            h += s.cheekbones * cheek * sides;
        }
        if s.temples > 0.0 {
            let temple = pair(x, y, 1.9 * e, a.eye_y + 0.7 * e, 0.55 * e, 0.65 * e);
            h -= s.temples * temple * sstep(-0.35, 0.2, n.z);
        }
        if s.nose > 0.0 {
            h += s.nose * self.nose(x, y) * midline;
        }
        if s.mouth > 0.0 {
            let muzzle = bump(x, y, 0.0, a.mouth_y + 0.05 * e, 1.25 * e, 0.95 * e);
            let upper = bump(x, y, 0.0, a.mouth_y + 0.09 * e, 0.5 * e, 0.15 * e);
            let lower = bump(x, y, 0.0, a.mouth_y - 0.13 * e, 0.45 * e, 0.16 * e);
            h += s.mouth * (muzzle + 0.25 * upper + 0.3 * lower) * sstep(0.3, 0.8, n.z);
        }
        if s.chin > 0.0 {
            let drop = (a.mouth_y - a.chin_y).max(0.02);
            let chin = bump(x, y, 0.0, a.chin_y + 0.3 * drop, 0.6 * e, 0.34 * drop);
            h += s.chin * chin * front;
        }
        if s.jaw > 0.0 {
            // The jaw's angle: below the ear and a little in front of it, where the jaw turns from the neck to the chin.
            let drop = (a.mouth_y - a.chin_y).max(0.02);
            let gonion = DVec3::new(a.ear.x.abs() * 0.9, a.chin_y + 0.75 * drop, a.ear.z + 0.35 * e);
            let reach = 0.95 * e;
            let side = |sign: f64| kernel((p - DVec3::new(sign * gonion.x, gonion.y, gonion.z)).length() / reach);
            h += s.jaw * (side(1.0) + side(-1.0));
        }
        h
    }

    /// The nose's shape at `(x, y)`, 1 at its tip: a bridge rising from between the eyes to a rounded tip over the
    /// painted nose, the wings either side of it and the drop under it to the lip.
    fn nose(&self, x: f64, y: f64) -> f64 {
        let a = &self.at;
        let e = a.eye_spacing;
        let top = a.eye_y + 0.1 * e;
        let tip = a.nose_y + 0.04 * e;
        let base = a.nose_y - 0.45 * e;
        if y > top + 0.3 * e || y < base {
            return 0.0;
        }
        let span = (top - tip).max(0.01);
        // Down the bridge, 0 at its top and 1 at the tip; below the tip it falls to the lip.
        let t = ((top - y) / span).clamp(0.0, 1.0);
        let rise = 0.1 + 0.9 * t.powf(1.8);
        let fall = if y < tip { 1.0 - sstep(tip, base, y) } else { 1.0 };
        let fade_top = sstep(top + 0.3 * e, top - 0.1 * e, y);
        let width = 0.15 * e + 0.17 * e * t;
        let bridge = rise * fall * fade_top * kernel(x / width);
        let ball = bump(x, y, 0.0, tip - 0.05 * e, 0.27 * e, 0.22 * e);
        let wings = 0.25 * pair(x, y, 0.26 * e, a.nose_y - 0.08 * e, 0.17 * e, 0.15 * e);
        bridge.max(ball) + wings
    }

    /// `p` (head-local) moved by its height along the ray from the centre.
    pub fn apply(&self, p: DVec3) -> DVec3 {
        let length = p.length();
        if length > 0.0 && length.is_finite() { p * (1.0 + self.height(p) / length) } else { p }
    }
}

/// How much a concave curvature of 1/m (real) closes the `skin` program's cavity: a socket bent like a 2 cm ball
/// (50/m) is about two fifths closed.
const CAVITY_PER_CURVATURE: f32 = 0.008;
/// The extra redness the `skin` program adds on the cheeks and on the nose's tip of a sculpted head.
const CHEEK_FLUSH: f64 = 0.35;
const NOSE_FLUSH: f64 = 0.4;

/// The `skin` program's `skin-info` for a sculpted head mesh (bind space, the template's metres, scaled to real ones
/// by `scale`) whose centre is `centre`: dry; the cavity closing where the surface bends in (the sockets, beside the
/// nose, under the lip, the jaw's turn); a warm flush on the cheeks (where `layout` paints the blush) and the nose's tip;
/// and the curvature in 1/m, so the scattering softens the light across tight forms. The grid's seam copies are welded,
/// so the seam down the face's middle reads as one surface.
pub fn skin_info(mesh: &MeshData, centre: Vec3, at: &Landmarks, layout: &FaceLayout, scale: f32) -> Vec<f32> {
    let n = mesh.positions.len();
    // One id per distinct position: the seam's two columns and each pole's ring share one.
    let mut ids = HashMap::with_capacity(n);
    let weld: Vec<usize> = mesh
        .positions
        .iter()
        .enumerate()
        .map(|(i, p)| *ids.entry(p.to_array().map(f32::to_bits)).or_insert(i))
        .collect();
    let mut sums = vec![(DVec3::ZERO, 0_u32); n];
    for tri in mesh.indices.as_chunks::<3>().0 {
        for k in 0..3 {
            let (a, b) = (tri[k] as usize, tri[(k + 1) % 3] as usize);
            let (wa, wb) = (weld[a], weld[b]);
            if wa == wb {
                continue;
            }
            let (pa, pb) = (mesh.positions[wa].as_dvec3(), mesh.positions[wb].as_dvec3());
            sums[wa].0 += pb - pa;
            sums[wa].1 += 1;
            sums[wb].0 += pa - pb;
            sums[wb].1 += 1;
        }
    }
    // The umbrella operator's offset along the normal over the mean squared edge: twice the mean curvature, positive
    // where the surface bulges, in 1/m of the template, then of the character.
    let edges = edge_lengths(mesh, &weld);
    let scale = f64::from(scale.max(1e-3));
    let curvature: Vec<f64> = (0..n)
        .map(|i| {
            let w = weld[i];
            let (sum, count) = sums[w];
            let normal = mesh.normals.get(w).map_or(DVec3::ZERO, |v| v.as_dvec3());
            let squared = edges[w];
            if count == 0 || squared <= 1e-14 {
                return 0.0;
            }
            -2.0 * (sum / f64::from(count)).dot(normal) / squared / scale
        })
        .collect();
    let e = at.eye_spacing;
    let mut info = Vec::with_capacity(n * 4);
    for (i, p) in mesh.positions.iter().enumerate() {
        let local = (*p - centre).as_dvec3();
        let front = sstep(0.2, 0.7, local.normalize_or_zero().z);
        let cheeks = pair(local.x, local.y, layout.blush_spacing, layout.blush_y, 0.7 * e, 0.45 * e);
        let nose = bump(local.x, local.y, 0.0, at.nose_y, 0.3 * e, 0.25 * e);
        let flush = (CHEEK_FLUSH * cheeks + NOSE_FLUSH * nose) * front;
        #[allow(clippy::cast_possible_truncation, reason = "curvatures of a head are far inside f32")]
        let k = curvature[i] as f32;
        let cavity = (1.0 - CAVITY_PER_CURVATURE * (-k).max(0.0)).clamp(0.0, 1.0);
        #[allow(clippy::cast_possible_truncation, reason = "a weight in 0..=1")]
        info.extend_from_slice(&[0.0, cavity, flush.min(1.0) as f32, k.abs()]);
    }
    info
}

/// Each welded vertex's mean squared edge length.
fn edge_lengths(mesh: &MeshData, weld: &[usize]) -> Vec<f64> {
    let mut sums = vec![(0.0, 0_u32); mesh.positions.len()];
    for tri in mesh.indices.as_chunks::<3>().0 {
        for k in 0..3 {
            let (a, b) = (weld[tri[k] as usize], weld[tri[(k + 1) % 3] as usize]);
            if a == b {
                continue;
            }
            let squared = mesh.positions[a].as_dvec3().distance_squared(mesh.positions[b].as_dvec3());
            for v in [a, b] {
                sums[v].0 += squared;
                sums[v].1 += 1;
            }
        }
    }
    sums.into_iter().map(|(sum, count)| if count == 0 { 0.0 } else { sum / f64::from(count) }).collect()
}

/// How much denser a sculpted head's grid is at the front (phi) and round the eye line (theta), where the forms are.
const WARP_PHI: f64 = 0.55;
const WARP_THETA: f64 = 0.4;

/// The sphere grid's angles for a sculpted head: `u` (phi, from the front, `0..TAU`) and `v` (theta, from the top,
/// `0..PI`) moved so the columns crowd the face (about 2.2 times as many per radian at the front) and the rows the
/// equator (1.7 times), keeping both ends in place, so the grid's topology and seam are unchanged.
pub fn warp(u: f64, v: f64) -> (f64, f64) {
    (u - WARP_PHI * u.sin(), v + 0.5 * WARP_THETA * (2.0 * v).sin())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{PI, TAU};

    fn at() -> Landmarks {
        Landmarks::new(&FaceLayout::default(), &HeadOutline::default(), &Ears::default())
    }

    fn front(x: f64, y: f64) -> DVec3 {
        DVec3::new(x, y, (0.18_f64.powi(2) - x * x - y * y).max(0.0).sqrt())
    }

    #[test]
    fn a_flat_sculpt_moves_nothing() {
        assert!(Sculptor::new(&Sculpt::default(), at()).is_none());
    }

    #[test]
    fn each_form_moves_its_own_place_and_not_the_back_of_the_head() {
        let a = at();
        let e = a.eye_spacing;
        let only = |f: fn(&mut Sculpt)| {
            let mut s = Sculpt::default();
            f(&mut s);
            Sculptor::new(&s, a).unwrap()
        };
        let nose = only(|s| s.nose = 0.03);
        assert!(nose.height(front(0.0, a.nose_y)) > 0.02, "the nose stands out at its tip");
        assert!(nose.height(front(e, a.eye_y)).abs() < 1e-9, "not at the eyes");
        let sockets = only(|s| s.sockets = 0.02);
        assert!(sockets.height(front(0.92 * e, a.eye_y + 0.14 * e)) < -0.015, "the sockets go in");
        let brow = only(|s| s.brow = 0.02);
        assert!(brow.height(front(0.5 * e, a.eye_y + 0.85 * a.brow_gap)) > 0.01, "the brow stands out over the eyes");
        let all = Sculpt {
            brow: 0.03,
            sockets: 0.03,
            cheekbones: 0.03,
            temples: 0.02,
            nose: 0.06,
            mouth: 0.03,
            chin: 0.03,
            jaw: 0.03,
        };
        let all = Sculptor::new(&all, a).unwrap();
        for back in [DVec3::new(0.0, 0.0, -0.18), DVec3::new(0.0, 0.18, -0.05), DVec3::new(0.1, 0.1, -0.12)] {
            assert!(all.height(back).abs() < 1e-12, "{back:?}");
        }
        assert_eq!(all.height(DVec3::ZERO), 0.0);
        assert_eq!(all.apply(DVec3::ZERO), DVec3::ZERO);
    }

    #[test]
    fn the_warp_keeps_the_grid_s_ends_and_crowds_the_face() {
        let (u0, v0) = warp(0.0, 0.0);
        let (u1, v1) = warp(TAU, PI);
        assert!(u0.abs() < 1e-12 && v0.abs() < 1e-12 && (u1 - TAU).abs() < 1e-12 && (v1 - PI).abs() < 1e-12);
        let step = 1e-4;
        let at_front = warp(step, 0.5 * PI).0 - warp(0.0, 0.5 * PI).0;
        let at_back = warp(PI + step, 0.5 * PI).0 - warp(PI, 0.5 * PI).0;
        assert!(at_front < 0.5 * step && at_back > 1.4 * step, "{at_front} {at_back}");
        let mut last = -1.0;
        for k in 0..=1000 {
            let (u, v) = warp(TAU * f64::from(k) / 1000.0, PI * f64::from(k) / 1000.0);
            assert!(u > last - 1e-15 && v >= 0.0, "monotonic");
            last = u;
        }
    }
}
