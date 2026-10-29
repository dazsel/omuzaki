//! Named expressions (spec FE-03) and the visemes that lip sync drives (FE-08).
//!
//! An expression is a set of **channel deltas** with an intensity curve, in the bus's own vocabulary (`ARKit` channels
//! and the anime extras), so tracker, score, lip sync and expression all speak one language. A driver gives weights
//! (`happy` at 0.6, `aa` at 0.3); [`ExpressionSet::blend`] adds the scaled deltas onto a base buffer and sanitises.
//! Visemes are ordinary expressions with the VRM preset names, so a lip-sync source and a VRM export agree without a
//! translation table.
//!
//! [`ExpressionId`] is declared by `omuzaki-anim-api` (score cues refer to it); this crate owns the library.

use std::collections::BTreeMap;

pub use omuzaki_anim_api::ExpressionId;
use omuzaki_anim_api::{CHANNELS, Channel, Channel as C, ChannelBuf};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use crate::paint::narrow;

/// The five mouth shapes lip sync produces, with the VRM preset names. Audio sources emit weights for these ids.
pub const VISEMES: [&str; 5] = ["aa", "ih", "ou", "ee", "oh"];

/// A sparse set of channel values: an expression's deltas, a character's resting buffer. Written as a map of channel
/// name to number; absent channels mean "no change". `f64`, like every parameter that reaches the document or an op
/// (`0.6` stays `0.6` in files and agent tool results); the bus itself is `f32`, converted when written into it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelValues(pub BTreeMap<Channel, f64>);

impl ChannelValues {
    /// Sets `c` to `v` (builder style).
    #[must_use]
    pub fn with(mut self, c: Channel, v: f64) -> Self {
        self.0.insert(c, v);
        self
    }

    /// Sets every listed channel in `buf` to its value (absolute), then sanitises the buffer.
    pub fn write_into(&self, buf: &mut ChannelBuf) {
        for (c, v) in &self.0 {
            buf.set(*c, narrow(*v));
        }
        buf.sanitize();
    }

    /// Checks every value is finite and inside its channel's legal range (use for absolute values such as a resting
    /// buffer; deltas may legitimately be negative).
    ///
    /// # Errors
    /// The first offending channel, named, with its range.
    pub fn validate_ranges(&self) -> Result<(), String> {
        for (c, v) in &self.0 {
            let (lo, hi) = (decimal(c.range().0), decimal(c.range().1));
            if !(v.is_finite() && (lo..=hi).contains(v)) {
                return Err(format!("{c} = {v} is out of range: use a number from {lo} to {hi}"));
            }
        }
        Ok(())
    }
}

impl JsonSchema for ChannelValues {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ChannelValues".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        // One property per channel, so an agent reading the schema sees every channel with its range.
        let props: serde_json::Map<String, serde_json::Value> = CHANNELS
            .iter()
            .map(|i| {
                (
                    i.name.to_owned(),
                    serde_json::json!({ "type": "number", "minimum": decimal(i.min), "maximum": decimal(i.max) }),
                )
            })
            .collect();
        schemars::json_schema!({ "type": "object", "properties": props, "additionalProperties": false })
    }
}

/// An `f32` channel limit as the decimal it was written as (`-12.6`, not `-12.600000381`), for messages and schemas.
pub(crate) fn decimal(v: f32) -> f64 {
    v.to_string().parse().unwrap_or_else(|_| f64::from(v))
}

/// What an expression is for; lets UIs group them and the range test (FE-04) treat visemes separately.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExpressionKind {
    /// A mood: happy, sad, angry, smug, shy.
    Emotion,
    /// A mouth shape for lip sync.
    Viseme,
    /// A lid close (VRM `blink`).
    Blink,
    /// A short accent: a wink, a sparkle, a sweat drop.
    Accent,
}

/// How the weight `0..=1` maps to how much of the deltas applies.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntensityCurve {
    /// Proportional.
    Linear,
    /// Slow start, fast finish (`w²`): subtle at low weights.
    EaseIn,
    /// Fast start, slow finish: shows up early.
    EaseOut,
    /// Smoothstep: soft at both ends.
    Smooth,
    /// `w^exponent`, for a custom feel.
    Power {
        /// Greater than 0; below 1 shows up early, above 1 late.
        exponent: f64,
    },
}

