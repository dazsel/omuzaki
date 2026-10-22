//! Face gestures that fire triggers (spec v3 PF-25: "auto-detected gestures, tongue out → bleh"): a tracked face held
//! in a shape for a moment presses a trigger key, and letting go releases it, so a gesture shows an expression the way a
//! held hotkey does.
//!
//! Each gesture reads the tracked `ARKit` channels after the calibration in use (each channel's range scaled to `0..1`),
//! so a webcam that reads low and a phone read alike. A gesture needs its channels in the frame: the webcam's model has
//! no tongue, so the tongue gesture comes from a phone. Detection has two thresholds (on and off) and two hold times
//! (to start and to end), so a blink is not a wink and a flicker over the line does not toggle. No allocation per
//! frame.

use omuzaki_anim_api::{Calibration, Channel, TrackFrame, TrackStatus};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A face gesture.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Gesture {
    /// The tongue out.
    TongueOut,
    /// The left eye shut while the right stays open (the performer's left).
    WinkLeft,
    /// The right eye shut while the left stays open.
    WinkRight,
    /// Both cheeks puffed.
    CheekPuff,
    /// The mouth wide open, as in surprise.
    MouthWide,
}

impl Gesture {
    /// Every gesture, in a fixed order (the detector's state is indexed by it).
    pub const ALL: [Self; 5] = [Self::TongueOut, Self::WinkLeft, Self::WinkRight, Self::CheekPuff, Self::MouthWide];

    /// The trigger key a gesture presses: bind an expression to it with `trigger.bind`.
    pub const fn key(self) -> &'static str {
        match self {
            Self::TongueOut => "gesture:tongue_out",
            Self::WinkLeft => "gesture:wink_left",
            Self::WinkRight => "gesture:wink_right",
            Self::CheekPuff => "gesture:cheek_puff",
            Self::MouthWide => "gesture:mouth_wide",
        }
    }

    /// The gesture whose key is `key`, ignoring case.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|g| g.key().eq_ignore_ascii_case(key))
    }

    /// The channels it reads: the one that must be high, and the one that must stay low (for a wink).
    const fn channels(self) -> (Channel, Option<Channel>) {
        match self {
            Self::TongueOut => (Channel::TONGUE_OUT, None),
            Self::WinkLeft => (Channel::EYE_BLINK_LEFT, Some(Channel::EYE_BLINK_RIGHT)),
            Self::WinkRight => (Channel::EYE_BLINK_RIGHT, Some(Channel::EYE_BLINK_LEFT)),
            Self::CheekPuff => (Channel::CHEEK_PUFF, None),
            Self::MouthWide => (Channel::JAW_OPEN, None),
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// How the detector decides. The defaults suit a calibrated face.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct GestureConfig {
    /// The level (calibrated, `0..1`) a gesture's channel must reach to start it.
    pub on: f32,
    /// The level it must fall below to end it.
    pub off: f32,
    /// The highest level the other eye may have during a wink.
    pub other_eye: f32,
    /// Seconds the shape must hold before the gesture starts: longer than a blink, so a blink is never a wink.
    pub hold: f64,
    /// Seconds below `off` before the gesture ends.
    pub release: f64,
}

impl Default for GestureConfig {
    fn default() -> Self {
        Self { on: 0.6, off: 0.4, other_eye: 0.35, hold: 0.25, release: 0.15 }
    }
}

/// One gesture starting (`true`) or ending.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    /// The gesture.
    pub gesture: Gesture,
    /// Whether it started.
    pub down: bool,
}

#[derive(Copy, Clone, Debug, Default)]
struct State {
    active: bool,
    /// When the shape started (while inactive) or stopped (while active), on the frames' clock.
    since: Option<f64>,
}

/// Watches tracked frames for gestures.
#[derive(Clone, Debug, Default)]
pub struct GestureDetector {
    config: GestureConfig,
    states: [State; Gesture::ALL.len()],
}

impl GestureDetector {
    /// A detector deciding as `config` says.
    pub fn new(config: GestureConfig) -> Self {
        Self { config, states: Default::default() }
    }

    /// Whether `gesture` is showing.
    pub fn active(&self, gesture: Gesture) -> bool {
        self.states.get(gesture.index()).is_some_and(|s| s.active)
    }

    /// Reads `frame` through `calibration` and returns the gestures that started or ended, in [`Gesture::ALL`] order,
    /// as a fixed array (`None` where nothing changed). A frame with no face ends every gesture at once.
    pub fn update(&mut self, frame: &TrackFrame, calibration: &Calibration) -> [Option<Edge>; Gesture::ALL.len()] {
        let mut edges = [None; Gesture::ALL.len()];
        let time = frame.time.0;
        let tracked = frame.status == TrackStatus::Tracking;
        for (gesture, (state, edge)) in Gesture::ALL.into_iter().zip(self.states.iter_mut().zip(edges.iter_mut())) {
            let (main, other) = gesture.channels();
            let read = |c: Channel| frame.present.contains(c).then(|| calibration.range(c).normalize(frame.get(c)));
            let level = if tracked { read(main) } else { None };
            let other_ok = |limit: f32| other.is_none_or(|o| read(o).is_some_and(|v| v <= limit));
            let shaped = |threshold: f32, limit: f32| level.is_some_and(|v| v >= threshold) && other_ok(limit);
            if state.active {
                // Held while above `off`; the other eye may open a little more than at the start.
                if shaped(self.config.off, self.config.other_eye + (self.config.on - self.config.off)) {
                    state.since = None;
                } else if level.is_none() || time - *state.since.get_or_insert(time) >= self.config.release {
                    *state = State::default();
                    *edge = Some(Edge { gesture, down: false });
                }
            } else if shaped(self.config.on, self.config.other_eye) {
                if time - *state.since.get_or_insert(time) >= self.config.hold {
                    *state = State { active: true, since: None };
                    *edge = Some(Edge { gesture, down: true });
                }
            } else {
                state.since = None;
            }
        }
        edges
    }

