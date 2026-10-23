//! Eyes first, then the head (#174): a look at a new target turns the eyes at once and the head 120 to 200 ms later,
//! and the head only as far as the eyes can't reach from rest. A small look (inside the eyes' reach) moves only the
//! eyes; the head stays inside a comfort zone and the eyes take the rest.
//!
//! The head's part is a turn on top of whatever the bus's head already does (rest, a score's nods), eased with a
//! critically damped spring so it never overshoots. A person's tracked head is never turned: the eyes do all of the
//! look then. Large shifts are reported so the life layer can blink with them, through [`Cues`], instead of this
//! module keeping blink logic of its own.

use omuzaki_anim_api::{Bus, Channel, ChannelMask, Cues, Source, Span, Tick};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::rng::Rng;

use super::solve::{GazeFrame, GazeSolver};
use super::target::{GazeScene, GazeTarget};

/// A head goal closer than this to the last one (radians, about half a degree) is the same goal: tracking noise in a
/// moving target does not restart the head's wait.
const SAME_GOAL_RAD: f32 = 0.009;
/// The head has arrived when it is this close to its goal (radians) and this slow (radians per second).
const SETTLED_RAD: f32 = 0.002;
const SETTLED_RAD_S: f32 = 0.02;
/// A target is only turned toward while it is in front of the character: `z / |v|` above this (about 75 degrees off
/// axis), as for the eyes.
const FRONT_CONE: f32 = 0.25;

/// How the head shares a look with the eyes.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct HeadShare {
    /// How far the eyes turn before the head helps, radians (15 degrees).
    pub eye_reach_rad: f32,
    /// The comfort zone: the most the head turns sideways for a look, radians. The eyes take the rest.
    pub max_yaw_rad: f32,
    /// The same, up and down.
    pub max_pitch_rad: f32,
    /// How long the head waits after the eyes start, seconds.
    pub delay_s: Span,
    /// The head's rate of approach, 1/s. Critically damped: from rest it covers 96% of a turn in `4.7 / rate`.
    pub rate: f32,
    /// A look shift at least this large, radians, may earn a blink.
    pub blink_shift_rad: f32,
}

impl Default for HeadShare {
    fn default() -> Self {
        Self {
            eye_reach_rad: 15_f32.to_radians(),
            max_yaw_rad: 45_f32.to_radians(),
            max_pitch_rad: 25_f32.to_radians(),
            delay_s: Span::new(0.12, 0.20),
            rate: 16.0,
            blink_shift_rad: 20_f32.to_radians(),
        }
    }
}

/// One frame of a coordinated look.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct GazeTurn {
    /// The eyes, as the solver wrote them.
    pub eyes: GazeFrame,
    /// The head's turn for the look (yaw toward the character's left, pitch down), radians, on top of the bus's head.
    pub head: [f32; 2],
    /// True while the head is waiting for the eyes or turning after them.
    pub leading: bool,
    /// True on the frame a look shifts by at least [`HeadShare::blink_shift_rad`].
    pub shifted: bool,
}

impl GazeTurn {
    /// Tells the life layer about this turn: it doesn't lead it again, and may blink with a large shift.
    pub fn apply(&self, cues: &mut Cues) {
        cues.eyes_lead_turn = self.leading;
        cues.look_shift = self.shifted;
    }
}

/// A look from a cue, held until a time or the next look.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Held {
    target: GazeTarget,
    until: Option<f64>,
}

/// Aims the eyes and turns the head for a gaze target, eyes first. Run it before the life layer, in place of a bare
/// [`GazeSolver`]. No allocation; deterministic for a seed.
#[derive(Clone, Debug)]
pub struct GazeCoordinator {
    solver: GazeSolver,
    share: HeadShare,
    rng: Rng,
    clock: f64,
    turn: [f32; 2],
    velocity: [f32; 2],
    goal: [f32; 2],
    /// A goal the head starts toward at the given time.
    pending: Option<([f32; 2], f64)>,
    /// The direction looked at on the last frame (yaw, pitch down), radians from straight ahead.
    last: [f32; 2],
    held: Option<Held>,
}

impl GazeCoordinator {
    /// A coordinator with these eyes, this share, and a seed for the head's waits.
    pub fn new(solver: GazeSolver, share: HeadShare, seed: u64) -> Self {
        Self {
            solver,
            share,
            rng: Rng::new(seed),
            clock: 0.0,
            turn: [0.0; 2],
            velocity: [0.0; 2],
            goal: [0.0; 2],
            pending: None,
            last: [0.0; 2],
            held: None,
        }
    }

    /// The share it works with.
    pub fn share(&self) -> &HeadShare {
        &self.share
    }

    /// Looks at `target` for `hold_s` seconds from now (a `Look` cue), then goes back to the target
    /// [`step`](Self::step) is given. A `hold_s` of zero, or one that isn't finite, holds until the next look.
    pub fn look(&mut self, target: GazeTarget, hold_s: f32) {
        let until = (hold_s.is_finite() && hold_s > 0.0).then(|| self.clock + f64::from(hold_s));
        self.held = Some(Held { target, until });
    }

