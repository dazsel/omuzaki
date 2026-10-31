//! Smear frames (`scene.smears`): a fast move drawn as one streak, the way animated films draw a swing.
//!
//! [`Trails`] keeps the skinning palettes of the last moments of a body. Each frame it hands the renderer the palette
//! from `length_s` ago, carried into the model space of the frame being drawn, as the instances' trailing pose
//! ([`omuzaki_render_api::Smear`]); the renderer pulls each vertex that faces away from its motion back toward it. The
//! threshold is the distance a part covers at `min_speed` in that time, so slow moves stay crisp. Drawn only: the rig,
//! the physics, the face and the touches see the body as posed, and a pinned frame (a still, a sheet) shows no smear.

use std::collections::VecDeque;

use omuzaki_core::glam::Mat4;
use omuzaki_render_api::{JointRange, Smear};
use omuzaki_stage_api::Smears;

/// A gap between frames longer than this (a pause, a seek) starts the trail afresh, seconds.
const MAX_GAP_S: f64 = 0.5;
/// A root that moves this far between frames teleported: the trail starts afresh, metres.
const TELEPORT_M: f32 = 1.0;
/// Slack on `length_s`, seconds, so a 60 Hz clock's 1/60 s steps land on a whole number of frames.
const SLACK_S: f64 = 5e-4;

/// One frame of a body's past: when, its palette, and the world matrix it was drawn with.
#[derive(Clone, Debug)]
struct Past {
    time: f64,
    palette: Vec<Mat4>,
    root: Mat4,
}

/// The recent palettes of one body, and its smear settings.
#[derive(Clone, Debug)]
pub(crate) struct Trails {
    settings: Smears,
    /// Oldest first. At most one frame older than `length_s` is kept.
    past: VecDeque<Past>,
    /// Palettes of frames dropped from `past`, kept for reuse so a frame allocates nothing once warm.
    spare: Vec<Vec<Mat4>>,
}

impl Default for Trails {
    fn default() -> Self {
        Self { settings: Smears::OFF, past: VecDeque::new(), spare: Vec::new() }
    }
}

impl Trails {
    /// Takes a scene's settings; a change starts the trail afresh.
    pub(crate) fn set(&mut self, settings: Smears) {
        if settings != self.settings {
            self.settings = settings;
            self.clear();
        }
    }

    /// Forgets the past: the next frames are drawn as posed until a trail builds up again.
    pub(crate) fn clear(&mut self) {
        while let Some(past) = self.past.pop_front() {
            self.spare.push(past.palette);
        }
    }

    /// Records the frame drawn at `time`: its `palette` and the world matrix `root` it was drawn with. A clock that went
    /// back, a pause or a teleport starts the trail afresh. Nothing is kept while smears are off.
    pub(crate) fn push(&mut self, time: f64, palette: &[Mat4], root: Mat4) {
        if self.settings.is_off() {
            self.clear();
            return;
        }
        if let Some(last) = self.past.back() {
            let jumped = (root.w_axis - last.root.w_axis).truncate().length() >= TELEPORT_M;
            if time <= last.time || time - last.time > MAX_GAP_S || jumped {
                self.clear();
            }
        }
        let length = f64::from(self.settings.length_s) - SLACK_S;
        // Keep one frame at least `length_s` old: drop the oldest while the next one is old enough on its own.
        while self.past.len() >= 2 && self.past.get(1).is_some_and(|next| time - next.time >= length) {
            if let Some(dropped) = self.past.pop_front() {
                self.spare.push(dropped.palette);
            }
        }
        let mut kept = self.spare.pop().unwrap_or_default();
        kept.clear();
        kept.extend_from_slice(palette);
        self.past.push_back(Past { time, palette: kept, root });
    }