impl IntensityCurve {
    /// The applied fraction for weight `w` (clamped to `0..=1`).
    pub fn eval(self, w: f32) -> f32 {
        let w = w.clamp(0.0, 1.0);
        match self {
            Self::Linear => w,
            Self::EaseIn => w * w,
            Self::EaseOut => 1.0 - (1.0 - w) * (1.0 - w),
            Self::Smooth => w * w * (3.0 - 2.0 * w),
            Self::Power { exponent } => w.powf(narrow(exponent).max(f32::EPSILON)),
        }
    }
}

/// The layer a shown expression plays on (FE-03 "layers").
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackLayer {
    /// It stays until it is hidden or replaced: a mood the performer sets.
    #[default]
    Held,
    /// It plays over the held ones, which step back while it shows and return when it ends: a wink, a sweat drop.
    Momentary,
}

/// How an expression plays when a trigger shows it (spec FE-03: fades, auto-stop, exclusive groups, layers, dwell). The
/// trigger runtime (PF-25) reads it; the player in `omuzaki-face` (`player`) runs it. Lip sync and scores give weights
/// directly and don't use it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Playback {
    /// Seconds to fade in, `0..=10`; 0 shows it at once.
    pub fade_in_s: f64,
    /// Seconds to fade out, `0..=10`.
    pub fade_out_s: f64,
    /// Hide it on its own this many seconds after it is shown, `0.05..=600`; left out, it stays until hidden.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_stop_s: Option<f64>,
    /// Its exclusive group: showing an expression hides every other one in the same group (one mood at a time).
    /// Lowercase letters, digits, `_` and `-`; left out, it is in no group.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Held (stays) or momentary (plays over the held ones).
    pub layer: PlaybackLayer,
    /// The least time it holds before another expression in its group may replace it, `0..=10` s: a face asked for
    /// sooner waits until then (the latest one asked for wins), so faces don't flicker. Left out, it may be replaced at
    /// once. A cut ignores it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dwell_s: Option<f64>,
    /// It replaces another in its group at once, whatever that one's dwell: a startle or a flash of anger cuts in.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cuts_in: bool,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            fade_in_s: 0.12,
            fade_out_s: 0.2,
            auto_stop_s: None,
            group: None,
            layer: PlaybackLayer::Held,
            dwell_s: None,
            cuts_in: false,
        }
    }
}

impl Playback {
    /// True for the default playback, which files leave out.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl crate::check::Validate for Playback {
    fn check(&self, c: &mut crate::check::Check) {
        c.range("fade_in_s", self.fade_in_s, 0.0, 10.0);
        c.range("fade_out_s", self.fade_out_s, 0.0, 10.0);
        if let Some(stop) = self.auto_stop_s {
            c.range("auto_stop_s", stop, 0.05, 600.0);
        }
        if let Some(dwell) = self.dwell_s {
            c.range("dwell_s", dwell, 0.0, 10.0);
        }
        if let Some(group) = &self.group {
            let ok = !group.is_empty()
                && group.len() <= 32
                && group.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
            c.rule(&["group"], ok, "a group is 1 to 32 lowercase letters, digits, '_' or '-'");
        }
    }
}

/// One named expression.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Expression {
    /// What it is for.
    pub kind: ExpressionKind,
    /// Channel deltas at full weight. Only the channels listed move.
    pub deltas: ChannelValues,
    /// The weight curve; linear unless a curve is given.
    #[serde(default = "linear")]
    pub curve: IntensityCurve,
    /// How it plays when a trigger shows it; the default (fade in 0.12 s, out 0.2 s, held, no group, no auto-stop) is
    /// left out of files.
    #[serde(default, skip_serializing_if = "Playback::is_default")]
    pub playback: Playback,
}

fn linear() -> IntensityCurve {
    IntensityCurve::Linear
}

/// A starter-table row: name, kind, channel deltas.
type StarterRow<'a> = (&'a str, ExpressionKind, &'a [(Channel, f64)]);

