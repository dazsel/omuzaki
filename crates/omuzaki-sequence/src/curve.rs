//! Keyframes and easing: every animated value of a sequence is a [`Track`], either one value held for the whole time
//! or keys that each say when, what, and how the value travels there from the key before ([`Ease`]).
//!
//! The easing library covers what motion designers and animators reach for: the Penner families (sine, quad, cubic,
//! quart, expo, back, elastic, bounce) in, out and in-out; a hold for stepped (on-twos, snap) motion; any CSS
//! `cubic-bezier`; and a damped spring that settles like a physical one. Overshooting curves leave `0..=1`, so a value
//! can swing past its key and come back, which is what makes a lower third slam in or a head snap round.

use std::f32::consts::{PI, TAU};

use omuzaki_core::colour::Colour;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A named easing curve.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Curve {
    /// Constant speed.
    Linear,
    /// Smoothstep: eases in and out gently. The default.
    #[default]
    Smooth,
    /// Holds the previous value and jumps at the key: stepped motion, a cut.
    Hold,
    /// Cubic ease-in: starts slow.
    In,
    /// Cubic ease-out: arrives slow.
    Out,
    /// Cubic ease-in-out.
    InOut,
    /// Sine ease-in.
    SineIn,
    /// Sine ease-out.
    SineOut,
    /// Sine ease-in-out.
    SineInOut,
    /// Quadratic ease-in.
    QuadIn,
    /// Quadratic ease-out.
    QuadOut,
    /// Quadratic ease-in-out.
    QuadInOut,
    /// Quartic ease-in.
    QuartIn,
    /// Quartic ease-out.
    QuartOut,
    /// Quartic ease-in-out.
    QuartInOut,
    /// Exponential ease-in: almost still, then fast.
    ExpoIn,
    /// Exponential ease-out: a whip that lands softly (whip pans, slams).
    ExpoOut,
    /// Exponential ease-in-out.
    ExpoInOut,
    /// Pulls back before it goes (anticipation).
    BackIn,
    /// Overshoots the key and settles back (follow-through).
    BackOut,
    /// Anticipates and overshoots.
    BackInOut,
    /// Winds up with a growing wobble.
    ElasticIn,
    /// Springs past the key and wobbles to rest.
    ElasticOut,
    /// Bounces to rest like a dropped ball.
    BounceOut,
    /// Bounces off the start.
    BounceIn,
}

impl Curve {
    /// Every curve, for listings.
    pub const ALL: [Self; 25] = [
        Self::Linear,
        Self::Smooth,
        Self::Hold,
        Self::In,
        Self::Out,
        Self::InOut,
        Self::SineIn,
        Self::SineOut,
        Self::SineInOut,
        Self::QuadIn,
        Self::QuadOut,
        Self::QuadInOut,
        Self::QuartIn,
        Self::QuartOut,
        Self::QuartInOut,
        Self::ExpoIn,
        Self::ExpoOut,
        Self::ExpoInOut,
        Self::BackIn,
        Self::BackOut,
        Self::BackInOut,
        Self::ElasticIn,
        Self::ElasticOut,
        Self::BounceOut,
        Self::BounceIn,
    ];

