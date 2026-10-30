//! Hand gestures that press trigger keys (spec v3 PF-06 "gestures as triggers", PF-25): a hand held in a shape to the
//! camera for a moment shows it, and letting go releases it, as a face gesture does (`crate::gesture`).
//!
//! Each frame classifies each seen hand from its solved finger angles ([`classify`]): which of the four fingers are
//! straight, which are curled, and whether the thumb stands out. A finger between the two counts as neither, so a
//! half-closed hand shows nothing rather than flickering between an open palm and a fist. A gesture then needs its shape
//! for [`HandGestureConfig::hold`] to start and its absence for [`HandGestureConfig::release`] to end. Either hand
//! shows a gesture. No allocation per frame.

use omuzaki_track_api::body::{BodyFrame, FingerAngles, Fingers, HandGesture, HandGestures};

/// How the detector decides. Angles in radians.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct HandGestureConfig {
    /// A finger is straight when its two upper joints together bend less than this, and its knuckle less than
    /// [`Self::straight_knuckle`].
    pub straight: f32,
    /// The knuckle's limit for a straight finger.
    pub straight_knuckle: f32,
    /// A finger is curled when its three joints together bend more than this.
    pub curled: f32,
    /// The lowest hand confidence that counts.
    pub min_confidence: f32,
    /// Seconds a shape must hold before its gesture starts.
    pub hold: f64,
    /// Seconds without it before the gesture ends.
    pub release: f64,
}

impl Default for HandGestureConfig {
    fn default() -> Self {
        Self { straight: 0.8, straight_knuckle: 0.7, curled: 1.9, min_confidence: 0.5, hold: 0.3, release: 0.2 }
    }
}

impl HandGestureConfig {
    fn is_straight(&self, f: &FingerAngles) -> bool {
        f.pip + f.dip < self.straight && f.mcp.abs() < self.straight_knuckle
    }

    fn is_curled(&self, f: &FingerAngles) -> bool {
        f.mcp.max(0.0) + f.pip + f.dip > self.curled
    }
}

/// The gesture one hand's fingers show, if any.
pub fn classify(fingers: &Fingers, config: &HandGestureConfig) -> Option<HandGesture> {
    let [thumb, index, middle, ring, little] = fingers;
    let straight = [index, middle, ring, little].map(|f| config.is_straight(f));
    let curled = [index, middle, ring, little].map(|f| config.is_curled(f));
    // The thumb stands out when it is straight: folded across a fist it bends at both joints.
    let thumb_out = config.is_straight(thumb);
    match (straight, curled) {
        ([true, true, true, true], _) => Some(HandGesture::OpenPalm),
        (_, [true, true, true, true]) if thumb_out => Some(HandGesture::ThumbsUp),
        (_, [true, true, true, true]) => Some(HandGesture::Fist),
        ([true, false, false, false], [false, true, true, true]) => Some(HandGesture::Point),
        ([true, true, false, false], [false, false, true, true]) => Some(HandGesture::Peace),
        _ => None,
    }
}

#[derive(Copy, Clone, Debug, Default)]
struct State {
    active: bool,
    /// When the shape started (while inactive) or stopped (while active), on the frames' clock.
    since: Option<f64>,
}

/// Watches body frames for hand gestures.
#[derive(Clone, Debug, Default)]
pub struct HandGestureDetector {
    config: HandGestureConfig,
    states: [State; HandGesture::ALL.len()],
}

impl HandGestureDetector {
    /// A detector deciding as `config` says.
    pub fn new(config: HandGestureConfig) -> Self {
        Self { config, states: Default::default() }
    }

    /// Reads `frame` and returns the gestures showing after it.
    pub fn update(&mut self, frame: &BodyFrame) -> HandGestures {
        let time = frame.time.0;
        let mut seen = HandGestures::NONE;
        for (fingers, &confidence) in frame.fingers.iter().zip(&frame.hands) {
            if let Some(fingers) = fingers.filter(|_| confidence >= self.config.min_confidence)
                && let Some(gesture) = classify(&fingers, &self.config)
            {
                seen = seen.with(gesture);
            }
        }
        let mut showing = HandGestures::NONE;
        for (gesture, state) in HandGesture::ALL.into_iter().zip(&mut self.states) {
            let wait = if state.active { self.config.release } else { self.config.hold };
            if seen.contains(gesture) == state.active {
                state.since = None;
            } else {
                // The shape differs from what shows: switch once it has held for the wait.
                if time - *state.since.get_or_insert(time) >= wait {
                    *state = State { active: !state.active, since: None };
                }
            }
            if state.active {
                showing = showing.with(gesture);
            }
        }
        showing
    }

