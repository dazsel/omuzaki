//! Joint shaping: where two regions overlap, the rings are shaped so the outline runs through the joint without a step.
//!
//! **Shoulder.** An arm starts inside the torso, but its root ring is a disc standing proud of the shoulder line: the
//! torso's outline slopes down from the neck and the arm's rounded top rises out of it, a notch like an epaulette. The
//! arm therefore *grows a root*: rings continue backwards from its first ring along a curve that leaves tangent to the
//! arm's axis and bends to a point inside the torso just under the neck base, tapering as they go. The outline then
//! runs from the neck down the root's upper edge and round into the arm, with no corner where the arm used to start.
//! The extra rings lie inside the torso and (the cap closing them) are hidden by it.
//!
//! **Ankle.** The shin keeps going into the foot and widens to the foot's width ([`grow_ankle`]), and the front outline
//! of its lowest rings is then dilated over the foot's dome ([`fill_ankle`]), so the shin runs into the foot as one arc,
//! not a stub sitting on a dome with a waist between. The shin's width is measured to the outer edge, so it lands on
//! the dome's widest point whether or not the leg's and foot's centres share an x; a step there reads as a notch on the
//! Live mesh, whose end cap is a coarse bevel. The rings therefore go on down the dome's lower outline past its widest
//! point, so the cap starts inside the foot and never cuts across the dome.
//!
//! **Hip.** Between the pelvis and the thighs the template's outline can dip (a thigh narrower at its top than the pelvis
//! above it). [`fill_hip`] bridges the dip with a smooth arc, so the silhouette runs from the pelvis into the thigh.

use omuzaki_char_api::BodyRegion;
use omuzaki_core::glam::{DVec2, DVec3};

use super::mesh::{Ring, caps_of};
use crate::kit::curve::{TAU, fraction, lerp, sstep};

/// The radius of the arc that bridges the hip, as a fraction of the torso's length.
const HIP_RHO: f64 = 0.16;

/// How far down from the torso's top ring the root ends, as a fraction of the way to the arm's first ring.
const ROOT_DROP: f64 = 0.1;
/// How far from the torso's axis the root ends, as a fraction of the torso top ring's half-width.
const ROOT_OFFSET: f64 = 0.4;
/// The root's end radii, as a fraction of the torso top ring's.
const ROOT_TAPER: f64 = 0.4;

/// The radius of the arc that bridges the ankle, as a fraction of the leg's length.
const ANKLE_RHO: f64 = 0.11;
/// Points taken round each of the foot's cross-sections to find its dome.
const DOME_SAMPLES: usize = 24;
/// How far round the widest section's lower half the leg follows the dome, as an angle from its widest point.
const DOME_TAIL: f64 = 1.0;
/// Bisection steps that find the deepest tail angle whose end cap clears the sole.
const TAIL_BISECTIONS: usize = 24;
/// The shallowest tail worth building, radians.
const MIN_TAIL_ANGLE: f64 = 0.05;
/// The narrowest a ring on the dome's lower outline gets, as a fraction of the widest section's half-width.
const MIN_TAIL: f64 = 0.05;
/// How far above the foot's top the leg's rings are dilated, in arc radii.
const ANKLE_ZONE: f64 = 0.5;

/// Shapes the rings of every region in place: the hip valley is bridged, each leg grows an ankle (`ankle_rings` rings)
/// into the foot and each arm a root (`root_rings` rings) into the torso.
pub fn shape(rings: &mut [(BodyRegion, Vec<Ring>)], root_rings: usize, ankle_rings: usize) {
    fill_hip(rings);
    grow_ankles(rings, ankle_rings);
    let Some(top) = rings.iter().find(|(r, _)| *r == BodyRegion::Torso).and_then(|(_, v)| v.last().copied()) else {
        return;
    };
    for (region, arm) in rings.iter_mut() {
        if let (BodyRegion::ArmL | BodyRegion::ArmR, Some(root)) = (*region, grow_root(&top, arm, root_rings)) {
            arm.splice(0..0, root);
        }
    }
}

