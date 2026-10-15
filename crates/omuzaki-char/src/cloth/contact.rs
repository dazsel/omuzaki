//! Cloth contacts and wind: sphere and capsule colliders with Coulomb friction, and an aerodynamic wind field,
//! as a [`SubstepHook`] for the cloth solver ([`ClothSolver::step_with`](super::solver::ClothSolver::step_with)).
//!
//! # Contacts
//!
//! Colliders are the rig's [`ColliderShape`]s (VRM spheres and capsules; the inside shapes and planes are for springs and are skipped) placed in the solver's space by the caller
//! ([`Shape::from_collider`]). Each frame the caller gives every collider's pose at the frame's end; the hook moves it
//! linearly from its pose at the frame's start, so a collider that follows the pose is where the pins are in every
//! sub-step. A sphere is a capsule with both ends at its centre, so one code path serves both.
//!
//! A particle is kept `radius + thickness + margin` from a collider's axis, where `thickness` is the cloth's
//! half-thickness ([`ClothParams::thickness`](super::solver::ClothParams)) and `margin` is extra clearance. The
//! projection runs after the constraints in each sub-step (XPBD's contact, a position correction), and the solver then
//! derives velocities from the corrected positions, so contacts are inelastic and carry no jitter of their own.
//!
//! **No tunnelling.** The particle's path over the sub-step, taken in the collider's moving frame, is swept against the
//! collider inflated by that distance; a particle that enters it is put on the surface it entered through, carried to
//! the collider's final pose. This holds at any relative speed up to the solver's [`MAX_SPEED`] clamp.
//!
//! **Thin colliders (limit 1, #129).** The sweep tests particles, so a collider thinner than the particle spacing (a
//! hairpin, a 3 mm strap against 2 cm cells) could pass between particles. Once the environment knows the sheet
//! ([`ClothEnvironment::set_sheet`]), every collider whose diameter (with clearance) is under the sheet's longest rest
//! edge is also tested against the sheet's **triangles**: its axis, clipped to each nearby triangle's prism, must stay
//! `radius + thickness + margin` on the side of the triangle it started the sub-step on, and a triangle that is closer
//! is moved away by that depth, shared among its corners by barycentric weight and inverse mass (the triangle-point
//! constraint of position-based dynamics, with the collider immovable). A 3 mm capsule swung at 5 m/s through a sheet
//! of 1.9 cm cells crosses no triangle (`tests/cloth_contact.rs`). What remains: the side is judged from the axis's
//! midpoint at the sub-step's start, so a collider already through the sheet stays through it, and a collider that
//! moves more than a cell in one sub-step (over 7 m/s at 1.9 cm cells and the live 1/360 s sub-step) can still pass
//! between triangles' prisms.
//!
//! **Overlapping colliders (limit 2, #129).** Colliders are met one after the other, so a particle pressed into the
//! crease between two overlapping colliders could be pushed out of the second back into the first. A particle that
//! touched two or more colliders in a sub-step is therefore resolved again: up to three times, it is put on the crease
//! of the two colliders it is deepest in (the circle where their inflated surfaces meet: outside both), or pushed out
//! radially when it is inside only one. A particle squeezed between two 5 cm spheres 6 cm apart stays outside both over
//! 10 s. What remains: three or more colliders overlapping at one point are resolved two at a time, so the particle can
//! end up to a sub-step's travel inside the third.
//!
//! **Friction** is Coulomb's, in position-based form (Macklin et al., "Unified Particle Physics", 2014): the depth
//! `d` a contact pushed the particle out is the normal impulse, and the particle's sub-step displacement relative to
//! the collider's surface, tangential to the contact, is cancelled outright if it is below `μs · d` (static: the
//! particle sticks) and cancelled down to `μk · d` otherwise (kinetic: it slides). A sheet on a slope of angle `θ`
//! therefore sticks exactly while `tan θ < μs`. Contact points move with the collider, so a rolling or swinging
//! collider drags the cloth along.
//!
//! # Wind
//!
//! [`Wind`] is a velocity field, a steady vector plus a gust that is a pure function of the simulation clock and
//! position (no random numbers, so replays are exact). Per triangle, with `u` the air's velocity relative to the
//! triangle, `A` its area and `n` its unit normal: a pressure drag `drag · A · (u·n)|u·n| n` and a lift
//! `lift · A · |u·n| · |u_perp|` perpendicular to `u`, on the side the air pushes. For `lift = 0` a hanging flag in a
//! steady wind settles where `drag u² cos²θ = ρ g sin θ` (`ρ` its mass per area, `θ` from the vertical). The solver's
//! own `damping` is the still-air drag and stays on, so wind adds to it.
//!
//! # Cost
//!
//! [`ClothEnvironment`] allocates once in `new`; placing colliders and stepping never allocate. Each sub-step bounds
//! every collider's swept volume once and rejects a particle with one distance test, so only particles near a collider
//! pay for the sweep (`cargo bench -p omuzaki-char --bench cloth_contact`).

