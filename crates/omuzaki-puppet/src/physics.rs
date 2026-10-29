//! Pendulum physics (#1363): each pendulum hangs from its anchor node, lags behind it as the puppet moves, and writes
//! its swing to a parameter.
//!
//! **Rigid** (a rod of length `L`): with `g = gravity * world.gravity * world.pixels_per_meter`,
//! `θ'' = -(g / L) sin θ - 2 ζ sqrt(g / L) θ'`, where `θ` is measured from straight down and `ζ` is
//! `angle_damping` (1 is critical: no overshoot). Small swings have the period `2π sqrt(L / g)`.
//!
//! **Spring** (natural frequency `f`, stiffness `k = (2π f)²`): the bob moves in the plane under gravity and the
//! spring's pull toward its rest length `L - g / k`, with the angular and radial parts of its velocity damped by
//! `angle_damping` and `length_damping` (critical factors `2 sqrt(g / L)` and `2 sqrt(k)`).
//!
//! Both integrate with fourth-order Runge-Kutta in fixed steps of [`STEP`] seconds; time left over carries to the next
//! call, so the same inputs at any frame rate give the same motion at the step boundaries. Model space is y down, so
//! gravity is `+y`.
//!
//! **Output.** The bob, relative to the anchor and turned into the anchor's frame, becomes the parameter: for
//! `AngleLength`, x is the angle from straight down over π (`-1..1`) and y the length over `L`; `Xy` gives the bob's
//! offset from rest in lengths, y up. Each is scaled by `output_scale`.
#![allow(clippy::many_single_char_names, reason = "geometry and sampling read best in their formulas' own letters")]

use std::f32::consts::PI;

use omuzaki_core::glam::Vec2;
use omuzaki_puppet_api::{MapMode, ParamIx, Pendulum, PendulumModel, PhysicsWorld};

use crate::eval::Evaluator;

/// The fixed physics step, seconds (240 Hz).
pub const STEP: f32 = 1.0 / 240.0;
/// The longest gap one call simulates; a longer one (a stall) is cut so the puppet doesn't run away.
pub const MAX_GAP: f32 = 0.25;

#[derive(Copy, Clone, Debug, Default)]
struct State {
    started: bool,
    bob: Vec2,
    /// Rigid: angle and angular velocity.
    theta: f32,
    omega: f32,
    /// Spring: bob velocity.
    vel: Vec2,
}

/// The pendulums of one puppet.
#[derive(Clone, Debug)]
pub struct Physics {
    states: Vec<State>,
    carry: f32,
}

fn gravity(world: PhysicsWorld, p: &Pendulum) -> f32 {
    p.gravity * world.gravity * world.pixels_per_meter
}

fn rigid_accel(p: &Pendulum, g: f32, theta: f32, omega: f32) -> f32 {
    let w2 = g / p.length;
    -w2 * theta.sin() - omega * p.angle_damping * 2.0 * w2.abs().sqrt()
}

fn spring_accel(p: &Pendulum, g: f32, anchor: Vec2, bob: Vec2, vel: Vec2) -> Vec2 {
    let ks = p.frequency * 2.0 * PI;
    let k = ks * ks;
    let rest = p.length - g / k;
    let off = bob - anchor;
    let dist = off.length();
    let n = if dist > 1e-6 { off / dist } else { Vec2::Y };
    let force = Vec2::new(0.0, g) - n * (dist - rest) * k;
    // The velocity in the pendulum's frame: x across the rod, y along it.
    let across = Vec2::new(n.y, -n.x);
    let (v_across, v_along) = (vel.dot(across), vel.dot(n));
    let crit_angle = 2.0 * (g / p.length).abs().sqrt();
    let crit_length = 2.0 * ks;
    let damp = -across * v_across * p.angle_damping * crit_angle - n * v_along * p.length_damping * crit_length;
    force + damp
}

impl Physics {
    /// Pendulums at rest, for `count` pendulums (the puppet's [`omuzaki_puppet_api::PuppetDef::physics`]).
    #[must_use]
    pub fn new(count: usize) -> Self {
        Self { states: vec![State::default(); count], carry: 0.0 }
    }

    /// Lets the pendulums hang at rest under their anchors again.
    pub fn reset(&mut self) {
        self.states.iter_mut().for_each(|s| *s = State::default());
        self.carry = 0.0;
    }