/// The rings that lead from just inside the torso's top to `arm`'s first ring, ordered towards the arm (the first ring
/// is the innermost; the arm's own first ring follows it).
fn grow_root(top: &Ring, arm: &[Ring], count: usize) -> Option<Vec<Ring>> {
    let start = arm.first()?;
    let side = if start.centre.x >= 0.0 { 1.0 } else { -1.0 };
    let end = DVec3::new(
        top.centre.x + side * ROOT_OFFSET * top.radii.x,
        lerp(top.centre.y, start.centre.y, ROOT_DROP),
        top.centre.z,
    );
    let control = start.centre - start.axis * (0.5 * start.centre.distance(end));
    let end_radii = top.radii * ROOT_TAPER;
    // A quadratic Bezier from the arm's first ring to `end`, leaving against the arm's axis, and its direction.
    let at = |s: f64| start.centre * (1.0 - s).powi(2) + control * (2.0 * (1.0 - s) * s) + end * (s * s);
    let toward_end = |s: f64| (control - start.centre) * (2.0 * (1.0 - s)) + (end - control) * (2.0 * s);
    (1..=count)
        .rev()
        .map(|k| {
            let s = fraction(k, count);
            let axis = (-toward_end(s)).try_normalize()?;
            let through = (start.through - axis * start.through.dot(axis)).try_normalize()?;
            let radii = DVec2::new(
                lerp(start.radii.x, end_radii.x, sstep(0.0, 1.0, s)),
                lerp(start.radii.y, end_radii.y, sstep(0.0, 1.0, s)),
            );
            Some(Ring { centre: at(s), axis, across: through.cross(axis), through, radii })
        })
        .collect()
}

/// An edge of a ring seen from the front or the side: how far out it reaches, and how to push it further out.
#[derive(Copy, Clone)]
enum Edge {
    /// The outer edge (+x, front view).
    Outer,
    /// The front edge (+z, side view).
    Front,
    /// The back edge (-z, side view).
    Back,
}

impl Edge {
    const ALL: [Self; 3] = [Self::Outer, Self::Front, Self::Back];

    /// The ring's half-extent along the world axis the edge is on, from its centre.
    fn reach(self, ring: &Ring) -> f64 {
        let (a, t) = match self {
            Self::Outer => (ring.across.x, ring.through.x),
            Self::Front | Self::Back => (ring.across.z, ring.through.z),
        };
        (a * ring.radii.x).hypot(t * ring.radii.y)
    }

    /// The edge's coordinate (x for the outer edge, z for the front, and minus z for the back, so that larger is
    /// further out for all three).
    fn at(self, ring: &Ring) -> f64 {
        match self {
            Self::Outer => ring.centre.x + self.reach(ring),
            Self::Front => ring.centre.z + self.reach(ring),
            Self::Back => -ring.centre.z + self.reach(ring),
        }
    }

    /// Moves the edge out to `target` if it is short of it.
    fn extend(self, ring: &mut Ring, target: f64) {
        if target > self.at(ring) {
            self.move_to(ring, target);
        }
    }

    /// Moves the edge to `target`, in or out. The outer edge keeps the centre and scales `radii.x`; a front or back edge
    /// changes `radii.y` by half the gap and moves the centre the other half, so the opposite edge stays.
    fn move_to(self, ring: &mut Ring, target: f64) {
        let gap = target - self.at(ring);
        match self {
            Self::Outer => {
                let reach = self.reach(ring);
                if reach > 1e-9 {
                    ring.radii.x *= (reach + gap) / reach;
                }
            }
            Self::Front => {
                ring.radii.y += gap / 2.0;
                ring.centre.z += gap / 2.0;
            }
            Self::Back => {
                ring.radii.y += gap / 2.0;
                ring.centre.z -= gap / 2.0;
            }
        }
    }
}

/// Fills the valley in the outline between the pelvis and the thighs, seen from the front and from the side. A
/// parabolic dilation of the edge samples of the torso's lowest rings and the leg's highest,
/// `target(y) = max_j (edge_j - (y - y_j)^2 / 2 rho)`, is at least the edge everywhere and equals it on every hill the
/// parabola is flatter than: where the template's thigh is narrower at its top than the pelvis above it (or the leg is
/// tucked in under the torso), the target bridges the two hills with an arc tangent to both, so the outline has no
/// corner. Rings are widened to the target; a smooth outline is left as it is. `rho` scales with the torso so the arc is
/// as gentle on a tall body as on a chibi.
fn fill_hip(rings: &mut [(BodyRegion, Vec<Ring>)]) {
    let zone = |v: &[Ring]| v[..(v.len() * 2).div_ceil(5)].to_vec();
    let find =
        |rings: &[(BodyRegion, Vec<Ring>)], region| rings.iter().find(|(r, _)| *r == region).map(|(_, v)| v.clone());
    let (Some(full), Some(leg)) = (find(rings, BodyRegion::Torso), find(rings, BodyRegion::LegL)) else { return };
    let (Some(first), Some(last)) = (full.first(), full.last()) else { return };
    let rho = HIP_RHO * (last.centre.y - first.centre.y);
    let (torso, leg) = (zone(&full), zone(&leg));
    for edge in Edge::ALL {
        let samples: Vec<(f64, f64)> = torso.iter().chain(&leg).map(|r| (r.centre.y, edge.at(r))).collect();
        let target =
            |y: f64| samples.iter().map(|&(yj, w)| w - (y - yj).powi(2) / (2.0 * rho)).fold(f64::MIN, f64::max);
        let (lo, hi) = samples.iter().fold((f64::MAX, f64::MIN), |(a, b), s| (a.min(s.0), b.max(s.0)));
        for (region, v) in rings.iter_mut() {
            let part: &mut [Ring] = match region {
                BodyRegion::Torso => &mut v[..torso.len()],
                BodyRegion::LegL | BodyRegion::LegR => &mut v[..leg.len()],
                _ => continue,
            };
            for ring in part.iter_mut().filter(|r| (lo..=hi).contains(&r.centre.y)) {
                let target = target(ring.centre.y);
                on_left(ring, *region == BodyRegion::LegR && matches!(edge, Edge::Outer), |r| edge.extend(r, target));
            }
        }
    }
}