    /// The curve's name as files and ops write it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Smooth => "smooth",
            Self::Hold => "hold",
            Self::In => "in",
            Self::Out => "out",
            Self::InOut => "in-out",
            Self::SineIn => "sine-in",
            Self::SineOut => "sine-out",
            Self::SineInOut => "sine-in-out",
            Self::QuadIn => "quad-in",
            Self::QuadOut => "quad-out",
            Self::QuadInOut => "quad-in-out",
            Self::QuartIn => "quart-in",
            Self::QuartOut => "quart-out",
            Self::QuartInOut => "quart-in-out",
            Self::ExpoIn => "expo-in",
            Self::ExpoOut => "expo-out",
            Self::ExpoInOut => "expo-in-out",
            Self::BackIn => "back-in",
            Self::BackOut => "back-out",
            Self::BackInOut => "back-in-out",
            Self::ElasticIn => "elastic-in",
            Self::ElasticOut => "elastic-out",
            Self::BounceOut => "bounce-out",
            Self::BounceIn => "bounce-in",
        }
    }

    /// The eased progress at `u` in `0..=1`: 0 at 0 and 1 at 1 for every curve; the back and elastic curves leave
    /// `0..=1` between.
    pub fn apply(self, u: f32) -> f32 {
        let u = u.clamp(0.0, 1.0);
        let in_out = |f: fn(f32) -> f32| if u < 0.5 { f(2.0 * u) / 2.0 } else { 1.0 - f(2.0 - 2.0 * u) / 2.0 };
        match self {
            Self::Linear => u,
            Self::Smooth => u * u * (3.0 - 2.0 * u),
            Self::Hold => {
                if u >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            Self::In => u * u * u,
            Self::Out => 1.0 - (1.0 - u).powi(3),
            Self::InOut => in_out(|x| x * x * x),
            Self::SineIn => 1.0 - (u * PI / 2.0).cos(),
            Self::SineOut => (u * PI / 2.0).sin(),
            Self::SineInOut => -((PI * u).cos() - 1.0) / 2.0,
            Self::QuadIn => u * u,
            Self::QuadOut => 1.0 - (1.0 - u) * (1.0 - u),
            Self::QuadInOut => in_out(|x| x * x),
            Self::QuartIn => u.powi(4),
            Self::QuartOut => 1.0 - (1.0 - u).powi(4),
            Self::QuartInOut => in_out(|x| x.powi(4)),
            Self::ExpoIn => expo_in(u),
            Self::ExpoOut => 1.0 - expo_in(1.0 - u),
            Self::ExpoInOut => in_out(expo_in),
            Self::BackIn => back_in(u),
            Self::BackOut => 1.0 - back_in(1.0 - u),
            Self::BackInOut => in_out(back_in),
            Self::ElasticIn => 1.0 - elastic_out(1.0 - u),
            Self::ElasticOut => elastic_out(u),
            Self::BounceOut => bounce_out(u),
            Self::BounceIn => 1.0 - bounce_out(1.0 - u),
        }
    }
}

fn expo_in(u: f32) -> f32 {
    if u <= 0.0 { 0.0 } else { 2f32.powf(10.0 * u - 10.0) }
}

/// Penner's back-in with the standard 10% overshoot.
fn back_in(u: f32) -> f32 {
    const C1: f32 = 1.701_58;
    (C1 + 1.0) * u * u * u - C1 * u * u
}

fn elastic_out(u: f32) -> f32 {
    if u <= 0.0 {
        return 0.0;
    }
    if u >= 1.0 {
        return 1.0;
    }
    2f32.powf(-10.0 * u) * ((u * 10.0 - 0.75) * (TAU / 3.0)).sin() + 1.0
}

fn bounce_out(u: f32) -> f32 {
    const N: f32 = 7.5625;
    const D: f32 = 2.75;
    if u < 1.0 / D {
        N * u * u
    } else if u < 2.0 / D {
        let x = u - 1.5 / D;
        N * x * x + 0.75
    } else if u < 2.5 / D {
        let x = u - 2.25 / D;
        N * x * x + 0.9375
    } else {
        let x = u - 2.625 / D;
        N * x * x + 0.984_375
    }
}

/// How a value travels from the key before to this one.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Ease {
    /// A named curve: `"back-out"`.
    Named(Curve),
    /// A CSS `cubic-bezier(x1, y1, x2, y2)`; `x1` and `x2` in `0..=1`, the `y`s free (beyond `0..=1` overshoots).
    Bezier {
        /// `[x1, y1, x2, y2]`.
        bezier: [f32; 4],
    },
    /// A damped spring released at the key before and settling on this one.
    Spring {
        /// The spring.
        spring: Spring,
    },
}