// Ray, capsule and triangle geometry in the textbook's single-letter names (Ericson, Real-Time Collision Detection).
#![allow(clippy::many_single_char_names)]

use omuzaki_anim_api::ColliderShape;
use omuzaki_core::glam::{Affine3A, Vec3, Vec3A};

use super::solver::{SheetShape, Substep, SubstepHook};

/// Particles per broad-phase chunk.
const CHUNK: usize = 8;

/// Why a collider, a parameter or a wind was refused.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ContactError {
    /// A shape with a non-finite coordinate or a radius that is not finite and positive.
    #[error("collider {index} is not finite or has a radius that is not finite and positive")]
    BadShape {
        /// The collider's slot.
        index: usize,
    },
    /// More colliders than the environment was built for.
    #[error("collider slot {index} is beyond the environment's capacity of {capacity}")]
    Capacity {
        /// The slot asked for.
        index: usize,
        /// Slots the environment has.
        capacity: usize,
    },
    /// A parameter outside its range.
    #[error("{name} = {value} must be {expected}")]
    Param {
        /// The field.
        name: &'static str,
        /// What it was.
        value: f32,
        /// What it must be.
        expected: &'static str,
    },
}

/// A collider in the solver's space: the volume within `radius` of the segment `a`-`b` (a sphere when `a == b`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Shape {
    a: Vec3A,
    b: Vec3A,
    radius: f32,
}

impl Shape {
    /// A ball of `radius` metres at `centre`.
    pub fn sphere(centre: Vec3, radius: f32) -> Self {
        Self { a: centre.into(), b: centre.into(), radius }
    }

    /// The volume within `radius` of the segment from `a` to `b`.
    pub fn capsule(a: Vec3, b: Vec3, radius: f32) -> Self {
        Self { a: a.into(), b: b.into(), radius }
    }

    /// A rig collider placed by `to_solver`, its bone's space in the solver's (see `omuzaki_anim_api::Collider`). The
    /// radius scales by the transform's mean scale.
    ///
    /// Cloth contacts are spheres and capsules only: the inside shapes and planes of the spring colliders (and any
    /// shape added later) return `None` and are skipped.
    pub fn from_collider(shape: &ColliderShape, to_solver: &Affine3A) -> Option<Self> {
        let scale = libm::cbrtf(to_solver.matrix3.determinant().abs());
        match *shape {
            ColliderShape::Sphere { offset, radius } => {
                Some(Self::sphere(to_solver.transform_point3(offset), radius * scale))
            }
            ColliderShape::Capsule { offset, radius, tail } => Some(Self::capsule(
                to_solver.transform_point3(offset),
                to_solver.transform_point3(tail),
                radius * scale,
            )),
            _ => None,
        }
    }

    /// The axis's two ends (equal for a sphere).
    pub fn axis(&self) -> (Vec3, Vec3) {
        (self.a.into(), self.b.into())
    }

    /// The radius, metres.
    pub fn radius(&self) -> f32 {
        self.radius
    }

    fn is_valid(&self) -> bool {
        self.a.is_finite() && self.b.is_finite() && self.radius.is_finite() && self.radius > 0.0
    }

    fn lerp(&self, to: &Self, t: f32) -> Self {
        Self { a: self.a.lerp(to.a, t), b: self.b.lerp(to.b, t), radius: self.radius + (to.radius - self.radius) * t }
    }
}

/// Coulomb friction coefficients (dimensionless, `kinetic <= static_`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Friction {
    /// Below this the cloth sticks: `tan` of the steepest slope it holds.
    pub static_: f32,
    /// While sliding, the share of the normal push that is cancelled.
    pub kinetic: f32,
}

/// How contacts keep the cloth off colliders.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ContactParams {
    /// The cloth's half-thickness, metres: pass [`ClothParams::thickness`](super::solver::ClothParams).
    pub thickness: f32,
    /// Extra clearance, metres, so a resting sheet is never exactly on the collider.
    pub margin: f32,
    /// Friction against colliders.
    pub friction: Friction,
}

impl Default for ContactParams {
    /// 2 mm thickness (the cloth default), 1 mm margin, and cotton on a skin-like surface: `μs = 0.6`, `μk = 0.4`.
    fn default() -> Self {
        Self { thickness: 0.002, margin: 0.001, friction: Friction { static_: 0.6, kinetic: 0.4 } }
    }
}