/// The starter library's rows (see [`ExpressionSet::starter`]).
const STARTER: [StarterRow<'static>; 11] = [
    (
        "happy",
        ExpressionKind::Emotion,
        &[
            (C::EYE_ARC_LEFT, 1.0),
            (C::EYE_ARC_RIGHT, 1.0),
            (C::JAW_OPEN, 0.36),
            (C::MOUTH_SMILE_LEFT, 0.8),
            (C::MOUTH_SMILE_RIGHT, 0.8),
            (C::BLUSH, 0.35),
            (C::BROW_OUTER_UP_LEFT, 0.2),
            (C::BROW_OUTER_UP_RIGHT, 0.2),
        ],
    ),
    (
        "angry",
        ExpressionKind::Emotion,
        &[
            (C::BROW_DOWN_LEFT, 0.8),
            (C::BROW_DOWN_RIGHT, 0.8),
            (C::EYE_SQUINT_LEFT, 0.25),
            (C::EYE_SQUINT_RIGHT, 0.25),
            (C::MOUTH_FROWN_LEFT, 0.7),
            (C::MOUTH_FROWN_RIGHT, 0.7),
            (C::JAW_OPEN, 0.07),
            (C::NOSE_SNEER_LEFT, 0.25),
            (C::NOSE_SNEER_RIGHT, 0.25),
        ],
    ),
    (
        "sad",
        ExpressionKind::Emotion,
        &[
            (C::BROW_INNER_UP, 0.9),
            (C::MOUTH_FROWN_LEFT, 0.9),
            (C::MOUTH_FROWN_RIGHT, 0.9),
            (C::EYE_SQUINT_LEFT, 0.1),
            (C::EYE_SQUINT_RIGHT, 0.1),
            (C::JAW_OPEN, 0.06),
            (C::TEARS, 1.0),
            (C::BLUSH, 0.25),
        ],
    ),
    (
        "relaxed",
        ExpressionKind::Emotion,
        &[
            (C::EYE_SQUINT_LEFT, 0.9),
            (C::EYE_SQUINT_RIGHT, 0.9),
            (C::MOUTH_SMILE_LEFT, 0.7),
            (C::MOUTH_SMILE_RIGHT, 0.7),
            (C::BROW_DOWN_LEFT, 0.15),
            (C::BROW_DOWN_RIGHT, 0.15),
            (C::BLUSH, 0.25),
        ],
    ),
    (
        "surprised",
        ExpressionKind::Emotion,
        &[
            (C::EYE_WIDE_LEFT, 1.0),
            (C::EYE_WIDE_RIGHT, 1.0),
            (C::BROW_INNER_UP, 0.6),
            (C::BROW_OUTER_UP_LEFT, 0.7),
            (C::BROW_OUTER_UP_RIGHT, 0.7),
            (C::JAW_OPEN, 0.4),
            (C::MOUTH_FUNNEL, 0.8),
            (C::SPARKLE, 1.0),
        ],
    ),
    ("blink", ExpressionKind::Blink, &[(C::EYE_BLINK_LEFT, 1.0), (C::EYE_BLINK_RIGHT, 1.0)]),
    ("aa", ExpressionKind::Viseme, &[(C::JAW_OPEN, 0.74), (C::MOUTH_SMILE_LEFT, 0.1), (C::MOUTH_SMILE_RIGHT, 0.1)]),
    (
        "ih",
        ExpressionKind::Viseme,
        &[
            (C::JAW_OPEN, 0.24),
            (C::MOUTH_STRETCH_LEFT, 0.4),
            (C::MOUTH_STRETCH_RIGHT, 0.4),
            (C::MOUTH_UPPER_UP_LEFT, 0.8),
            (C::MOUTH_UPPER_UP_RIGHT, 0.8),
            (C::MOUTH_SMILE_LEFT, 0.2),
            (C::MOUTH_SMILE_RIGHT, 0.2),
        ],
    ),
    ("ou", ExpressionKind::Viseme, &[(C::JAW_OPEN, 0.36), (C::MOUTH_FUNNEL, 0.85)]),
    (
        "ee",
        ExpressionKind::Viseme,
        &[
            (C::JAW_OPEN, 0.28),
            (C::MOUTH_STRETCH_LEFT, 0.6),
            (C::MOUTH_STRETCH_RIGHT, 0.6),
            (C::MOUTH_UPPER_UP_LEFT, 0.8),
            (C::MOUTH_UPPER_UP_RIGHT, 0.8),
            (C::MOUTH_SMILE_LEFT, 0.3),
            (C::MOUTH_SMILE_RIGHT, 0.3),
        ],
    ),
    ("oh", ExpressionKind::Viseme, &[(C::JAW_OPEN, 0.54), (C::MOUTH_FUNNEL, 0.65)]),
];

/// A weight for one expression.
pub type Weight<'a> = (&'a ExpressionId, f32);

/// A character's expression library.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ExpressionSet(pub BTreeMap<ExpressionId, Expression>);

/// A weight named an expression the set doesn't have.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no expression '{0}': define it in the face profile, or use one of the character's expressions")]
pub struct UnknownExpression(pub ExpressionId);