impl Default for Ease {
    fn default() -> Self {
        Self::Named(Curve::default())
    }
}

/// A damped spring as an easing: how fast it swings and how much it wobbles before it rests.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Spring {
    /// Swings per segment, `0.5..=20`: how many times it would oscillate undamped between the two keys.
    pub frequency: f32,
    /// Damping ratio, `0.05..=1`: 1 settles without overshoot, low values wobble long.
    pub damping: f32,
}

impl Ease {
    /// The eased progress at `u`.
    pub fn apply(&self, u: f32) -> f32 {
        match self {
            Self::Named(curve) => curve.apply(u),
            Self::Bezier { bezier } => cubic_bezier(*bezier, u.clamp(0.0, 1.0)),
            Self::Spring { spring } => spring_at(*spring, u.clamp(0.0, 1.0)),
        }
    }

    /// Checks the curve's numbers, naming `field`.
    ///
    /// # Errors
    /// A sentence naming the bad value.
    pub fn check(&self, field: &str) -> Result<(), String> {
        match self {
            Self::Named(_) => Ok(()),
            Self::Bezier { bezier: [x1, y1, x2, y2] } => {
                if [x1, x2].iter().all(|x| (0.0..=1.0).contains(*x)) && y1.is_finite() && y2.is_finite() {
                    Ok(())
                } else {
                    Err(format!("{field}: a bezier's x1 and x2 are 0 to 1 and its ys finite"))
                }
            }
            Self::Spring { spring } => {
                if (0.5..=20.0).contains(&spring.frequency) && (0.05..=1.0).contains(&spring.damping) {
                    Ok(())
                } else {
                    Err(format!("{field}: a spring's frequency is 0.5 to 20 and its damping 0.05 to 1"))
                }
            }
        }
    }
}

/// The y of a CSS cubic-bezier at x = `u`: Newton's method on x(s), with bisection when the slope is flat.
fn cubic_bezier([x1, y1, x2, y2]: [f32; 4], u: f32) -> f32 {
    let coord = |a: f32, b: f32, s: f32| {
        let r = 1.0 - s;
        3.0 * r * r * s * a + 3.0 * r * s * s * b + s * s * s
    };
    let slope = |a: f32, b: f32, s: f32| {
        let r = 1.0 - s;
        3.0 * r * r * a + 6.0 * r * s * (b - a) + 3.0 * s * s * (1.0 - b)
    };
    let mut s = u;
    for _ in 0..8 {
        let error = coord(x1, x2, s) - u;
        let d = slope(x1, x2, s);
        if error.abs() < 1e-6 {
            return coord(y1, y2, s);
        }
        if d.abs() < 1e-6 {
            break;
        }
        s = (s - error / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        s = 0.5 * (lo + hi);
        if coord(x1, x2, s) < u {
            lo = s;
        } else {
            hi = s;
        }
    }
    coord(y1, y2, s)
}

/// An underdamped (or critically damped) spring's step response, scaled so it lands exactly on 1 at `u = 1`.
fn spring_at(spring: Spring, u: f32) -> f32 {
    let omega = TAU * spring.frequency;
    let zeta = spring.damping.clamp(0.05, 1.0);
    let raw = |t: f32| {
        if zeta >= 0.999 {
            1.0 - (1.0 + omega * t) * (-omega * t).exp()
        } else {
            let wd = omega * (1.0 - zeta * zeta).sqrt();
            1.0 - (-zeta * omega * t).exp() * ((wd * t).cos() + zeta * omega / wd * (wd * t).sin())
        }
    };
    // What is left of the swing at the key is spread back over the segment, so the value meets the key exactly.
    let end = raw(1.0);
    raw(u) + (1.0 - end) * u
}

/// A value a track can carry: it can be blended.
pub trait Lerp: Copy {
    /// What an empty track reads as (zero, transparent black). [`Track::check`] refuses an empty track, so only an
    /// unchecked one ever shows it.
    const NEUTRAL: Self;

    /// `self` toward `other` by `t` (beyond `0..=1` extrapolates).
    #[must_use]
    fn lerp(self, other: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    const NEUTRAL: Self = 0.0;

    fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }
}

impl<const N: usize> Lerp for [f32; N] {
    const NEUTRAL: Self = [0.0; N];

    fn lerp(self, other: Self, t: f32) -> Self {
        let mut out = self;
        for (o, b) in out.iter_mut().zip(other) {
            *o += (b - *o) * t;
        }
        out
    }
}

impl Lerp for Colour {
    const NEUTRAL: Self = Self::rgba(0, 0, 0, 0);

    /// In sRGB-encoded values, as motion-graphics tools blend colours; alpha too.
    fn lerp(self, other: Self, t: f32) -> Self {
        let mix = |a: u8, b: u8| {
            let v = f32::from(a) + (f32::from(b) - f32::from(a)) * t;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to a byte")]
            let byte = v.round().clamp(0.0, 255.0) as u8;
            byte
        };
        Self::rgba(mix(self.r, other.r), mix(self.g, other.g), mix(self.b, other.b), mix(self.a, other.a))
    }
}

/// One key of a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Key<T> {
    /// When, in seconds from the start of what the track belongs to.
    pub t: f32,
    /// The value.
    pub v: T,
    /// How the value travels here from the key before; `smooth` when left out.
    #[serde(default, skip_serializing_if = "is_default_ease")]
    pub ease: Ease,
}

