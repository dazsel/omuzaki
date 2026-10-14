//! The eyes lead a fast head turn: they move first, the head follows 120 to 200 ms later.
//!
//! The source head's speed is watched; when it passes the threshold the head target is held where it was while the
//! gaze jumps toward the new direction, then the head is let go and catches up. Re-arming needs a quarter second of
//! calm, so one sweep is one lead.

use omuzaki_anim_api::LOOK_FULL_RAD;
use omuzaki_core::rng::Rng;

use super::frame::Frame;

/// The speed estimate follows the raw speed with this time constant, so a 30 fps tracker feeding a 60 fps loop does
/// not flicker around the threshold.
const SPEED_TAU_S: f32 = 0.05;
/// This long below the threshold re-arms the trigger.
const CALM_S: f32 = 0.25;
/// A lead ends once the head is within this of its target (about 1.5 degrees), or after [`MAX_LEAD_S`].
const SETTLED_RAD: f32 = 0.026_2;
const MAX_LEAD_S: f64 = 1.5;
/// The fastest raw head speed the estimate is allowed (rad/s, far above any real head). It keeps a tiny but normal
/// `dt` from overflowing the raw speed to infinity, where `inf * 0` would leave the estimate NaN for good.
const MAX_RAW_SPEED: f32 = 1.0e4;

/// A head pose in the source, radians.
pub(super) type Pose = [f32; 3];

/// What the lead asks of the rest of the frame.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(super) struct LeadOut {
    /// Where the head target is held while the eyes get ahead, if it is.
    pub hold: Option<Pose>,
    /// The gaze offset toward the head's destination, look units.
    pub gaze: (f32, f32),
    /// True while a lead is under way (a glance waits for it).
    pub active: bool,
    /// True on the frame a fast turn starts, which may earn a blink.
    pub started: bool,
}

/// The state of the eyes-lead logic.
#[derive(Clone, Debug)]
pub(super) struct Lead {
    previous: Option<Pose>,
    speed: f32,
    calm_for: f32,
    armed: bool,
    active: bool,
    began: f64,
    hold_until: f64,
    hold: Pose,
}

impl Lead {
    pub fn new() -> Self {
        Self {
            previous: None,
            speed: 0.0,
            calm_for: 1.0,
            armed: true,
            active: false,
            began: 0.0,
            hold_until: 0.0,
            hold: [0.0; 3],
        }
    }

    /// Forgets everything: a person drives the head, or the behaviour is off.
    fn reset(&mut self) {
        self.previous = None;
        self.speed = 0.0;
        self.active = false;
        self.armed = true;
        self.calm_for = 1.0;
    }

    /// `source` is the incoming head pose; `rendered` is where the head springs are now (yaw, pitch); `rng` is the
    /// head stream (how long the head is held back).
    pub fn step(&mut self, frame: &Frame<'_>, source: Pose, rendered: (f32, f32), rng: &mut Rng) -> LeadOut {
        if !frame.gate.head {
            self.reset();
            return LeadOut::default();
        }
        let params = &frame.params.gaze;
        let mut started = false;
        // Only a normal positive dt measures a speed: a subnormal one divides the snap into an overflow. The pose is
        // still remembered below, so the next timed frame measures from here.
        let timed = frame.dt.is_normal() && frame.dt > 0.0;
        if let Some(prev) = self.previous.filter(|_| timed) {
            let raw = ((source[0] - prev[0]).hypot(source[1] - prev[1]) / frame.dt).min(MAX_RAW_SPEED);
            self.speed += (raw - self.speed) * (1.0 - (-frame.dt / SPEED_TAU_S).exp());
            if self.speed >= params.lead_speed_rad_s {
                if frame.systems.gaze_lead && self.armed && !self.active {
                    started = true;
                    self.begin(frame, prev, rng);
                }
                self.calm_for = 0.0;
            } else {
                self.calm_for += frame.dt;
                if self.calm_for >= CALM_S {
                    self.armed = true;
                }
            }
        }
        self.previous = Some(source);
        if !self.active {
            return LeadOut { started, ..LeadOut::default() };
        }
        let (yaw_error, pitch_error) = (source[0] - rendered.0, source[1] - rendered.1);
        let hold = (frame.clock < self.hold_until).then_some(self.hold);
        // One look unit is LOOK_FULL_RAD of eye rotation; a positive yaw looks toward the character's left, a
        // positive pitch (nodding down) looks down.
        let reach = |angle: f32| (angle / LOOK_FULL_RAD).clamp(-params.lead_max, params.lead_max);
        let gaze = (reach(yaw_error), reach(-pitch_error));
        let arrived =
            frame.clock >= self.hold_until && yaw_error.abs() < SETTLED_RAD && pitch_error.abs() < SETTLED_RAD;
        if arrived || frame.clock - self.began > MAX_LEAD_S {
            self.active = false;
        }
        LeadOut { hold, gaze, active: true, started }
    }

    /// Starts a lead from the pose the head was in a frame ago. The head is held only when the gaze is free to lead;
    /// if a person drives the eyes there is nothing for the head to wait for.
    fn begin(&mut self, frame: &Frame<'_>, from: Pose, rng: &mut Rng) {
        self.armed = false;
        self.active = true;
        self.began = frame.clock;
        self.hold = from;
        self.hold_until = if frame.gate.gaze {
            frame.clock + f64::from(frame.params.gaze.lead_hold_s.sample(rng.next_f32()))
        } else {
            frame.clock
        };
    }
}