impl ContactParams {
    /// Checks every field is in range.
    ///
    /// # Errors
    /// [`ContactError::Param`] naming the first field out of range.
    pub fn validate(&self) -> Result<(), ContactError> {
        let non_negative = |name, value: f32| {
            if value.is_finite() && value >= 0.0 {
                Ok(())
            } else {
                Err(ContactError::Param { name, value, expected: "finite and >= 0" })
            }
        };
        non_negative("thickness", self.thickness)?;
        non_negative("margin", self.margin)?;
        non_negative("friction.static_", self.friction.static_)?;
        non_negative("friction.kinetic", self.friction.kinetic)?;
        if self.friction.kinetic > self.friction.static_ {
            return Err(ContactError::Param {
                name: "friction.kinetic",
                value: self.friction.kinetic,
                expected: "<= the static coefficient",
            });
        }
        Ok(())
    }
}

/// A collider's pose at the frame's start and end.
#[derive(Copy, Clone, Debug)]
struct Slot {
    from: Shape,
    to: Shape,
}

/// A collider in one sub-step: the pose it starts it in, its motion over it, and the sphere holding the swept volume.
#[derive(Copy, Clone, Debug, Default)]
struct Pose {
    /// Start of the axis, and the axis vector, at the sub-step's start and end.
    a0: Vec3A,
    e0: Vec3A,
    a1: Vec3A,
    e1: Vec3A,
    /// `axis_length_squared` of `e0` and `e1`.
    len0: f32,
    len1: f32,
    /// How far the axis's midpoint moved: removed from a particle's path to sweep in the collider's frame.
    shift: Vec3A,
    /// The distance a particle is kept from the axis.
    reach: f32,
    /// The most any point of the axis moved over the sub-step.
    moved: f32,
    centre: Vec3A,
    bound: f32,
}

impl Pose {
    fn new(from: &Shape, to: &Shape, f0: f32, f1: f32, clearance: f32) -> Self {
        let (s0, s1) = (from.lerp(to, f0), from.lerp(to, f1));
        let (a0, e0, a1, e1) = (s0.a, s0.b - s0.a, s1.a, s1.b - s1.a);
        let reach = s0.radius.max(s1.radius) + clearance;
        let centre = (a0 + a0 + e0 + a1 + a1 + e1) * 0.25;
        let far = |p: Vec3A| p.distance(centre);
        let bound = far(a0).max(far(a0 + e0)).max(far(a1)).max(far(a1 + e1)) + reach;
        Self {
            a0,
            e0,
            a1,
            e1,
            len0: axis_length_squared(e0),
            len1: axis_length_squared(e1),
            shift: (a1 + e1 * 0.5) - (a0 + e0 * 0.5),
            reach,
            moved: a0.distance(a1).max((a0 + e0).distance(a1 + e1)),
            centre,
            bound,
        }
    }
}

/// `|e|²`, or 0 for an axis too short to have a direction (a sphere): computed once per sub-step.
fn axis_length_squared(e: Vec3A) -> f32 {
    let length_squared = e.length_squared();
    if length_squared > 1e-12 { length_squared } else { 0.0 }
}

/// Where along the segment `a + s e` (`s` in `[0, 1]`) `p` is closest, given `length_squared = axis_length_squared(e)`.
/// A true division, not a multiplication by a reciprocal: results stay bit-identical to the plain formula.
fn axis_param(p: Vec3A, a: Vec3A, e: Vec3A, length_squared: f32) -> f32 {
    if length_squared > 0.0 { ((p - a).dot(e) / length_squared).clamp(0.0, 1.0) } else { 0.0 }
}

/// First `t` in `[0, 1]` at which the ray `m + t d` enters the ball of radius `r` at the origin (`m` outside it).
fn ray_ball(m: Vec3A, d: Vec3A, r: f32) -> Option<f32> {
    let a = d.length_squared();
    let (b, c) = (m.dot(d), m.length_squared() - r * r);
    let disc = b * b - a * c;
    if a < 1e-20 || disc < 0.0 {
        return None;
    }
    let t = (-b - disc.sqrt()) / a;
    (0.0..=1.0).contains(&t).then_some(t)
}

/// Where the path `p + t d`, `t` in `[0, 1]`, starting outside the capsule of radius `r` around `a + s e`, first
/// enters it: `(t, s)` of the entry point's nearest axis point.
fn sweep(p: Vec3A, d: Vec3A, a: Vec3A, e: Vec3A, r: f32) -> Option<(f32, f32)> {
    let mut best: Option<(f32, f32)> = None;
    let mut keep = |t: f32, s: f32| {
        if best.is_none_or(|(b, _)| t < b) {
            best = Some((t, s));
        }
    };
    for (s, end) in [(0.0, a), (1.0, a + e)] {
        if let Some(t) = ray_ball(p - end, d, r) {
            keep(t, s);
        }
    }
    let e2 = e.length_squared();
    if e2 > 1e-12 {
        // The infinite cylinder, then only the part between the caps (Ericson, Real-Time Collision Detection, 5.3.7).
        let (m, nd) = (p - a, d.dot(e));
        let md = m.dot(e);
        let quad_a = e2 * d.length_squared() - nd * nd;
        let quad_b = e2 * m.dot(d) - md * nd;
        let quad_c = e2 * (m.length_squared() - r * r) - md * md;
        let disc = quad_b * quad_b - quad_a * quad_c;
        if quad_a.abs() > 1e-12 * e2 && disc >= 0.0 {
            let t = (-quad_b - disc.sqrt()) / quad_a;
            let s = (md + t * nd) / e2;
            if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&s) {
                keep(t, s);
            }
        }
    }
    best
}