    /// Ends every gesture: what to release when the source changes or stops.
    pub fn reset(&mut self) -> [Option<Edge>; Gesture::ALL.len()] {
        let mut edges = [None; Gesture::ALL.len()];
        for ((gesture, state), edge) in Gesture::ALL.into_iter().zip(self.states.iter_mut()).zip(edges.iter_mut()) {
            if state.active {
                *edge = Some(Edge { gesture, down: false });
            }
            *state = State::default();
        }
        edges
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss)] // frame counts as times
mod tests {
    use omuzaki_anim_api::{ChannelMask, DeviceProfile};
    use omuzaki_core::time::Seconds;

    use super::*;

    /// A tracked frame at `time` with `values` set and every ARKit channel present.
    fn frame(time: f64, values: &[(Channel, f32)]) -> TrackFrame {
        let mut f = TrackFrame::empty(0, Seconds(time), DeviceProfile::Arkit);
        f.status = TrackStatus::Tracking;
        f.present = ChannelMask::ARKIT;
        for &(c, v) in values {
            f.blend[c.index()] = v;
        }
        f
    }

    /// Runs `frames` at 60 fps from `start` with `values` and collects the edges.
    fn run(d: &mut GestureDetector, start: f64, frames: usize, values: &[(Channel, f32)]) -> Vec<Edge> {
        let cal = Calibration::identity(DeviceProfile::Arkit);
        (0..frames).flat_map(|i| d.update(&frame(start + i as f64 / 60.0, values), &cal)).flatten().collect()
    }

    #[test]
    fn a_held_tongue_presses_once_and_letting_go_releases_once() {
        let mut d = GestureDetector::default();
        let out = [(Channel::TONGUE_OUT, 0.9)];
        // Shorter than the hold: nothing yet.
        assert_eq!(run(&mut d, 0.0, 10, &out), []);
        let started = run(&mut d, 10.0 / 60.0, 20, &out);
        assert_eq!(started, [Edge { gesture: Gesture::TongueOut, down: true }]);
        // A dip that stays above `off` keeps it.
        assert_eq!(run(&mut d, 0.5, 30, &[(Channel::TONGUE_OUT, 0.45)]), []);
        let ended = run(&mut d, 1.0, 20, &[]);
        assert_eq!(ended, [Edge { gesture: Gesture::TongueOut, down: false }]);
        assert!(!d.active(Gesture::TongueOut));
    }

    #[test]
    fn a_blink_is_not_a_wink_and_a_held_one_eyed_close_is() {
        let mut d = GestureDetector::default();
        // A normal blink: both eyes, 0.15 s.
        let blink = [(Channel::EYE_BLINK_LEFT, 1.0), (Channel::EYE_BLINK_RIGHT, 1.0)];
        assert_eq!(run(&mut d, 0.0, 9, &blink), []);
        // Both eyes held shut is not a wink either.
        assert_eq!(run(&mut d, 1.0, 60, &blink), []);
        // A one-eyed close shorter than the hold.
        assert_eq!(run(&mut d, 3.0, 9, &[(Channel::EYE_BLINK_LEFT, 1.0)]), []);
        let wink = run(&mut d, 4.0, 30, &[(Channel::EYE_BLINK_LEFT, 1.0), (Channel::EYE_BLINK_RIGHT, 0.1)]);
        assert_eq!(wink, [Edge { gesture: Gesture::WinkLeft, down: true }]);
    }

    #[test]
    fn a_missing_channel_never_fires_and_a_lost_face_ends_everything() {
        let mut d = GestureDetector::default();
        let cal = Calibration::identity(DeviceProfile::Webcam);
        let mut webcam = frame(0.0, &[(Channel::TONGUE_OUT, 1.0)]);
        webcam.present = ChannelMask::ARKIT.without(ChannelMask::of(Channel::TONGUE_OUT));
        for i in 0..60 {
            webcam.time = Seconds(f64::from(i) / 60.0);
            assert!(d.update(&webcam, &cal).iter().all(Option::is_none));
        }
        run(&mut d, 2.0, 30, &[(Channel::JAW_OPEN, 1.0)]);
        assert!(d.active(Gesture::MouthWide));
        let mut lost = frame(3.0, &[]);
        lost.status = TrackStatus::Lost;
        let edges: Vec<Edge> = d.update(&lost, &cal).into_iter().flatten().collect();
        assert_eq!(edges, [Edge { gesture: Gesture::MouthWide, down: false }]);
    }

    #[test]
    fn the_calibration_scales_a_webcam_that_reads_low() {
        let mut d = GestureDetector::default();
        let mut cal = Calibration::identity(DeviceProfile::Webcam);
        cal.set_range(Channel::CHEEK_PUFF, omuzaki_anim_api::ChannelRange { lo: 0.0, hi: 0.3 });
        let edges: Vec<Edge> = (0..30)
            .flat_map(|i| d.update(&frame(f64::from(i) / 60.0, &[(Channel::CHEEK_PUFF, 0.25)]), &cal))
            .flatten()
            .collect();
        assert_eq!(edges, [Edge { gesture: Gesture::CheekPuff, down: true }]);
    }

    #[test]
    fn keys_name_their_gesture() {
        for g in Gesture::ALL {
            assert_eq!(Gesture::from_key(g.key()), Some(g));
            assert!(g.key().len() <= 32);
        }
        assert_eq!(Gesture::from_key("GESTURE:Tongue_Out"), Some(Gesture::TongueOut));
        assert_eq!(Gesture::from_key("F1"), None);
    }
}
