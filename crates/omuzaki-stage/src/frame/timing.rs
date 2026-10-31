//! Animated timing (`scene.timing`): the cast's poses held on twos or threes while the camera, the lights, the effects
//! and the render run at the frame rate, as drawn animation and stylized 3D animation are timed.
//!
//! [`Holds`] decides, per frame, what moves: the body (the rig's solve and the character's root), the secondary motion
//! (hair, cloth and the other runtimes) and the painted face. A frame in the same slot as the last one moved
//! ([`Timing::slot`]) keeps what that frame showed. With `hold_secondary` the runtimes hold too and, at each new pose,
//! step once over the whole held time, so hair and cloth catch up in one drawn step instead of sliding between poses.
//! A pinned frame (a still, a sheet) always moves everything: it must show the pose it is given.

use omuzaki_anim_api::Tick;
use omuzaki_stage_api::Timing;

/// The longest step the runtimes take to catch up after a hold, seconds: an eighth of a second covers "on threes" at
/// 24 fps, and a longer hold (a scene that held at 1 Hz) catches up no further than this in one step.
const MAX_CATCH_UP_S: f32 = 0.25;

/// What one frame moves.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct HoldFrame {
    /// Solve the rig with this frame's pose and goals, and move the root.
    pub(crate) body: bool,
    /// Step the runtimes by this many seconds; `None` holds them.
    pub(crate) secondary_dt: Option<f32>,
    /// Paint the face with this frame's channels.
    pub(crate) face: bool,
}

impl HoldFrame {
    /// Everything moves, the runtimes by `dt`.
    pub(crate) fn all(dt: f32) -> Self {
        Self { body: true, secondary_dt: Some(dt), face: true }
    }
}

/// The holds state of a stage: the timing and the slots the body and the face last moved in.
#[derive(Clone, Debug, Default)]
pub(crate) struct Holds {
    timing: Timing,
    body: Option<u64>,
    face: Option<u64>,
    /// When the body last moved, for the runtimes' catch-up step.
    body_time: Option<f64>,
}

impl Holds {
    /// Takes a scene's timing; a change starts the holds afresh, so the next frame moves everything.
    pub(crate) fn set(&mut self, timing: Timing) {
        if timing != self.timing {
            *self = Self { timing, ..Self::default() };
        }
    }

    /// The timing in force.
    pub(crate) fn timing(&self) -> Timing {
        self.timing
    }

    /// What the frame at `tick` moves, and the slots it moved in.
    pub(crate) fn frame(&mut self, tick: Tick) -> HoldFrame {
        if self.timing.is_smooth() {
            return HoldFrame::all(tick.dt);
        }
        let now = tick.time.0;
        let body_slot = Timing::slot(now, self.timing.body_hz);
        let body = body_slot.is_none() || body_slot != self.body;
        let secondary_dt = match (body, self.timing.hold_secondary && body_slot.is_some()) {
            (_, false) => Some(tick.dt),
            (false, true) => None,
            (true, true) => {
                // The time since the last pose, back to one frame when there was none or the clock went back.
                #[allow(clippy::cast_possible_truncation)] // a hold of seconds fits f32
                let held = self.body_time.map_or(tick.dt, |t| (now - t) as f32);
                Some(if held > 0.0 && held.is_finite() { held.min(MAX_CATCH_UP_S) } else { tick.dt })
            }
        };
        if body {
            self.body = body_slot;
            self.body_time = Some(now);
        }
        let face_slot = Timing::slot(now, self.timing.face_hz);
        let face = face_slot.is_none() || face_slot != self.face;
        if face {
            self.face = face_slot;
        }
        HoldFrame { body, secondary_dt, face }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::time::Seconds;

    use super::*;

    fn tick(i: u64) -> Tick {
        Tick { index: i, time: Seconds(i as f64 / 60.0), dt: 1.0 / 60.0 }
    }

    #[test]
    fn smooth_timing_moves_everything_every_frame() {
        let mut holds = Holds::default();
        for i in 0..10 {
            assert_eq!(holds.frame(tick(i)), HoldFrame::all(1.0 / 60.0));
        }
    }

    #[test]
    fn on_twos_moves_the_body_twelve_times_a_second_and_secondary_catches_up_once() {
        let mut holds = Holds::default();
        holds.set(Timing { body_hz: 12.0, face_hz: 0.0, hold_secondary: true });
        let frames: Vec<HoldFrame> = (0..60).map(|i| holds.frame(tick(i))).collect();
        let moved: Vec<u64> = (0..60).filter(|&i| frames[i as usize].body).collect();
        assert_eq!(moved, vec![0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55], "a new pose every 5 frames at 60 Hz");
        assert!(frames.iter().all(|f| f.face), "the face moves every frame");
        assert_eq!(frames[1].secondary_dt, None, "hair holds with the body");
        let catch_up = frames[5].secondary_dt.unwrap();
        assert!((catch_up - 5.0 / 60.0).abs() < 1e-5, "one step over the held time: {catch_up}");
    }

    #[test]
    fn threes_and_a_held_face_slot_independently_and_secondary_can_keep_moving() {
        let mut holds = Holds::default();
        holds.set(Timing { body_hz: 8.0, face_hz: 12.0, hold_secondary: false });
        let frames: Vec<HoldFrame> = (0..60).map(|i| holds.frame(tick(i))).collect();
        assert_eq!(frames.iter().filter(|f| f.body).count(), 8);
        assert_eq!(frames.iter().filter(|f| f.face).count(), 12);
        assert!(frames.iter().all(|f| f.secondary_dt == Some(1.0 / 60.0)), "hair keeps moving between poses");
    }

    #[test]
    fn a_new_timing_or_a_clock_that_restarts_moves_everything_at_once() {
        let mut holds = Holds::default();
        holds.set(Timing { body_hz: 12.0, ..Timing::SMOOTH });
        assert!(holds.frame(tick(3)).body);
        assert!(!holds.frame(tick(4)).body);
        holds.set(Timing { body_hz: 8.0, ..Timing::SMOOTH });
        assert!(holds.frame(tick(4)).body, "a changed timing starts afresh");
        let restarted = holds.frame(tick(0));
        assert!(restarted.body, "time went back to another slot");
        assert_eq!(restarted.secondary_dt, Some(1.0 / 60.0), "no negative catch-up");
    }
}