/// A unit vector for a particle exactly on the axis: away from where it was, else any perpendicular to the axis.
fn fallback_normal(away: Vec3A, e: Vec3A) -> Vec3A {
    let away = away.normalize_or_zero();
    if away != Vec3A::ZERO {
        return away;
    }
    let side = if e.normalize_or_zero().dot(Vec3A::Y).abs() < 0.9 { Vec3A::Y } else { Vec3A::X };
    e.cross(side).normalize_or(Vec3A::Y)
}

/// Moves one particle out of one collider, if it is in it or passed through it this sub-step, and applies friction.
/// Returns the new position.
fn contact(pose: &Pose, friction: Friction, x0: Vec3A, x1: Vec3A, travel: f32) -> Vec3A {
    let r = pose.reach;
    // Where it ended, if that is inside the collider's skin.
    let end_s = axis_param(x1, pose.a1, pose.e1, pose.len1);
    let off = x1 - (pose.a1 + pose.e1 * end_s);
    // Anything touched on the way was within the particle's own travel plus the axis's of where it ended.
    let touch = r + travel + pose.moved;
    if off.length_squared() > touch * touch {
        return x1;
    }
    let inside_at_end = off.length_squared() < r * r;
    // A particle resting on the skin is on it only to rounding, so "outside" allows slack: otherwise half of all
    // resting contacts would read as having started inside and be pushed out along the wrong normal.
    let start_s = axis_param(x0, pose.a0, pose.e0, pose.len0);
    let from_axis = x0 - (pose.a0 + pose.e0 * start_s);
    let outside_at_start = from_axis.length_squared() > 0.96 * r * r;
    // Where the path entered the collider (in its own frame): the normal that tells which side the particle came from.
    // A path too short to reach the skin from where it started cannot enter it.
    let path = x1 - pose.shift - x0;
    let reach = r + path.length();
    let entry = if outside_at_start && from_axis.length_squared() <= reach * reach {
        sweep(x0, path, pose.a0, pose.e0, r).map(|(t, s)| {
            let q = x0 + path * t;
            (s, (q - (pose.a0 + pose.e0 * s)).normalize_or_zero())
        })
    } else {
        None
    };
    let ended =
        || (end_s, if off.length_squared() > 1e-12 { off.normalize() } else { fallback_normal(from_axis, pose.e1) });
    // Resting and sliding contacts push straight out of where the particle ended, which keeps its tangential motion.
    // A particle that passed deep into or through the collider (its end-normal faces away from the way it came in)
    // goes back to the surface it entered through.
    let (s, normal) = match entry {
        Some((s, n)) if n != Vec3A::ZERO => {
            if inside_at_end && ended().1.dot(n) > 0.0 {
                ended()
            } else {
                (s, n)
            }
        }
        _ if inside_at_end => ended(),
        _ => return x1,
    };
    let surface = pose.a1 + pose.e1 * s + normal * r;
    let push = (surface - x1).dot(normal);
    if push <= 0.0 {
        return x1;
    }
    // Friction: the displacement relative to the surface point under the particle, tangential to the contact.
    let carried = (pose.a1 - pose.a0) + (pose.e1 - pose.e0) * s;
    let slip = (surface - x0) - carried;
    let slip = slip - normal * slip.dot(normal);
    let length = slip.length();
    if length <= 1e-9 {
        return surface;
    }
    let share = if length <= friction.static_ * push { 1.0 } else { (friction.kinetic * push / length).min(1.0) };
    surface - slip * share
}