    /// Advances `dt` seconds with the anchors where `eval` last posed them, and writes each pendulum's parameter.
    /// Evaluate again afterwards to see the swing.
    pub fn step(&mut self, eval: &mut Evaluator, dt: f32) {
        let world = eval.def().world;
        let mut time = self.carry + dt.clamp(0.0, MAX_GAP);
        let mut steps = 0u32;
        while time >= STEP {
            time -= STEP;
            steps += 1;
        }
        self.carry = time;
        let count = self.states.len().min(eval.def().physics.len());
        for i in 0..count {
            let (param, out) = self.advance(eval, world, i, steps);
            eval.set(param, out);
        }
    }

    fn advance(&mut self, eval: &Evaluator, world: PhysicsWorld, i: usize, steps: u32) -> (ParamIx, [f32; 2]) {
        {
            let p = &eval.def().physics[i];
            let anchor = eval.to_model(p.anchor, Vec2::ZERO);
            let frame = eval.frame(p.anchor);
            let g = gravity(world, p);
            let s = &mut self.states[i];
            if !s.started {
                *s = State { started: true, bob: anchor + Vec2::new(0.0, p.length), ..State::default() };
            }
            for _ in 0..steps {
                match p.model {
                    PendulumModel::Rigid => {
                        let d = s.bob - anchor;
                        s.theta = (-d.x).atan2(d.y);
                        let (t0, w0) = (s.theta, s.omega);
                        let f = |t: f32, w: f32| (w, rigid_accel(p, g, t, w));
                        let (k1t, k1w) = f(t0, w0);
                        let (k2t, k2w) = f(t0 + STEP * 0.5 * k1t, w0 + STEP * 0.5 * k1w);
                        let (k3t, k3w) = f(t0 + STEP * 0.5 * k2t, w0 + STEP * 0.5 * k2w);
                        let (k4t, k4w) = f(t0 + STEP * k3t, w0 + STEP * k3w);
                        let t1 = t0 + STEP / 6.0 * (k1t + 2.0 * k2t + 2.0 * k3t + k4t);
                        let w1 = w0 + STEP / 6.0 * (k1w + 2.0 * k2w + 2.0 * k3w + k4w);
                        if t1.is_finite() && w1.is_finite() {
                            s.theta = t1;
                            s.omega = w1;
                        }
                        s.bob = anchor + Vec2::new(-s.theta.sin(), s.theta.cos()) * p.length;
                    }
                    PendulumModel::Spring => {
                        let (x0, v0) = (s.bob, s.vel);
                        let f = |x: Vec2, v: Vec2| (v, spring_accel(p, g, anchor, x, v));
                        let (k1x, k1v) = f(x0, v0);
                        let (k2x, k2v) = f(x0 + k1x * (STEP * 0.5), v0 + k1v * (STEP * 0.5));
                        let (k3x, k3v) = f(x0 + k2x * (STEP * 0.5), v0 + k2v * (STEP * 0.5));
                        let (k4x, k4v) = f(x0 + k3x * STEP, v0 + k3v * STEP);
                        let x1 = x0 + (k1x + k2x * 2.0 + k3x * 2.0 + k4x) * (STEP / 6.0);
                        let v1 = v0 + (k1v + k2v * 2.0 + k3v * 2.0 + k4v) * (STEP / 6.0);
                        if x1.is_finite() && v1.is_finite() {
                            s.bob = x1;
                            s.vel = v1;
                        }
                    }
                }
            }
            let rel = s.bob - anchor;
            let local = frame.inverse().transform_vector2(rel);
            let dir = local.normalize_or(Vec2::Y);
            let length = rel.length() / p.length;
            let angle = (-dir.x).atan2(dir.y) / PI;
            let xy = {
                let v = dir * length - Vec2::Y;
                Vec2::new(v.x, -v.y)
            };
            let out = match p.map {
                MapMode::AngleLength => Vec2::new(angle, length),
                MapMode::LengthAngle => Vec2::new(length, angle),
                MapMode::Xy => xy,
                MapMode::Yx => Vec2::new(xy.y, xy.x),
            } * Vec2::from(p.output_scale);
            (p.param, out.to_array())
        }
    }

    /// The bob of pendulum `i`, model space (for tests and overlays).
    #[must_use]
    pub fn bob(&self, i: usize) -> Option<Vec2> {
        self.states.get(i).filter(|s| s.started).map(|s| s.bob)
    }
}