/// Runs `f` on `ring`, or on its mirror image when `mirrored`, copying the radii back: the right leg's outer edge is on
/// -x, where [`Edge::Outer`] looks at +x.
fn on_left(ring: &mut Ring, mirrored: bool, f: impl FnOnce(&mut Ring)) {
    if !mirrored {
        return f(ring);
    }
    let flip = |v: DVec3| DVec3::new(-v.x, v.y, v.z);
    let mut left = Ring { centre: flip(ring.centre), across: flip(ring.across), through: flip(ring.through), ..*ring };
    f(&mut left);
    ring.radii = left.radii;
}

/// The rings that lead down from the end of a leg, ordered downwards: the shin keeps going straight down into the
/// foot (the last `count.div_ceil(4)` of the `count` rings go on down the dome's lower outline, a quarter ellipse of the
/// widest section), its width following a cubic Hermite curve that leaves the leg's last ring at the shin's own slope
/// (taken from its last two rings) and arrives vertical at the width of the foot's widest section, where the foot's own outline is
/// vertical. The shin's outline then runs into the foot's as one smooth arc. `side` is +1 for the left leg and -1 for the
/// right: the edge that has to meet the dome is the outer one, so the width is measured to it. Nothing if the foot's widest
/// section is not below the leg's end. `angle` is how far round the widest section's lower half the tail goes (no tail
/// at or below [`MIN_TAIL_ANGLE`]).
fn grow_ankle(leg: &[Ring], foot: &[Ring], count: usize, side: f64, angle: f64) -> Vec<Ring> {
    let (Some(last), Some(widest)) = (leg.last(), foot.iter().max_by(|a, b| a.radii.x.total_cmp(&b.radii.x))) else {
        return Vec::new();
    };
    let depth = last.centre.y - widest.centre.y;
    if depth <= 0.0 || last.axis.y.abs() < 1e-9 {
        return Vec::new();
    }
    let length = depth / last.axis.y.abs();
    // Radius change per metre going down the shin, so the growth starts without a kink.
    let slope = match leg.len().checked_sub(2).map(|k| &leg[k]) {
        Some(prev) if prev.centre.distance(last.centre) > 1e-9 => {
            (last.radii.x - prev.radii.x) / prev.centre.distance(last.centre)
        }
        _ => 0.0,
    };
    // The width that puts the shin's outer edge on the dome's widest point: the two centres need not share an x.
    let end_width = widest.radii.x + side * (widest.centre.x - (last.centre.x + last.axis.x * length));
    let drop = (widest.across.y * widest.radii.x).hypot(widest.through.y * widest.radii.y);
    // The tail ring at `angle` round the widest section's lower half: as wide as the dome there, as thick as the shin
    // scaled by the same cosine.
    let ring_at = |angle: f64| {
        let (sin, cos) = angle.sin_cos();
        Ring {
            centre: last.centre + last.axis * (length + drop * sin / last.axis.y.abs()),
            radii: DVec2::new(
                (end_width - widest.radii.x * (1.0 - cos)).max(MIN_TAIL * widest.radii.x),
                last.radii.y * cos,
            ),
            ..*last
        }
    };
    // The tail takes a quarter of the rings, so the leg's triangle count stays what it was.
    let tail = if drop > 1e-9 && angle > MIN_TAIL_ANGLE { count.div_ceil(4).min(count.saturating_sub(1)) } else { 0 };
    let reach = count - tail;
    let upper = (1..=reach).map(|k| {
        let s = fraction(k, reach);
        let (s2, s3) = (s * s, s * s * s);
        let width = last.radii.x * (2.0 * s3 - 3.0 * s2 + 1.0)
            + slope * length * (s3 - 2.0 * s2 + s)
            + end_width * (3.0 * s2 - 2.0 * s3);
        Ring { centre: last.centre + last.axis * (s * length), radii: DVec2::new(width, last.radii.y), ..*last }
    });
    // Then on down the dome's lower outline, so the leg's end cap starts inside the foot and a coarse Live bevel never
    // cuts across the dome.
    let dome = (1..=tail).map(|k| ring_at(fraction(k, tail) * angle));
    upper.chain(dome).collect()
}