fn is_default_ease(ease: &Ease) -> bool {
    *ease == Ease::default()
}

impl<T> Key<T> {
    /// A key with the default ease.
    pub fn new(t: f32, v: T) -> Self {
        Self { t, v, ease: Ease::default() }
    }

    /// The same key with `ease`.
    #[must_use]
    pub fn eased(mut self, ease: Ease) -> Self {
        self.ease = ease;
        self
    }
}

/// An animated value: one value for all time, or keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Track<T> {
    /// One value, held.
    Hold(T),
    /// Keys in time order; before the first the first value holds, after the last the last.
    Keys(Vec<Key<T>>),
}

impl<T: Default> Default for Track<T> {
    fn default() -> Self {
        Self::Hold(T::default())
    }
}

impl<T: Lerp> Track<T> {
    /// A track holding `value`.
    pub fn hold(value: T) -> Self {
        Self::Hold(value)
    }

    /// The value at `t`.
    pub fn at(&self, t: f32) -> T {
        match self {
            Self::Hold(v) => *v,
            Self::Keys(keys) => sample(keys, t),
        }
    }

    /// Whether the value ever changes.
    pub fn is_animated(&self) -> bool {
        matches!(self, Self::Keys(keys) if keys.len() > 1)
    }

    /// Checks the keys are in time order with finite times, at least one, and every ease is sound.
    ///
    /// # Errors
    /// A sentence naming `field` and the first bad key.
    pub fn check(&self, field: &str) -> Result<(), String> {
        let Self::Keys(keys) = self else { return Ok(()) };
        if keys.is_empty() {
            return Err(format!("{field}: give at least one key, or a plain value"));
        }
        for (i, key) in keys.iter().enumerate() {
            if !key.t.is_finite() {
                return Err(format!("{field}[{i}]: the time must be a number of seconds"));
            }
            key.ease.check(&format!("{field}[{i}].ease"))?;
            if i > 0 && key.t < keys[i - 1].t {
                return Err(format!("{field}[{i}]: keys go in time order ({} comes after {})", key.t, keys[i - 1].t));
            }
        }
        Ok(())
    }

