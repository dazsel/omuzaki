//! The numbers behind each preset: how big, how fast and how often the body moves for a temperament. A
//! [`BodyStyle`]'s amounts multiply these; the measure (`measure.body_motion`) reads them back.

use omuzaki_anim_api::{BodyStyle, Personality, Span};

use super::arm::ArmPose;

/// Every tuning number of the living body for one temperament, after the style's amounts are applied.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Tuning {
    /// Degrees of torso sway at the noise's peak, on each of pitch, yaw and roll.
    pub sway_deg: f32,
    /// Seconds between weight shifts.
    pub shift_gap_s: Span,
    /// Degrees the hips roll at a full weight shift (the chest counter-rolls).
    pub shift_deg: f32,
    /// Share of the head's yaw, pitch and roll the chest and shoulders take.
    pub follow: [f32; 3],
    /// Degrees the chest lifts and the shoulders rise on a full breath.
    pub breath_deg: [f32; 2],
    /// How strongly the face's emotion bends the posture.
    pub emotion: f32,
    /// Standing posture: degrees of forward slump (negative stands tall) and of shoulders brought in.
    pub posture_deg: [f32; 2],
    /// Gesture size (the stroke's travel from its preparation, as a multiple of the authored one).
    pub gesture_size: f32,
    /// Seconds of talk between gesture phrases.
    pub gesture_gap_s: Span,
    /// Speed of everything the arms do, as a multiple.
    pub speed: f32,
    /// Share of the torso's motion the arms lag behind (follow-through).
    pub arms: f32,
    /// Seconds between idle fidgets.
    pub fidget_gap_s: Span,
    /// The talk-rest arm: where the hands wait between gestures.
    pub talk_rest: ArmPose,
    /// Odds that a one-handed gesture takes the leading hand.
    pub lead_odds: f32,
}

impl Tuning {
    /// The tuning for `style`: its preset's numbers with every amount applied.
    pub fn of(style: &BodyStyle) -> Self {
        let s = style.clamped();
        let mut t = Self::preset(s.preset);
        let a = s.amount;
        t.sway_deg *= s.sway * a;
        t.shift_deg *= s.sway * a;
        t.follow = t.follow.map(|f| f * s.follow * a);
        t.breath_deg = t.breath_deg.map(|b| b * s.breath * a);
        t.emotion *= s.emotion * a;
        t.posture_deg = t.posture_deg.map(|p| p * a);
        // Size grows to half again; past that the amount buys rate, not ever-bigger arms.
        t.gesture_size *= (s.gestures * a).min(1.5);
        // More gestures come closer together; none at all is handled by the scheduler (size 0 never starts one).
        let rate = (s.gestures * a).max(0.05);
        t.gesture_gap_s = Span::new(t.gesture_gap_s.lo / rate, t.gesture_gap_s.hi / rate);
        t.arms *= s.arms * a;
        let fidgets = (s.fidgets * a).max(0.05);
        t.fidget_gap_s = Span::new(t.fidget_gap_s.lo / fidgets, t.fidget_gap_s.hi / fidgets);
        if s.fidgets * a <= 0.0 {
            t.fidget_gap_s = Span::new(f32::INFINITY, f32::INFINITY);
        }
        t
    }

    /// A preset's numbers, every amount at 1.
    fn preset(p: Personality) -> Self {
        let natural = Self {
            sway_deg: 0.8,
            shift_gap_s: Span::new(5.0, 12.0),
            shift_deg: 2.0,
            follow: [0.22, 0.18, 0.2],
            breath_deg: [1.4, 1.6],
            emotion: 1.0,
            posture_deg: [0.0, 0.0],
            gesture_size: 1.0,
            gesture_gap_s: Span::new(0.6, 2.2),
            speed: 1.0,
            arms: 0.65,
            fidget_gap_s: Span::new(6.0, 12.0),
            talk_rest: ArmPose {
                raise: 10.0,
                forward: 16.0,
                twist: 28.0,
                elbow: 62.0,
                pronate: 15.0,
                wrist: 6.0,
                shrug: 0.0,
                reach: 2.0,
            },
            lead_odds: 0.68,
        };
        match p {
            Personality::Natural => natural,
            Personality::Energetic => Self {
                sway_deg: 1.1,
                shift_gap_s: Span::new(3.0, 8.0),
                shift_deg: 2.4,
                follow: [0.28, 0.22, 0.24],
                emotion: 1.3,
                posture_deg: [-2.0, -1.0],
                gesture_size: 1.3,
                gesture_gap_s: Span::new(0.25, 1.1),
                speed: 1.25,
                arms: 0.75,
                fidget_gap_s: Span::new(4.0, 9.0),
                talk_rest: ArmPose { forward: 22.0, elbow: 72.0, ..natural.talk_rest },
                ..natural
            },
            Personality::Calm => Self {
                sway_deg: 0.5,
                shift_gap_s: Span::new(8.0, 16.0),
                shift_deg: 1.5,
                follow: [0.16, 0.12, 0.14],
                emotion: 0.7,
                gesture_size: 0.8,
                gesture_gap_s: Span::new(1.4, 4.0),
                speed: 0.82,
                arms: 0.55,
                fidget_gap_s: Span::new(10.0, 18.0),
                talk_rest: ArmPose { forward: 12.0, elbow: 50.0, twist: 32.0, ..natural.talk_rest },
                ..natural
            },
            Personality::Shy => Self {
                sway_deg: 0.7,
                shift_gap_s: Span::new(4.0, 9.0),
                shift_deg: 1.6,
                follow: [0.18, 0.2, 0.16],
                emotion: 1.1,
                posture_deg: [3.0, 4.0],
                gesture_size: 0.6,
                gesture_gap_s: Span::new(2.5, 6.0),
                speed: 0.9,
                arms: 0.5,
                fidget_gap_s: Span::new(4.0, 8.0),
                talk_rest: ArmPose { forward: 20.0, elbow: 80.0, twist: 42.0, raise: 6.0, ..natural.talk_rest },
                lead_odds: 0.8,
                ..natural
            },
            Personality::Sleepy => Self {
                sway_deg: 1.0,
                shift_gap_s: Span::new(7.0, 15.0),
                shift_deg: 1.8,
                follow: [0.2, 0.24, 0.22],
                emotion: 0.7,
                posture_deg: [5.0, 2.5],
                gesture_size: 0.55,
                gesture_gap_s: Span::new(3.0, 7.0),
                speed: 0.68,
                arms: 0.6,
                fidget_gap_s: Span::new(8.0, 15.0),
                talk_rest: ArmPose { forward: 8.0, elbow: 36.0, ..natural.talk_rest },
                ..natural
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_plausible_and_off_is_still() {
        for p in [Personality::Natural, Personality::Energetic, Personality::Calm, Personality::Shy, Personality::Sleepy] {
            let t = Tuning::of(&BodyStyle::preset(p));
            assert!(t.talk_rest.is_plausible(), "{p:?}");
            assert!(t.gesture_gap_s.lo <= t.gesture_gap_s.hi && t.shift_gap_s.lo <= t.shift_gap_s.hi);
        }
        let off = Tuning::of(&BodyStyle::OFF);
        assert_eq!(off.sway_deg, 0.0);
        assert_eq!(off.gesture_size, 0.0);
        assert_eq!(off.follow, [0.0; 3]);
        assert!(off.fidget_gap_s.lo.is_infinite());
    }
}