/// Puts a particle at `x` outside the colliders it is inside at their end pose, two at a time (limit 2 in the
/// [module docs](self)): on the crease of the two it is deepest in, or radially out of one.
fn resolve_overlap(poses: &[Pose], mut x: Vec3A) -> Vec3A {
    for _ in 0..3 {
        // The two colliders `x` is deepest in, as (depth, nearest axis point, reach).
        let mut first: Option<(f32, Vec3A, f32)> = None;
        let mut second: Option<(f32, Vec3A, f32)> = None;
        for pose in poses {
            let c = pose.a1 + pose.e1 * axis_param(x, pose.a1, pose.e1, pose.len1);
            let depth = pose.reach - x.distance(c);
            if depth <= 1e-7 {
                continue;
            }
            let entry = Some((depth, c, pose.reach));
            if first.is_none_or(|(d, ..)| depth > d) {
                second = first;
                first = entry;
            } else if second.is_none_or(|(d, ..)| depth > d) {
                second = entry;
            }
        }
        let Some((_, c1, r1)) = first else { return x };
        let radial = |x: Vec3A, c: Vec3A, r: f32| c + fallback_normal(x - c, Vec3A::Y) * r;
        x = match second {
            None => radial(x, c1, r1),
            Some((_, c2, r2)) => {
                let axis = c2 - c1;
                let length = axis.length();
                if length < 1e-7 {
                    radial(x, c1, r1.max(r2))
                } else {
                    // The crease: the circle where the two spheres' surfaces meet.
                    let dir = axis / length;
                    let along = (length * length + r1 * r1 - r2 * r2) / (2.0 * length);
                    let rho2 = r1 * r1 - along * along;
                    if rho2 <= 0.0 {
                        // One sphere holds the other's whole cap: leave the larger.
                        if r1 >= r2 { radial(x, c1, r1) } else { radial(x, c2, r2) }
                    } else {
                        let middle = c1 + dir * along;
                        let q = x - middle;
                        let out = fallback_normal(q - dir * q.dot(dir), dir);
                        middle + out * rho2.sqrt()
                    }
                }
            }
        };
    }
    x
}

/// Keeps the sheet's triangles on their own side of a thin collider (limit 1 in the [module docs](self)).
#[allow(clippy::similar_names)] // corner names
fn triangle_contacts(
    pose: &Pose,
    shape: SheetShape,
    spacing: f32,
    positions: &mut [Vec3A],
    previous: &[Vec3A],
    inv_mass: &[f32],
) {
    let cols = shape.cols;
    let cell_cols = if shape.wrap { cols } else { cols - 1 };
    let at = |r: usize, c: usize| r * cols + c % cols;
    let mid0 = pose.a0 + pose.e0 * 0.5;
    // A push never exceeds what one sub-step can make up: the collider's reach and travel and a cell.
    let most = pose.reach + pose.moved + spacing;
    for r in 0..shape.rows - 1 {
        for c in 0..cell_cols {
            let corners = [at(r, c), at(r, c + 1), at(r + 1, c), at(r + 1, c + 1)];
            let centre = corners.iter().map(|&i| positions[i]).sum::<Vec3A>() * 0.25;
            let extent = corners.iter().map(|&i| positions[i].distance(centre)).fold(0.0, f32::max);
            if centre.distance(pose.centre) > pose.bound + extent {
                continue;
            }
            for [i, j, k] in [[corners[0], corners[2], corners[1]], [corners[1], corners[2], corners[3]]] {
                push_triangle(pose, [i, j, k], mid0, most, positions, previous, inv_mass);
            }
        }
    }
}

/// One triangle against one thin collider: see [`triangle_contacts`].
fn push_triangle(
    pose: &Pose,
    [i, j, k]: [usize; 3],
    mid0: Vec3A,
    most: f32,
    positions: &mut [Vec3A],
    previous: &[Vec3A],
    inv_mass: &[f32],
) {
    let (x0, x1, x2) = (positions[i], positions[j], positions[k]);
    let normal = (x1 - x0).cross(x2 - x0);
    let area2 = normal.length();
    if area2 < 1e-12 {
        return;
    }
    let mut n = normal / area2;
    // The side the collider started on, from where the triangle and the axis's midpoint were.
    let (y0, y1, y2) = (previous[i], previous[j], previous[k]);
    let n0 = (y1 - y0).cross(y2 - y0);
    let n0 = if n0.length_squared() > 1e-24 { n0 } else { n };
    if n.dot(n0) < 0.0 {
        n = -n;
    }
    if n0.dot(mid0 - y0) < 0.0 {
        n = -n;
    }
    // Clip the axis (at the end pose) to the triangle's prism.
    let (a, e) = (pose.a1, pose.e1);
    let (mut t0, mut t1) = (0.0_f32, 1.0_f32);
    for (p, q, other) in [(x0, x1, x2), (x1, x2, x0), (x2, x0, x1)] {
        let mut m = (q - p).cross(n);
        if m.dot(other - p) < 0.0 {
            m = -m;
        }
        let (f0, slope) = (m.dot(a - p), m.dot(e));
        if slope.abs() < 1e-12 {
            if f0 < 0.0 {
                return;
            }
        } else {
            let t = -f0 / slope;
            if slope > 0.0 { t0 = t0.max(t) } else { t1 = t1.min(t) }
        }
        if t0 > t1 {
            return;
        }
    }
    // The axis's lowest point over the triangle, measured on the collider's side.
    let height = |t: f32| n.dot(a + e * t - x0);
    let t = if height(t0) <= height(t1) { t0 } else { t1 };
    let depth = (pose.reach - height(t)).min(most);
    if depth <= 0.0 {
        return;
    }
    // Barycentric weights of the point under the axis.
    let q = a + e * t;
    let q = q - n * n.dot(q - x0);
    let (v0, v1, v2) = (x1 - x0, x2 - x0, q - x0);
    let (d00, d01, d11, d20, d21) = (v0.dot(v0), v0.dot(v1), v1.dot(v1), v2.dot(v0), v2.dot(v1));
    let denom = d00 * d11 - d01 * d01;
    if denom.abs() < 1e-20 {
        return;
    }
    let bj = ((d11 * d20 - d01 * d21) / denom).clamp(0.0, 1.0);
    let bk = ((d00 * d21 - d01 * d20) / denom).clamp(0.0, 1.0);
    let bi = (1.0 - bj - bk).max(0.0);
    let weights = [(i, bi), (j, bj), (k, bk)];
    let sum: f32 = weights.iter().map(|&(p, b)| inv_mass[p] * b * b).sum();
    if sum < 1e-12 {
        return;
    }
    for (p, b) in weights {
        positions[p] -= n * (depth * inv_mass[p] * b / sum);
    }
}