/// Appends [`grow_ankle`]'s rings to both legs. The tail goes as deep down the dome as the leg's end cap, built past the
/// last ring once [`fill_ankle`] has widened it, still clears the foot's lowest point: the deepest such angle is found
/// by bisection, since the clearance falls as the angle grows.
fn grow_ankles(rings: &mut [(BodyRegion, Vec<Ring>)], count: usize) {
    for (leg_region, foot_region) in [(BodyRegion::LegL, BodyRegion::FootL), (BodyRegion::LegR, BodyRegion::FootR)] {
        let Some(foot) = rings.iter().find(|(r, _)| *r == foot_region).map(|(_, v)| v.clone()) else { continue };
        if let Some((_, leg)) = rings.iter_mut().find(|(r, _)| *r == leg_region) {
            let mirrored = leg_region == BodyRegion::LegR;
            let side = if mirrored { -1.0 } else { 1.0 };
            let floor = foot
                .iter()
                .flat_map(|r| (0..DOME_SAMPLES).map(|k| r.point(TAU * fraction(k, DOME_SAMPLES)).y))
                .fold(f64::MAX, f64::min);
            let build = |angle: f64| {
                let mut shaped = leg.clone();
                shaped.extend(grow_ankle(leg, &foot, count, side, angle));
                fill_ankle(&mut shaped, &foot, mirrored);
                shaped
            };
            let clearance = |shaped: &[Ring]| {
                shaped.last().map_or(f64::MAX, |end| {
                    let cap = caps_of(leg_region, std::slice::from_ref(end), 1).1.length;
                    end.centre.y - cap * end.axis.y.abs() - floor
                })
            };
            let mut angle = DOME_TAIL;
            if clearance(&build(angle)) < 0.0 {
                let (mut lo, mut hi) = (0.0, DOME_TAIL);
                for _ in 0..TAIL_BISECTIONS {
                    let mid = f64::midpoint(lo, hi);
                    if clearance(&build(mid)) >= 0.0 { lo = mid } else { hi = mid }
                }
                angle = lo;
            }
            *leg = build(angle);
        }
    }
}

/// Bridges the waist between the shin and the foot's dome, seen from the front: a parabolic dilation (as in
/// [`fill_hip`]) of the outer edge of the leg's rings and of the foot's cross-sections, `target(y) = max_j (edge_j -
/// (y - y_j)^2 / 2 rho)`, widens the leg's rings to an arc tangent to both the shin and the dome, wherever the dome
/// stands out past the shin. The foot's section outlines are sampled all round, since its widest point is not at the
/// height of its top. `mirrored` is the right leg, whose outer edge is on -x.
fn fill_ankle(leg: &mut [Ring], foot: &[Ring], mirrored: bool) {
    let (Some(first), Some(last)) = (leg.first(), leg.last()) else { return };
    let rho = ANKLE_RHO * first.centre.distance(last.centre);
    let side = if mirrored { -1.0 } else { 1.0 };
    let dome = foot
        .iter()
        .flat_map(|r| (0..DOME_SAMPLES).map(move |k| r.point(TAU * fraction(k, DOME_SAMPLES))))
        .map(|p| (p.y, side * p.x));
    let top = dome.clone().fold(f64::MIN, |a, (y, _)| a.max(y));
    let zone_from = leg.iter().position(|r| r.centre.y <= top + rho * ANKLE_ZONE).unwrap_or(leg.len());
    let samples: Vec<(f64, f64)> =
        leg[zone_from..].iter().map(|r| (r.centre.y, side * r.centre.x + Edge::Outer.reach(r))).chain(dome).collect();
    for ring in &mut leg[zone_from..] {
        let y = ring.centre.y;
        let target = samples.iter().map(|&(yj, w)| w - (y - yj).powi(2) / (2.0 * rho)).fold(f64::MIN, f64::max);
        on_left(ring, mirrored, |r| Edge::Outer.extend(r, target));
    }
}