    /// The trailing pose for a frame drawn at `time` with the world matrix `root`: the palette of the newest recorded
    /// frame at least `length_s` older, and the matrix that carries its model space into this frame's
    /// (`root⁻¹ × root then`). `None` while smears are off or before the trail is that long.
    pub(crate) fn trail(&self, time: f64, root: Mat4) -> Option<(&[Mat4], Mat4)> {
        if self.settings.is_off() {
            return None;
        }
        let cutoff = time - f64::from(self.settings.length_s) + SLACK_S;
        let then = self.past.iter().rev().find(|past| past.time <= cutoff)?;
        let inverse = root.inverse();
        inverse.is_finite().then(|| (then.palette.as_slice(), inverse * then.root))
    }

    /// The renderer's smear for a trailing pose at `trail` in the scene's joints.
    pub(crate) fn smear(&self, trail: JointRange) -> Smear {
        let s = self.settings;
        let threshold = (s.min_speed * s.length_s).min(Smear::MAX_THRESHOLD);
        Smear { threshold, jag: s.jag, ..Smear::new(trail, s.amount.min(Smear::MAX_AMOUNT)) }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::Vec3;

    use super::*;

    const ON: Smears = Smears { amount: 1.0, length_s: 0.05, min_speed: 1.0, jag: 0.0 };

    fn at(x: f32) -> Mat4 {
        Mat4::from_translation(Vec3::new(x, 0.0, 0.0))
    }

    /// Records 60 Hz frames `0..frames`, frame `i` with palette entry `i` and the root at `x = i / 100`.
    fn trails(settings: Smears, frames: u16) -> Trails {
        let mut t = Trails::default();
        t.set(settings);
        for i in 0..frames {
            let x = f32::from(i) / 100.0;
            t.push(f64::from(i) / 60.0, &[at(f32::from(i))], at(x));
        }
        t
    }

    #[test]
    fn the_trail_is_the_frame_length_s_ago_in_this_frames_model_space() {
        let t = trails(ON, 10);
        // Frame 10 at 10/60 s: 0.05 s back is frame 7.
        let (palette, carry) = t.trail(10.0 / 60.0, at(0.1)).expect("a trail");
        assert_eq!(palette, &[at(7.0)]);
        assert!(carry.abs_diff_eq(at(0.07 - 0.1), 1e-6), "{carry}");
        assert!(t.past.len() <= 4, "only what the trail can still use is kept: {}", t.past.len());
    }

    #[test]
    fn off_young_a_seek_or_a_teleport_have_no_trail() {
        assert!(trails(Smears::OFF, 10).trail(10.0 / 60.0, at(0.1)).is_none());
        assert!(trails(Smears::OFF, 10).past.is_empty(), "nothing is kept while off");
        assert!(trails(ON, 2).trail(2.0 / 60.0, at(0.02)).is_none(), "under length_s of history");
        let mut t = trails(ON, 10);
        t.push(9.0 / 60.0, &[at(0.0)], at(0.0));
        assert!(t.trail(9.0 / 60.0, at(0.0)).is_none(), "a clock that went back");
        let mut t = trails(ON, 10);
        t.push(10.0 / 60.0, &[at(0.0)], at(5.0));
        assert!(t.trail(10.0 / 60.0, at(5.0)).is_none(), "a teleport");
    }

    #[test]
    fn the_smear_carries_the_settings_with_the_threshold_from_the_speed() {
        let t = trails(Smears { min_speed: 2.0, jag: 0.4, ..ON }, 0);
        let s = t.smear(JointRange { start: 3, len: 2 });
        assert_eq!((s.trail, s.amount, s.jag), (JointRange { start: 3, len: 2 }, 1.0, 0.4));
        assert!((s.threshold - 0.1).abs() < 1e-6, "2 m/s for 0.05 s");
        let fast = trails(Smears { min_speed: 10.0, length_s: 0.25, ..ON }, 0).smear(JointRange { start: 0, len: 1 });
        assert!((fast.threshold - Smear::MAX_THRESHOLD).abs() < 1e-6, "capped at the renderer's most");
    }
}