/// Wind: a steady velocity and a deterministic gust, and the aerodynamic coefficients of the cloth.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct WindParams {
    /// The air's steady velocity in the solver's space, m/s.
    pub velocity: Vec3,
    /// Pressure drag coefficient (kg/m³; about 0.6 for a flat plate in air at 1.2 kg/m³).
    pub drag: f32,
    /// Lift coefficient, the same units; 0 turns lift off.
    pub lift: f32,
    /// Gust amplitude as a share of `velocity` (0 for a steady wind).
    pub gust: f32,
    /// Gust frequency, Hz.
    pub gust_hz: f32,
    /// Gust phase per metre along the wind (rad/m): gusts travel through the cloth rather than shaking it as a whole (sampled once per row of cells).
    pub gust_phase_per_m: f32,
    /// How often the wind is applied, Hz (default 120, the physics step): once every `1 / (rate · h)` sub-steps (at
    /// least one), with a push that long. Wind changes over tenths of a second, so a push per physics step is
    /// indistinguishable from one per sub-step (a gusty 0.5 m flag stays within 10 mm over 8 s, measured 6.9) and costs a quarter
    /// as much at the default 4 sub-steps. Set it to the sub-step rate (480) for the exact old behaviour; it is
    /// sampled from the shot clock, so replays stay bit-exact at any rate.
    pub rate_hz: f32,
}

impl Default for WindParams {
    /// Still air, with air's coefficients and a gentle 0.3 Hz gust of 30% once `velocity` is set.
    fn default() -> Self {
        Self {
            velocity: Vec3::ZERO,
            drag: 0.6,
            lift: 0.3,
            gust: 0.3,
            gust_hz: 0.3,
            gust_phase_per_m: 3.0,
            rate_hz: 120.0,
        }
    }
}

impl WindParams {
    /// Checks every field is in range.
    ///
    /// # Errors
    /// [`ContactError::Param`] naming the first field out of range.
    pub fn validate(&self) -> Result<(), ContactError> {
        if !self.velocity.is_finite() {
            return Err(ContactError::Param { name: "velocity", value: f32::NAN, expected: "finite" });
        }
        for (name, value) in [
            ("drag", self.drag),
            ("lift", self.lift),
            ("gust", self.gust),
            ("gust_hz", self.gust_hz),
            ("gust_phase_per_m", self.gust_phase_per_m),
            ("rate_hz", self.rate_hz),
        ] {
            if !(value.is_finite() && value >= 0.0) {
                return Err(ContactError::Param { name, value, expected: "finite and >= 0" });
            }
        }
        Ok(())
    }

    /// The force on a triangle of `area` and unit normal `n` in air moving at `u` relative to it: pressure drag
    /// along the normal and lift perpendicular to the air, on the side the air pushes.
    fn force(&self, u: Vec3A, n: Vec3A, area: f32) -> Vec3A {
        let un = u.dot(n);
        let mut force = n * (self.drag * area * un * un.abs());
        let speed = u.length();
        if self.lift > 0.0 && speed > 1e-6 {
            let pushed = n * un.signum();
            force += (pushed * (speed * speed) - u * un.abs()) * (self.lift * area * un.abs() / speed);
        }
        force
    }

    /// The air's velocity at `position` when the shot clock reads `seconds`: a pure function of both.
    #[allow(clippy::cast_possible_truncation)] // a phase in [-1, 1]
    fn air(&self, seconds: f64, position: Vec3A) -> Vec3A {
        let wind = Vec3A::from(self.velocity);
        if self.gust == 0.0 {
            return wind;
        }
        let along = f64::from(position.dot(wind.normalize_or_zero()) * self.gust_phase_per_m);
        let phase = std::f64::consts::TAU * f64::from(self.gust_hz) * seconds - along;
        // Two incommensurate sines, so the gust never repeats on a visible period.
        let wave = 0.6 * libm::sin(phase) + 0.4 * libm::sin(phase * 2.17 + 1.3);
        wind * (1.0 + self.gust * wave as f32)
    }
}