impl ExpressionSet {
    /// `base` plus every weighted expression, each scaled by its curve, then sanitised (clamped to the channel ranges).
    /// Allocation-free; expressions add, so `happy` + `aa` composes without a priority rule.
    ///
    /// # Errors
    /// [`UnknownExpression`] for the first weight whose expression doesn't exist.
    pub fn blend(&self, base: &ChannelBuf, weights: &[Weight<'_>]) -> Result<ChannelBuf, UnknownExpression> {
        let mut out = *base;
        for (id, w) in weights {
            let e = self.0.get(*id).ok_or_else(|| UnknownExpression((*id).clone()))?;
            let t = e.curve.eval(*w);
            for (c, d) in &e.deltas.0 {
                out[*c] += narrow(*d) * t;
            }
        }
        out.sanitize();
        Ok(out)
    }

    /// The starter library: the five visemes, `blink` and five moods, written in channels. They are first
    /// approximations of the prototype's expression table, re-expressed through the default `FaceMapping`
    /// (`jawOpen` for `mOpen`, smile and stretch channels for `mSmile` and `mWide`, and so on), for FE-04 to tune.
    pub fn starter() -> Self {
        let map = STARTER
            .into_iter()
            .filter_map(|(name, kind, deltas)| {
                let deltas = deltas.iter().fold(ChannelValues::default(), |m, (c, v)| m.with(*c, *v));
                Some((
                    ExpressionId::new(name).ok()?,
                    Expression { kind, deltas, curve: IntensityCurve::Linear, playback: Playback::default() },
                ))
            })
            .collect();
        Self(map)
    }

    /// The built-in library (FE-03): the five visemes (with the [`starter`](Self::starter)'s mouth openings) and `blink`, twenty-seven moods
    /// and eight accents, each with its intensity curve, tuned with the expression range test. It is data, in
    /// `library/expressions.toml` of this crate, compiled in. A new character's face starts with it.
    ///
    /// # Errors
    /// The parse error, if the compiled-in file does not read as an expression set (a test proves it does).
    pub fn library() -> Result<Self, String> {
        toml::from_str(LIBRARY).map_err(|e| format!("the built-in expression library does not read: {e}"))
    }
}

/// The built-in library's source.
const LIBRARY: &str = include_str!("../library/expressions.toml");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::FaceMapping;

    fn id(s: &str) -> ExpressionId {
        ExpressionId::new(s).unwrap()
    }

    #[test]
    fn curves_pass_through_0_and_1() {
        for c in [
            IntensityCurve::Linear,
            IntensityCurve::EaseIn,
            IntensityCurve::EaseOut,
            IntensityCurve::Smooth,
            IntensityCurve::Power { exponent: 2.5 },
        ] {
            assert_eq!((c.eval(0.0), c.eval(1.0)), (0.0, 1.0), "{c:?}");
            assert!(c.eval(0.5) > 0.0 && c.eval(0.5) < 1.0);
        }
        assert!(IntensityCurve::EaseIn.eval(0.5) < 0.5 && IntensityCurve::EaseOut.eval(0.5) > 0.5);
        assert_eq!(IntensityCurve::Linear.eval(7.0), 1.0, "weights clamp");
    }

    #[test]
    fn the_starter_set_has_every_viseme_and_blink() {
        let s = ExpressionSet::starter();
        assert_eq!(s.0.len(), 11);
        for v in VISEMES {
            assert_eq!(s.0[&id(v)].kind, ExpressionKind::Viseme, "{v}");
        }
        assert_eq!(s.0[&id("blink")].kind, ExpressionKind::Blink);
        for (name, e) in &s.0 {
            e.deltas.validate_ranges().unwrap_or_else(|m| panic!("{name}: {m}"));
        }
    }

    #[test]
    fn expressions_add_scale_and_clamp() {
        let s = ExpressionSet::starter();
        let base = ChannelBuf::NEUTRAL;
        let half = s.blend(&base, &[(&id("aa"), 0.5)]).unwrap();
        assert!((half.get(Channel::JAW_OPEN) - 0.37).abs() < 1e-6);
        let both = s.blend(&base, &[(&id("aa"), 1.0), (&id("surprised"), 1.0)]).unwrap();
        assert!((both.get(Channel::JAW_OPEN) - 1.0).abs() < 1e-6, "0.74 + 0.4 clamps to the channel maximum");
        assert_eq!(both.get(Channel::EYE_WIDE_LEFT), 1.0);
        assert_eq!(s.blend(&base, &[]).unwrap(), base);
    }

    #[test]
    fn unknown_expressions_are_errors_not_silence() {
        let e = ExpressionSet::starter().blend(&ChannelBuf::NEUTRAL, &[(&id("smug"), 1.0)]).unwrap_err();
        assert_eq!(e.0.as_str(), "smug");
    }