    /// Puts `key` in time order, replacing a key at the same time (within a millisecond). Turns a held value into keys.
    pub fn set_key(&mut self, key: Key<T>) {
        if let Self::Hold(_) = self {
            *self = Self::Keys(Vec::new());
        }
        let Self::Keys(keys) = self else { return };
        match keys.iter().position(|k| (k.t - key.t).abs() < 1e-3) {
            Some(i) => keys[i] = key,
            None => {
                let at = keys.iter().position(|k| k.t > key.t).unwrap_or(keys.len());
                keys.insert(at, key);
            }
        }
    }

    /// Removes the key at `t` (within a millisecond); false when there is none.
    pub fn remove_key(&mut self, t: f32) -> bool {
        let Self::Keys(keys) = self else { return false };
        let before = keys.len();
        keys.retain(|k| (k.t - t).abs() >= 1e-3);
        keys.len() != before
    }
}

/// The value of time-ordered `keys` at `t`.
fn sample<T: Lerp>(keys: &[Key<T>], t: f32) -> T {
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
        debug_assert!(false, "a checked track has keys");
        return T::NEUTRAL;
    };
    if t <= first.t {
        return first.v;
    }
    if t >= last.t {
        return last.v;
    }
    let next = keys.partition_point(|k| k.t <= t);
    let (a, b) = (&keys[next - 1], &keys[next]);
    let span = b.t - a.t;
    if span <= 0.0 {
        return b.v;
    }
    a.v.lerp(b.v, b.ease.apply((t - a.t) / span))
}

/// Smooth 1D value noise in `-1..=1` at `t` seconds, `frequency` wiggles a second, picked by `seed`: handheld drift,
/// camera shake, a nervous tremble.
pub fn wiggle(t: f32, frequency: f32, seed: u32) -> f32 {
    let x = t * frequency.max(0.0);
    let i = x.floor();
    let f = x - i;
    #[allow(clippy::cast_possible_truncation, reason = "a lattice index of a few minutes of wiggle")]
    let cell = i as i32;
    let a = lattice(cell, seed);
    let b = lattice(cell + 1, seed);
    let s = f * f * (3.0 - 2.0 * f);
    a + (b - a) * s
}