/// Wind on a sheet: the parameters and the sheet's triangles.
#[derive(Clone, Debug)]
pub struct Wind {
    /// The coefficients and the field; edit between frames.
    pub params: WindParams,
    triangles: Vec<[u32; 3]>,
    /// Triangles in a row of cells; the gust is evaluated once per row (a sine per triangle would cost as much as
    /// the rest of the wind).
    row_len: usize,
}

impl Wind {
    /// Wind over a sheet of `shape` (two triangles per cell, around the seam if wrapped).
    ///
    /// # Errors
    /// [`ContactError::Param`] if `params` fail [`WindParams::validate`].
    #[allow(clippy::cast_possible_truncation)] // the sheet checks its particle count fits in u32
    pub fn new(shape: SheetShape, params: WindParams) -> Result<Self, ContactError> {
        params.validate()?;
        let cell_cols = if shape.wrap { shape.cols } else { shape.cols.saturating_sub(1) };
        let mut triangles = Vec::with_capacity(2 * cell_cols * shape.rows.saturating_sub(1));
        for r in 0..shape.rows.saturating_sub(1) {
            for c in 0..cell_cols {
                let at = |r: usize, c: usize| (r * shape.cols + c % shape.cols) as u32;
                let (p00, p01, p10, p11) = (at(r, c), at(r, c + 1), at(r + 1, c), at(r + 1, c + 1));
                triangles.push([p00, p10, p01]);
                triangles.push([p01, p10, p11]);
            }
        }
        Ok(Self { params, triangles, row_len: (2 * cell_cols).max(1) })
    }

    /// Adds one sub-step of wind to `velocities`.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a clock; a stride
    fn accelerate(&self, substep: &Substep, x: &[Vec3A], v: &mut [Vec3A], inv_mass: &[f32]) {
        // Every `stride`-th sub-step, from the first, with the push of all of them: a function of the clock alone.
        let stride = (1.0 / (self.params.rate_hz.max(1e-3) * substep.h)).round().clamp(1.0, 1e6) as u64;
        if !substep.index.is_multiple_of(stride) {
            return;
        }
        let dt = substep.h * stride as f32;
        let seconds = substep.index as f64 * f64::from(substep.h);
        let p = &self.params;
        for row in self.triangles.chunks(self.row_len) {
            let air = p.air(seconds, x[row[0][0] as usize]);
            for &[i, j, k] in row {
                let (i, j, k) = (i as usize, j as usize, k as usize);
                let normal_area = (x[j] - x[i]).cross(x[k] - x[i]) * 0.5;
                let area = normal_area.length();
                if area < 1e-12 {
                    continue;
                }
                let dv = p.force(air - (v[i] + v[j] + v[k]) / 3.0, normal_area / area, area) * (dt / 3.0);
                for m in [i, j, k] {
                    v[m] += dv * inv_mass[m];
                }
            }
        }
    }
}

/// Everything around the cloth in a step: colliders with friction, and wind. Pass `&mut` of it to
/// [`ClothSolver::step_with`](super::solver::ClothSolver::step_with). Per frame call
/// [`begin_frame`](Self::begin_frame), then [`place`](Self::place) each collider's pose at the frame's end.
#[derive(Clone, Debug)]
pub struct ClothEnvironment {
    params: ContactParams,
    slots: Vec<Slot>,
    poses: Vec<Pose>,
    count: usize,
    last_fraction: f32,
    wind: Option<Wind>,
    /// The sheet's grid and longest rest edge, for the triangle contacts of thin colliders.
    sheet: Option<SheetShape>,
    spacing: f32,
}

impl ClothEnvironment {
    /// An environment for up to `capacity` colliders, and no wind.
    ///
    /// # Errors
    /// [`ContactError::Param`] if `params` fail [`ContactParams::validate`].
    pub fn new(capacity: usize, params: ContactParams) -> Result<Self, ContactError> {
        params.validate()?;
        let blank = Shape::sphere(Vec3::ZERO, 1.0);
        Ok(Self {
            params,
            slots: vec![Slot { from: blank, to: blank }; capacity],
            poses: vec![Pose::default(); capacity],
            count: 0,
            last_fraction: 0.0,
            wind: None,
            sheet: None,
            spacing: 0.0,
        })
    }

    /// Sets or removes the wind.
    pub fn set_wind(&mut self, wind: Option<Wind>) {
        self.wind = wind;
    }

    /// The wind, to edit its parameters between frames.
    pub fn wind_mut(&mut self) -> Option<&mut Wind> {
        self.wind.as_mut()
    }

    /// Starts a frame: every collider's last pose becomes its start pose. Call before each
    /// [`ClothSolver::step_with`](super::solver::ClothSolver::step_with).
    pub fn begin_frame(&mut self) {
        for slot in &mut self.slots[..self.count] {
            slot.from = slot.to;
        }
        self.last_fraction = 0.0;
    }