    /// Lets go of every gesture (the tracker restarted or lost the person).
    pub fn reset(&mut self) {
        self.states = Default::default();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // a panic is a test failure
mod tests {
    use omuzaki_core::time::Seconds;

    use super::*;

    const STRAIGHT: FingerAngles = FingerAngles { spread: 0.0, mcp: 0.1, pip: 0.1, dip: 0.05 };
    const CURLED: FingerAngles = FingerAngles { spread: 0.0, mcp: 1.4, pip: 1.5, dip: 0.9 };
    const HALF: FingerAngles = FingerAngles { spread: 0.0, mcp: 0.7, pip: 0.7, dip: 0.3 };
    const THUMB_IN: FingerAngles = FingerAngles { spread: 0.0, mcp: 0.6, pip: 0.6, dip: 0.6 };

    fn hand(fingers: [FingerAngles; 5]) -> Fingers {
        fingers
    }

    #[test]
    fn each_shape_reads_as_its_gesture() {
        let c = HandGestureConfig::default();
        let (s, k, t) = (STRAIGHT, CURLED, THUMB_IN);
        assert_eq!(classify(&hand([s, s, s, s, s]), &c), Some(HandGesture::OpenPalm));
        assert_eq!(classify(&hand([t, k, k, k, k]), &c), Some(HandGesture::Fist));
        assert_eq!(classify(&hand([s, k, k, k, k]), &c), Some(HandGesture::ThumbsUp));
        assert_eq!(classify(&hand([t, s, k, k, k]), &c), Some(HandGesture::Point));
        assert_eq!(classify(&hand([t, s, s, k, k]), &c), Some(HandGesture::Peace));
    }

    #[test]
    fn a_half_closed_hand_shows_nothing() {
        let c = HandGestureConfig::default();
        assert_eq!(classify(&hand([HALF; 5]), &c), None);
        assert_eq!(classify(&hand([STRAIGHT, STRAIGHT, HALF, CURLED, CURLED]), &c), None);
    }

    fn frame(time: f64, left: Option<Fingers>, right: Option<Fingers>) -> BodyFrame {
        let mut f = BodyFrame::empty(0, Seconds(time));
        f.fingers = [left, right];
        f.hands = [left.map_or(0.0, |_| 1.0), right.map_or(0.0, |_| 1.0)];
        f
    }

    #[test]
    fn a_gesture_starts_after_its_hold_and_ends_after_its_release() {
        let mut d = HandGestureDetector::default();
        let peace = hand([THUMB_IN, STRAIGHT, STRAIGHT, CURLED, CURLED]);
        let shows = |d: &mut HandGestureDetector, t: f64, f: Option<Fingers>| d.update(&frame(t, None, f));
        assert!(shows(&mut d, 0.0, Some(peace)).is_empty());
        assert!(shows(&mut d, 0.2, Some(peace)).is_empty(), "held 0.2 s, under the hold");
        assert!(shows(&mut d, 0.3, Some(peace)).contains(HandGesture::Peace));
        // A one-frame dropout doesn't end it.
        assert!(shows(&mut d, 0.35, None).contains(HandGesture::Peace));
        assert!(shows(&mut d, 0.4, Some(peace)).contains(HandGesture::Peace));
        assert!(shows(&mut d, 0.45, None).contains(HandGesture::Peace));
        assert!(shows(&mut d, 0.65, None).is_empty(), "gone 0.2 s");
    }

    #[test]
    fn either_hand_shows_a_gesture_and_both_can_show_at_once() {
        let mut d = HandGestureDetector::default();
        let fist = hand([THUMB_IN, CURLED, CURLED, CURLED, CURLED]);
        let open = hand([STRAIGHT; 5]);
        for i in 0..10 {
            d.update(&frame(f64::from(i) * 0.05, Some(fist), Some(open)));
        }
        let shown = d.update(&frame(0.5, Some(fist), Some(open)));
        // Held 0.5 s: past the hold.
        assert!(shown.contains(HandGesture::Fist) && shown.contains(HandGesture::OpenPalm), "{shown:?}");
    }

    #[test]
    fn an_unsure_hand_counts_for_nothing() {
        let mut d = HandGestureDetector::default();
        let mut f = frame(0.0, Some(hand([STRAIGHT; 5])), None);
        f.hands[0] = 0.2;
        for i in 0..10 {
            f.time = Seconds(f64::from(i) * 0.05);
            assert!(d.update(&f).is_empty());
        }
    }
}