    #[test]
    fn the_starter_expressions_look_right_through_the_default_mapping() {
        let (s, m) = (ExpressionSet::starter(), FaceMapping::default());
        let p = |name: &str| m.apply(&s.blend(&ChannelBuf::NEUTRAL, &[(&id(name), 1.0)]).unwrap());
        // The prototype's visemes open the mouth by (aa, oh, ou, ee, ih) = 0.85, 0.62, 0.42, 0.32, 0.28.
        let opens: Vec<f32> = ["aa", "oh", "ou", "ee", "ih"].map(|v| p(v).mouth.open).to_vec();
        for (got, want) in opens.iter().zip([0.85, 0.62, 0.42, 0.32, 0.28]) {
            assert!((got - want).abs() < 0.03, "{opens:?}");
        }
        assert!(p("ee").mouth.teeth_upper > 0.9 && p("ou").mouth.round > 0.9 && p("aa").mouth.teeth_upper == 0.0);
        let happy = p("happy");
        assert_eq!(happy.left_eye.arc, 1.0);
        assert!(happy.mouth.smile_left > 0.9 && happy.blush > 0.3);
        let sad = p("sad");
        assert_eq!(sad.effects.tears, 1.0);
        assert!(sad.mouth.smile_left < -0.5 && sad.eye_tilt < 0.0);
        let angry = p("angry");
        assert!(angry.left_brow.tilt > 0.5 && angry.eye_tilt > 0.0 && angry.mouth.smile_left < 0.0);
        assert!(p("surprised").left_eye.open > 1.6);
        assert_eq!(p("blink").left_eye.close, 1.0);
    }

    #[test]
    fn the_library_reads_keeps_the_contract_and_stays_in_range() {
        let library = ExpressionSet::library().unwrap();
        let starter = ExpressionSet::starter();
        assert_eq!(library.0[&id("blink")], starter.0[&id("blink")]);
        let mapping = FaceMapping::default();
        let open = |set: &ExpressionSet, name: &str| {
            mapping.apply(&set.blend(&ChannelBuf::NEUTRAL, &[(&id(name), 1.0)]).unwrap()).mouth.open
        };
        for name in VISEMES {
            assert_eq!(library.0[&id(name)].kind, ExpressionKind::Viseme);
            assert_eq!(open(&library, name), open(&starter, name), "{name} keeps the lip-sync opening");
        }
        let moods = library.0.values().filter(|e| e.kind == ExpressionKind::Emotion).count();
        let accents = library.0.values().filter(|e| e.kind == ExpressionKind::Accent).count();
        assert!(moods >= 18, "{moods} moods");
        assert_eq!((moods, accents), (27, 8));
        for (name, e) in &library.0 {
            assert!(!e.deltas.0.is_empty(), "{name} moves nothing");
            for (c, v) in &e.deltas.0 {
                let (lo, hi) = c.range();
                assert!(*v != 0.0 && narrow(*v) >= lo && narrow(*v) <= hi, "{name}: {c} = {v}");
            }
        }
        // Any two expressions blend inside the channel ranges (the blend clamps, so it never fails or overflows).
        for a in library.0.keys() {
            for b in library.0.keys() {
                let out = library.blend(&ChannelBuf::NEUTRAL, &[(a, 1.0), (b, 1.0)]).unwrap();
                for info in CHANNELS {
                    let v = out.get(info.channel);
                    assert!(v >= info.min && v <= info.max, "{a} + {b}: {} = {v}", info.name);
                }
            }
        }
    }

    #[test]
    fn a_set_survives_a_json_round_trip() {
        let s = ExpressionSet::starter();
        let back: ExpressionSet = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        assert!(serde_json::from_str::<ExpressionSet>(r#"{"x":{"kind":"emotion","deltas":{"nope":1}}}"#).is_err());
    }

    #[test]
    fn channel_values_write_absolute_values_and_validate_ranges() {
        let v = ChannelValues::default().with(Channel::JAW_OPEN, 0.5);
        let mut b = ChannelBuf::NEUTRAL;
        v.write_into(&mut b);
        assert_eq!(b.get(Channel::JAW_OPEN), 0.5);
        assert!(
            ChannelValues::default().with(Channel::JAW_OPEN, 4.0).validate_ranges().unwrap_err().contains("jawOpen")
        );
        let schema = serde_json::to_value(schemars::schema_for!(ChannelValues)).unwrap();
        assert_eq!(schema["properties"]["jawOpen"]["maximum"], 1.0);
    }
}