    /// Sets collider `index`'s pose at the frame's end. A collider placed for the first time starts there. Slots are
    /// filled in order: placing `index == len()` adds one.
    ///
    /// # Errors
    /// [`ContactError::BadShape`] for a shape that is not finite or has no radius; [`ContactError::Capacity`] beyond
    /// the capacity. Nothing changes on an error.
    pub fn place(&mut self, index: usize, shape: Shape) -> Result<(), ContactError> {
        if !shape.is_valid() {
            return Err(ContactError::BadShape { index });
        }
        if index >= self.slots.len() || index > self.count {
            return Err(ContactError::Capacity { index, capacity: self.slots.len() });
        }
        if index == self.count {
            self.slots[index].from = shape;
            self.count += 1;
        }
        self.slots[index].to = shape;
        Ok(())
    }

    /// Tells the environment the sheet's grid and its longest rest edge (metres), which turns on the triangle contacts
    /// for colliders thinner than that edge (see the [module docs](self)); `None` turns them off. Allocates nothing, so
    /// a runtime switching levels of detail calls it at the switch.
    pub fn set_sheet(&mut self, shape: Option<SheetShape>, spacing: f32) {
        self.sheet = shape.filter(|s| s.rows >= 2 && s.cols >= 2 && spacing.is_finite() && spacing > 0.0);
        self.spacing = if self.sheet.is_some() { spacing } else { 0.0 };
    }

    /// Removes the colliders from `len` on (so `truncate(0)` removes all).
    pub fn truncate(&mut self, len: usize) {
        self.count = self.count.min(len);
    }

    /// How many colliders are placed.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether no collider is placed.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

impl SubstepHook for ClothEnvironment {
    fn accelerate(&mut self, substep: &Substep, positions: &[Vec3A], velocities: &mut [Vec3A], inv_mass: &[f32]) {
        if let Some(wind) = &self.wind {
            wind.accelerate(substep, positions, velocities, inv_mass);
        }
    }

    fn project(&mut self, substep: &Substep, positions: &mut [Vec3A], previous: &[Vec3A], inv_mass: &[f32]) {
        let f1 = substep.fraction;
        let f0 = self.last_fraction.min(f1);
        self.last_fraction = f1;
        let clearance = self.params.thickness + self.params.margin;
        for (pose, slot) in self.poses.iter_mut().zip(&self.slots[..self.count]) {
            *pose = Pose::new(&slot.from, &slot.to, f0, f1, clearance);
        }
        let poses = &self.poses[..self.count];
        let friction = self.params.friction;
        // Consecutive particles are neighbours on the sheet (a row, then the next), so a sphere around each chunk's
        // paths rejects a whole chunk against a collider with one test; only chunks that reach a collider pay per
        // particle. Each particle still meets the colliders in order, so the result is the same as testing them one by
        // one.
        for ((xs, x0s), ws) in positions.chunks_mut(CHUNK).zip(previous.chunks(CHUNK)).zip(inv_mass.chunks(CHUNK)) {
            let mut paths = [(Vec3A::ZERO, 0.0_f32); CHUNK];
            let mut sum = Vec3A::ZERO;
            for ((path, &x), &x0) in paths.iter_mut().zip(xs.iter()).zip(x0s) {
                *path = ((x0 + x) * 0.5, (x - x0).length() * 0.5);
                sum += path.0;
            }
            #[allow(clippy::cast_precision_loss)] // at most CHUNK
            let centre = sum / xs.len() as f32;
            let radius =
                paths.iter().take(xs.len()).map(|&(mid, half)| mid.distance(centre) + half).fold(0.0, f32::max);
            let mut hits = [0_u8; CHUNK];
            for pose in poses {
                if centre.distance(pose.centre) > pose.bound + radius {
                    continue;
                }
                for ((((x, &x0), &w), &(mid, half)), hit) in
                    xs.iter_mut().zip(x0s).zip(ws).zip(&paths).zip(hits.iter_mut())
                {
                    // Outside the sphere around the swept volume, widened by the particle's own path: no contact.
                    let near = pose.bound + half;
                    if w > 0.0 && mid.distance_squared(pose.centre) <= near * near {
                        let moved = contact(pose, friction, x0, *x, 2.0 * half);
                        if moved != *x {
                            *hit = hit.saturating_add(1);
                            *x = moved;
                        }
                    }
                }
            }
            // Limit 2: a particle that met two or more colliders may have been pushed from one into another.
            for (x, &hit) in xs.iter_mut().zip(&hits) {
                if hit >= 2 {
                    *x = resolve_overlap(poses, *x);
                }
            }
        }
        // Limit 1: colliders thinner than the particle spacing also meet the sheet's triangles.
        if let Some(shape) = self.sheet {
            for pose in poses {
                if 2.0 * pose.reach < self.spacing {
                    triangle_contacts(pose, shape, self.spacing, positions, previous, inv_mass);
                }
            }
        }
    }
}