/// A hashed lattice value in `-1..=1`.
fn lattice(cell: i32, seed: u32) -> f32 {
    #[allow(clippy::cast_sign_loss, reason = "the cell's bits are hashed")]
    let mut h = (cell as u32).wrapping_mul(0x8da6_b343) ^ seed.wrapping_mul(0x9e37_79b9);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    #[allow(clippy::cast_precision_loss, reason = "24 bits are exact")]
    let unit = (h >> 8) as f32 / 16_777_216.0;
    unit * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_curve_starts_at_0_and_ends_at_1() {
        for curve in Curve::ALL {
            assert!(curve.apply(0.0).abs() < 1e-4, "{} at 0", curve.name());
            assert!((curve.apply(1.0) - 1.0).abs() < 1e-4, "{} at 1", curve.name());
        }
    }

    #[test]
    fn names_round_trip_through_serde() {
        for curve in Curve::ALL {
            let json = serde_json::to_value(curve).unwrap();
            assert_eq!(json, serde_json::Value::String(curve.name().to_owned()));
            assert_eq!(serde_json::from_value::<Curve>(json).unwrap(), curve);
        }
    }

    #[test]
    fn back_out_overshoots_and_in_anticipates() {
        let peak = (1..100).map(|i| Curve::BackOut.apply(i as f32 / 100.0)).fold(0.0f32, f32::max);
        assert!(peak > 1.05, "back-out peaks at {peak}");
        let dip = (1..100).map(|i| Curve::BackIn.apply(i as f32 / 100.0)).fold(1.0f32, f32::min);
        assert!(dip < -0.05, "back-in dips to {dip}");
    }

    #[test]
    fn a_hold_jumps_at_the_key() {
        let track = Track::Keys(vec![Key::new(0.0, 1.0), Key::new(1.0, 5.0).eased(Ease::Named(Curve::Hold))]);
        assert!((track.at(0.99) - 1.0).abs() < 1e-6);
        assert!((track.at(1.0) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn linear_bezier_is_the_identity_and_ease_matches_css() {
        for i in 0..=10 {
            let u = i as f32 / 10.0;
            assert!((cubic_bezier([0.0, 0.0, 1.0, 1.0], u) - u).abs() < 1e-4);
        }
        // CSS `ease` is (0.25, 0.1, 0.25, 1): at x = 0.5 it is about 0.8024.
        assert!((cubic_bezier([0.25, 0.1, 0.25, 1.0], 0.5) - 0.8024).abs() < 2e-3);
    }

    #[test]
    fn a_spring_lands_on_its_key_and_wobbles_when_light() {
        let light = Spring { frequency: 3.0, damping: 0.2 };
        assert!((spring_at(light, 1.0) - 1.0).abs() < 1e-5);
        let peak = (1..100).map(|i| spring_at(light, i as f32 / 100.0)).fold(0.0f32, f32::max);
        assert!(peak > 1.2, "a light spring overshoots: {peak}");
        let stiff = Spring { frequency: 3.0, damping: 1.0 };
        let peak = (1..100).map(|i| spring_at(stiff, i as f32 / 100.0)).fold(0.0f32, f32::max);
        assert!(peak <= 1.0 + 1e-4, "a critically damped spring doesn't: {peak}");
    }

    #[test]
    fn tracks_hold_before_and_after_and_blend_between() {
        let track = Track::Keys(vec![
            Key::new(1.0, [0.0, 10.0]),
            Key::new(2.0, [10.0, 20.0]).eased(Ease::Named(Curve::Linear)),
        ]);
        assert_eq!(track.at(0.0), [0.0, 10.0]);
        assert_eq!(track.at(1.5), [5.0, 15.0]);
        assert_eq!(track.at(9.0), [10.0, 20.0]);
    }

    #[test]
    fn set_key_keeps_order_and_replaces_at_the_same_time() {
        let mut track = Track::hold(0.0f32);
        track.set_key(Key::new(2.0, 2.0));
        track.set_key(Key::new(1.0, 1.0));
        track.set_key(Key::new(2.0, 3.0));
        let Track::Keys(keys) = &track else { panic!("keys") };
        assert_eq!(keys.iter().map(|k| (k.t, k.v)).collect::<Vec<_>>(), vec![(1.0, 1.0), (2.0, 3.0)]);
        assert!(track.remove_key(1.0));
        assert!(!track.remove_key(5.0));
        assert!(track.check("x").is_ok());
    }

    #[test]
    fn out_of_order_keys_are_refused() {
        let track = Track::Keys(vec![Key::new(2.0, 0.0f32), Key::new(1.0, 1.0)]);
        assert!(track.check("x").unwrap_err().contains("time order"));
    }

    #[test]
    fn tracks_read_as_a_value_or_keys() {
        let held: Track<f32> = serde_json::from_str("3.5").unwrap();
        assert_eq!(held, Track::Hold(3.5));
        let keyed: Track<[f32; 3]> =
            serde_json::from_str(r#"[{"t": 0, "v": [0, 0, 0]}, {"t": 1, "v": [1, 2, 3], "ease": "back-out"}]"#).unwrap();
        assert!(keyed.is_animated());
        let spring: Ease = serde_json::from_str(r#"{"spring": {"frequency": 2, "damping": 0.3}}"#).unwrap();
        assert!(matches!(spring, Ease::Spring { .. }));
    }

    #[test]
    fn wiggle_is_smooth_and_bounded() {
        let mut last = wiggle(0.0, 3.0, 7);
        for i in 1..1000 {
            let v = wiggle(i as f32 / 300.0, 3.0, 7);
            assert!((-1.0..=1.0).contains(&v));
            assert!((v - last).abs() < 0.1, "a jump at {i}");
            last = v;
        }
    }
}