    /// One frame. `body_frame` is the eye frame with the head at rest (origin between the pupils, `+z` forward, `+x`
    /// toward the character's left, `+y` up), in the world; `target` is the character's own gaze target, which a held
    /// [`look`](Self::look) overrides. Writes the head turn onto `headYaw` and `headPitch` (claimed for
    /// [`Source::Score`]) unless a person drives the head, then the eyes through the solver, aimed from the turned head.
    /// The head turns about the eyes' origin, which is close enough to the neck for the split of a look.
    pub fn step(
        &mut self,
        tick: Tick,
        body_frame: Mat4,
        target: GazeTarget,
        scene: &impl GazeScene,
        bus: &mut Bus,
    ) -> GazeTurn {
        let dt = if tick.dt.is_finite() { tick.dt.max(0.0) } else { 0.0 };
        self.clock += f64::from(dt);
        if self.held.is_some_and(|h| h.until.is_some_and(|until| self.clock >= until)) {
            self.held = None;
        }
        let resolved = self.held.map_or(target, |h| h.target).resolve(scene);
        let direction = resolved.and_then(|r| direction(body_frame.inverse().transform_point3(r.position)));
        let head_channels = ChannelMask::of(Channel::HEAD_YAW) | ChannelMask::of(Channel::HEAD_PITCH);
        let person = bus.owners.human_owns_any(head_channels);
        let base = [bus.values[Channel::HEAD_YAW], bus.values[Channel::HEAD_PITCH]];

        let looked = direction.unwrap_or_default();
        let shifted = (looked[0] - self.last[0]).hypot(looked[1] - self.last[1]) >= self.share.blink_shift_rad;
        self.last = looked;

        if person {
            (self.turn, self.velocity, self.goal, self.pending) = ([0.0; 2], [0.0; 2], [0.0; 2], None);
        } else {
            let wanted = direction.map_or([0.0; 2], |d| self.head_goal(d, base));
            self.schedule(wanted);
            self.spring(dt);
            if self.pending.is_some() || self.turn.iter().chain(&self.goal).any(|v| v.abs() > 0.0) {
                bus.values[Channel::HEAD_YAW] = base[0] + self.turn[0];
                bus.values[Channel::HEAD_PITCH] = base[1] + self.turn[1];
                bus.owners.claim(Source::Score, head_channels);
            }
        }
        let head = [bus.values[Channel::HEAD_YAW], bus.values[Channel::HEAD_PITCH]];
        let eye_frame = body_frame * Mat4::from_rotation_y(head[0]) * Mat4::from_rotation_x(head[1]);
        let eyes = self.solver.step(tick, eye_frame, resolved, bus);
        GazeTurn { eyes, head: self.turn, leading: self.pending.is_some() || !self.settled(), shifted }
    }

    /// The head turn a look in `direction` wants, given the head's own pose `base`: the least turn from rest that
    /// brings the target inside the eyes' reach, kept inside the comfort zone. A look the eyes can reach from rest
    /// brings the head home.
    fn head_goal(&self, direction: [f32; 2], base: [f32; 2]) -> [f32; 2] {
        let limits = [self.share.max_yaw_rad, self.share.max_pitch_rad];
        let reach = self.share.eye_reach_rad;
        [0, 1].map(|axis| {
            let needed = direction[axis] - base[axis];
            (needed - needed.clamp(-reach, reach)).clamp(-limits[axis], limits[axis])
        })
    }

    /// Sets the head off toward `wanted`: after the eyes' lead when the head is still, at once when it is already on
    /// its way (a target that keeps moving is followed, not waited for again).
    fn schedule(&mut self, wanted: [f32; 2]) {
        let latest = self.pending.map_or(self.goal, |(goal, _)| goal);
        let moved = (wanted[0] - latest[0]).abs() > SAME_GOAL_RAD || (wanted[1] - latest[1]).abs() > SAME_GOAL_RAD;
        if moved {
            if let Some((goal, _)) = self.pending.as_mut() {
                *goal = wanted;
            } else if self.settled() {
                let delay = self.share.delay_s.sample(self.rng.next_f32());
                self.pending = Some((wanted, self.clock + f64::from(delay)));
            } else {
                self.goal = wanted;
            }
        }
        if let Some((goal, at)) = self.pending
            && self.clock >= at
        {
            self.goal = goal;
            self.pending = None;
        }
    }

    /// Moves the head along its critically damped spring for `dt` (the exact solution, so any step is stable).
    fn spring(&mut self, dt: f32) {
        let rate = if self.share.rate.is_finite() { self.share.rate.max(0.0) } else { 0.0 };
        let decay = (-rate * dt).exp();
        for axis in 0..2 {
            let (x, v) = (self.turn[axis] - self.goal[axis], self.velocity[axis]);
            let k = (v + rate * x) * dt;
            self.turn[axis] = self.goal[axis] + (x + k) * decay;
            self.velocity[axis] = (v - rate * k) * decay;
        }
        if self.settled() {
            (self.turn, self.velocity) = (self.goal, [0.0; 2]);
        }
    }

    fn settled(&self) -> bool {
        (0..2).all(|axis| {
            (self.turn[axis] - self.goal[axis]).abs() < SETTLED_RAD && self.velocity[axis].abs() < SETTLED_RAD_S
        })
    }
}

/// The direction to `v` (eye-frame space) as yaw toward the character's left and pitch down, radians; `None` for a
/// point behind the character or not a finite point.
fn direction(v: Vec3) -> Option<[f32; 2]> {
    let length = v.length();
    (length > 0.0 && length.is_finite() && v.z / length > FRONT_CONE)
        .then(|| [v.x.atan2(v.z), -(v.y / length).clamp(-1.0, 1.0).asin()])
}
