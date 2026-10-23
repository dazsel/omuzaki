//! Scores and timelines (spec §8 clips and scores, AP-04 intents): keys per channel, plus cues for things that
//! happen rather than things that hold a value.
//!
//! A [`Score`] is **cold data**: serialisable, editable by people and agents, validated on load. Playing it means
//! [`compiling`](Score::compile) it once into a [`CompiledScore`], whose [`sample`](CompiledScore::sample) is a pure,
//! allocation-free function of time. Because sampling never depends on earlier calls, scrubbing a timeline,
//! rendering frame 4000 first, and playing live all give the same numbers (spec §4 determinism).
//!
//! ## Key semantics: transitions, not waypoints
//!
//! A [`Key`] says "**from this time**, ease from wherever the channel is now to this value, over `over` seconds".
//! That is how the prototype's performance score was transcribed (a change *starts* at the time you see it start), and
//! it makes mid-flight changes continuous: a new key that arrives before the last transition finished starts from the
//! value reached so far, with no jump. `over = 0` snaps. Standard waypoint keys are the special case where each key's
//! time is the arrival time minus its `over`; editors convert when showing a conventional curve.

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::blend::Layer;
use crate::channel::{Channel, ChannelMask};

omuzaki_core::slug_id!(
    /// A clip in the pose library or an imported motion (spec RG-06, RG-08).
    ClipId
);

omuzaki_core::slug_id!(
    /// A named expression: a blend of channels with an intensity curve (spec FE-03).
    ExpressionId
);

/// The shape of a transition from `0` to `1`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Ease {
    /// Constant speed.
    Linear,
    /// Slow in, slow out (smoothstep). For spins and camera-like moves.
    Smooth,
    /// Fast start, slow settle: the face "snaps and settles". The default for expressions.
    #[default]
    OutCubic,
    /// Cubic in and out.
    InOutCubic,
}

impl Ease {
    /// The eased fraction for a linear fraction `u`, with `u` clamped to `0..=1`.
    pub fn apply(self, u: f32) -> f32 {
        let u = u.clamp(0.0, 1.0);
        match self {
            Self::Linear => u,
            Self::Smooth => u * u * (3.0 - 2.0 * u),
            Self::OutCubic => 1.0 - (1.0 - u).powi(3),
            Self::InOutCubic => {
                if u < 0.5 {
                    4.0 * u * u * u
                } else {
                    1.0 - (-2.0 * u + 2.0).powi(3) / 2.0
                }
            }
        }
    }
}

/// One transition on a channel.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Key {
    /// When the transition starts, seconds from the score's start.
    pub time: Seconds,
    /// The value it ends at (in the channel's own unit).
    pub value: f32,
    /// How long it takes, seconds; `0` snaps.
    #[serde(default)]
    pub over: f32,
    /// Its shape.
    #[serde(default)]
    pub ease: Ease,
}

/// The keys of one channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelTrack {
    /// The channel it drives.
    pub channel: Channel,
    /// The value before the first key (the channel's resting value in this score).
    #[serde(default)]
    pub initial: f32,
    /// The keys, strictly increasing in time.
    pub keys: Vec<Key>,
}

/// Something that happens at a moment, rather than a value that is held.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CueKind {
    /// Play a clip from the pose library.
    Clip {
        /// Which clip.
        clip: ClipId,
        /// How strongly it blends over the procedural pose, `0..=1`.
        #[serde(default = "one")]
        weight: f32,
        /// Seconds to fade in and out.
        #[serde(default = "default_fade")]
        fade_s: f32,
    },
    /// Show a named expression.
    Expression {
        /// Which expression.
        expression: ExpressionId,
        /// Strength, `0..=1`.
        #[serde(default = "one")]
        intensity: f32,
        /// How long to hold it before fading out, seconds.
        #[serde(default)]
        hold_s: f32,
        /// Seconds to fade in and out.
        #[serde(default = "default_fade")]
        fade_s: f32,
    },
    /// A named marker other systems react to (a sound, a scene change, a prop).
    Marker {
        /// The marker's name.
        name: String,
    },
    /// Look at something for a while (spec FE-07): the eyes go first and the head follows if it must, then the gaze
    /// goes back to the character's own target (`omuzaki_anim::gaze::GazeCoordinator::look`).
    #[non_exhaustive]
    Look {
        /// What to look at.
        target: LookTarget,
        /// How long to hold the look, seconds; 0 holds until the next look.
        #[serde(default)]
        hold_s: f32,
    },
}

impl CueKind {
    /// A [`CueKind::Look`] at `target` for `hold_s` seconds.
    pub fn look(target: LookTarget, hold_s: f32) -> Self {
        Self::Look { target, hold_s }
    }
}

/// What a [`CueKind::Look`] aims the eyes at. The stage resolves it each frame, so a moving target is followed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "at", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LookTarget {
    /// The viewer: the active camera.
    Camera,
    /// Straight ahead: no target, so the eyes and the head go back to rest.
    Ahead,
    /// A fixed point, world space, metres.
    #[non_exhaustive]
    Point {
        /// Where.
        position: [f32; 3],
    },
    /// Something in the scene by name: another character, a prop, the chat overlay.
    #[non_exhaustive]
    Entity {
        /// Its name in the scene.
        name: String,
    },
}

impl LookTarget {
    /// A [`LookTarget::Point`].
    pub fn point(position: [f32; 3]) -> Self {
        Self::Point { position }
    }

    /// A [`LookTarget::Entity`].
    pub fn entity(name: impl Into<String>) -> Self {
        Self::Entity { name: name.into() }
    }
}

fn one() -> f32 {
    1.0
}

fn default_fade() -> f32 {
    0.2
}

/// A cue at a time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Cue {
    /// When it fires, seconds from the score's start.
    pub time: Seconds,
    /// What it does.
    #[serde(flatten)]
    pub kind: CueKind,
}

/// A performance on a timeline: channel tracks and cues. Cold data; see the module docs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Score {
    /// Length, seconds. Sampling past it holds the last values.
    pub duration: Seconds,
    /// One track per driven channel.
    pub tracks: Vec<ChannelTrack>,
    /// Cues in any order; compiling sorts them by time.
    #[serde(default)]
    pub cues: Vec<Cue>,
}

/// Why a score can't be compiled. Each says which track or cue to fix.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScoreError {
    /// Two tracks drive the same channel.
    #[error("channel {0} has two tracks; merge them")]
    DuplicateTrack(Channel),
    /// Keys aren't strictly increasing in time.
    #[error("track {channel}: key {index} is not later than the key before it")]
    UnsortedKeys {
        /// The track's channel.
        channel: Channel,
        /// The offending key.
        index: usize,
    },
    /// A time, value or duration is NaN or infinite, or `over` is negative.
    #[error("{what} has a value that isn't finite (or a negative duration)")]
    NotFinite {
        /// Where.
        what: String,
    },
}

/// A score ready to play: per-track transitions with their start values precomputed.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledScore {
    duration: Seconds,
    mask: ChannelMask,
    tracks: Vec<CompiledTrack>,
    cues: Vec<Cue>,
}

#[derive(Clone, Debug, PartialEq)]
struct CompiledTrack {
    channel: Channel,
    initial: f32,
    segments: Vec<Segment>,
}

#[derive(Copy, Clone, Debug, PartialEq)]
struct Segment {
    start: f64,
    from: f32,
    to: f32,
    over: f32,
    ease: Ease,
}

impl Segment {
    fn value_at(&self, t: f64) -> f32 {
        // A zero-length transition is a snap. `over` is validated non-negative and finite, so the division is safe.
        #[allow(clippy::cast_possible_truncation)] // a fraction in 0..1 after the clamp; f32 is plenty
        let u = if self.over > 0.0 { ((t - self.start) / f64::from(self.over)).clamp(0.0, 1.0) as f32 } else { 1.0 };
        self.from + (self.to - self.from) * self.ease.apply(u)
    }
}

impl Score {
    /// Validates and compiles the score.
    ///
    /// # Errors
    /// [`ScoreError`] for duplicate tracks, unsorted keys, or non-finite numbers.
    pub fn compile(&self) -> Result<CompiledScore, ScoreError> {
        let not_finite = |what: String| ScoreError::NotFinite { what };
        if !self.duration.0.is_finite() {
            return Err(not_finite("the score duration".into()));
        }
        let mut mask = ChannelMask::EMPTY;
        let mut tracks = Vec::with_capacity(self.tracks.len());
        for track in &self.tracks {
            if mask.contains(track.channel) {
                return Err(ScoreError::DuplicateTrack(track.channel));
            }
            mask.insert(track.channel);
            tracks.push(compile_track(track)?);
        }
        let mut cues = self.cues.clone();
        if let Some(bad) = cues.iter().position(|c| !c.time.0.is_finite()) {
            return Err(not_finite(format!("cue {bad}")));
        }
        cues.sort_by(|a, b| a.time.0.total_cmp(&b.time.0)); // stable: cues at the same time keep their order
        Ok(CompiledScore { duration: self.duration, mask, tracks, cues })
    }
}

fn compile_track(track: &ChannelTrack) -> Result<CompiledTrack, ScoreError> {
    if !track.initial.is_finite() {
        return Err(ScoreError::NotFinite { what: format!("the initial value of {}", track.channel) });
    }
    let mut segments: Vec<Segment> = Vec::with_capacity(track.keys.len());
    for (index, key) in track.keys.iter().enumerate() {
        if !(key.time.0.is_finite() && key.value.is_finite() && key.over.is_finite() && key.over >= 0.0) {
            return Err(ScoreError::NotFinite { what: format!("key {index} of {}", track.channel) });
        }
        if index > 0 && key.time.0 <= track.keys[index - 1].time.0 {
            return Err(ScoreError::UnsortedKeys { channel: track.channel, index });
        }
        // Where the channel is when this key starts: the previous transition, possibly still in flight.
        let from = segments.last().map_or(track.initial, |prev| prev.value_at(key.time.0));
        segments.push(Segment { start: key.time.0, from, to: key.value, over: key.over, ease: key.ease });
    }
    Ok(CompiledTrack { channel: track.channel, initial: track.initial, segments })
}

impl CompiledScore {
    /// The score's length.
    pub fn duration(&self) -> Seconds {
        self.duration
    }

    /// The channels the score drives.
    pub fn mask(&self) -> ChannelMask {
        self.mask
    }

    /// The cues, sorted by time.
    pub fn cues(&self) -> &[Cue] {
        &self.cues
    }

    /// Writes every track's value at `t` into `out.values` and sets `out.mask` to the driven channels. The layer's
    /// weight and mode are the player's business (it fades the score in and out). Pure and allocation-free; `t`
    /// outside the score holds the first or last value.
    pub fn sample(&self, t: Seconds, out: &mut Layer) {
        out.mask = self.mask;
        for track in &self.tracks {
            let i = track.segments.partition_point(|s| s.start <= t.0);
            out.values[track.channel] = match i.checked_sub(1) {
                Some(last) => track.segments[last].value_at(t.0),
                None => track.initial,
            };
        }
    }

    /// The cues that fire in `(after, up_to]`, in order: what a player collects each frame with the previous and the
    /// current time. A borrowed slice, so nothing is allocated. Seeking backwards or scrubbing should not fire cues;
    /// the player decides that, not the score.
    pub fn fired(&self, after: Seconds, up_to: Seconds) -> &[Cue] {
        let start = self.cues.partition_point(|c| c.time.0 <= after.0);
        let end = self.cues.partition_point(|c| c.time.0 <= up_to.0);
        &self.cues[start..end.max(start)]
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // tests compare values that are copied or exactly computed, and cast small loop counters
mod tests {
    use omuzaki_core::rng::Rng;

    use super::*;
    use crate::blend::BlendMode;

    fn key(time: f64, value: f32, over: f32) -> Key {
        Key { time: Seconds(time), value, over, ease: Ease::Linear }
    }

    fn score(keys: Vec<Key>) -> Score {
        Score {
            duration: Seconds(10.0),
            tracks: vec![ChannelTrack { channel: Channel::JAW_OPEN, initial: 0.0, keys }],
            cues: vec![],
        }
    }

    fn value(s: &CompiledScore, t: f64) -> f32 {
        let mut layer = Layer::absent(BlendMode::Override);
        s.sample(Seconds(t), &mut layer);
        layer.values[Channel::JAW_OPEN]
    }

    #[test]
    fn eases_hit_their_endpoints_and_stay_in_range() {
        for ease in [Ease::Linear, Ease::Smooth, Ease::OutCubic, Ease::InOutCubic] {
            assert_eq!((ease.apply(0.0), ease.apply(1.0)), (0.0, 1.0), "{ease:?}");
            assert_eq!((ease.apply(-1.0), ease.apply(2.0)), (0.0, 1.0));
            let mut last = 0.0;
            for i in 0..=100 {
                let v = ease.apply(i as f32 / 100.0);
                assert!(v >= last - 1e-6, "{ease:?} is monotonic");
                last = v;
            }
        }
        assert!(Ease::OutCubic.apply(0.5) > 0.8, "out-cubic is most of the way there at half time");
        assert_eq!(Ease::default(), Ease::OutCubic);
    }

    #[test]
    fn a_key_starts_a_transition_from_the_current_value() {
        let s = score(vec![key(1.0, 1.0, 0.5), key(3.0, 0.0, 0.0)]).compile().unwrap();
        assert_eq!(value(&s, 0.0), 0.0, "before the first key: the initial value");
        assert_eq!(value(&s, 1.0), 0.0);
        assert!((value(&s, 1.25) - 0.5).abs() < 1e-6, "half-way through a linear 0.5 s transition");
        assert_eq!(value(&s, 2.0), 1.0, "held");
        assert_eq!(value(&s, 3.0), 0.0, "over = 0 snaps");
        assert_eq!(value(&s, 99.0), 0.0, "past the end holds the last value");
    }

    #[test]
    fn a_key_arriving_mid_transition_continues_from_where_the_value_is() {
        // Rising 0 -> 1 over 1 s from t=0; at t=0.5 (value 0.5) a new key sends it to 0 over 0.5 s.
        let s = score(vec![key(0.0, 1.0, 1.0), key(0.5, 0.0, 0.5)]).compile().unwrap();
        assert!((value(&s, 0.5) - 0.5).abs() < 1e-6, "no jump at the new key");
        assert!((value(&s, 0.75) - 0.25).abs() < 1e-6);
        assert_eq!(value(&s, 1.0), 0.0);
    }

    #[test]
    fn sampling_is_a_pure_function_of_time() {
        let keys: Vec<Key> = (0..40)
            .map(|i| Key {
                time: Seconds(f64::from(i) * 0.37),
                value: (i % 7) as f32 / 7.0,
                over: 0.4,
                ease: Ease::OutCubic,
            })
            .collect();
        let s = score(keys).compile().unwrap();
        let mut rng = Rng::new(5);
        let times: Vec<f64> = (0..200).map(|_| f64::from(rng.range_f32(-1.0, 16.0))).collect();
        let forward: Vec<f32> = times.iter().map(|&t| value(&s, t)).collect();
        let backward: Vec<f32> = times.iter().rev().map(|&t| value(&s, t)).collect();
        assert!(forward.iter().eq(backward.iter().rev()), "order of sampling must not matter (scrub-proof)");
    }

    #[test]
    fn sample_sets_the_mask_to_the_driven_channels_only() {
        let mut sc = score(vec![key(0.0, 0.5, 0.0)]);
        sc.tracks.push(ChannelTrack { channel: Channel::HEAD_YAW, initial: 0.1, keys: vec![] });
        let compiled = sc.compile().unwrap();
        let mut layer = Layer::absent(BlendMode::Override);
        compiled.sample(Seconds(1.0), &mut layer);
        assert_eq!(layer.mask, ChannelMask::of(Channel::JAW_OPEN) | ChannelMask::of(Channel::HEAD_YAW));
        assert_eq!((layer.values[Channel::JAW_OPEN], layer.values[Channel::HEAD_YAW]), (0.5, 0.1));
        assert_eq!(layer.weight, 0.0, "weight is the player's");
    }

    #[test]
    fn bad_scores_are_refused_with_the_place_to_fix() {
        let unsorted = score(vec![key(1.0, 1.0, 0.0), key(1.0, 0.0, 0.0)]);
        assert_eq!(unsorted.compile(), Err(ScoreError::UnsortedKeys { channel: Channel::JAW_OPEN, index: 1 }));
        let mut dup = score(vec![]);
        dup.tracks.push(dup.tracks[0].clone());
        assert_eq!(dup.compile(), Err(ScoreError::DuplicateTrack(Channel::JAW_OPEN)));
        assert!(matches!(score(vec![key(0.0, f32::NAN, 0.0)]).compile(), Err(ScoreError::NotFinite { .. })));
        assert!(matches!(score(vec![key(0.0, 1.0, -0.1)]).compile(), Err(ScoreError::NotFinite { .. })));
        let mut inf = score(vec![]);
        inf.duration = Seconds(f64::INFINITY);
        assert!(matches!(inf.compile(), Err(ScoreError::NotFinite { .. })));
    }

    fn marker(time: f64, name: &str) -> Cue {
        Cue { time: Seconds(time), kind: CueKind::Marker { name: name.into() } }
    }

    #[test]
    fn cues_are_sorted_and_fire_once_in_a_half_open_window() {
        let mut sc = score(vec![]);
        sc.cues = vec![marker(2.0, "c"), marker(1.0, "a"), marker(1.0, "b"), marker(3.0, "d")];
        let s = sc.compile().unwrap();
        let names = |cues: &[Cue]| cues.iter().map(|c| format!("{:?}", c.kind)).collect::<Vec<_>>().join(",");
        assert_eq!(s.cues().len(), 4);
        assert_eq!(s.fired(Seconds(0.0), Seconds(1.0)).len(), 2, "a and b fire at 1.0");
        assert_eq!(s.fired(Seconds(1.0), Seconds(2.0)).len(), 1, "after 1.0, up to and including 2.0");
        assert_eq!(s.fired(Seconds(3.0), Seconds(9.0)).len(), 0);
        assert_eq!(s.fired(Seconds(2.5), Seconds(0.0)).len(), 0, "a backwards window fires nothing");
        // Splitting a span at any point fires every cue exactly once.
        let (first, second) = (s.fired(Seconds(-1.0), Seconds(1.5)), s.fired(Seconds(1.5), Seconds(9.0)));
        assert_eq!(first.len() + second.len(), 4);
        assert!(names(first).starts_with("Marker { name: \"a\" }"), "ties keep their authored order");
    }

    #[test]
    fn score_round_trips_through_json_with_defaults() {
        let json = r#"{
            "duration": 4.0,
            "tracks": [{"channel": "mouthSmileLeft", "keys": [{"time": 0.5, "value": 0.8, "over": 0.1}]}],
            "cues": [
                {"time": 1.0, "type": "clip", "clip": "wave"},
                {"time": 2.0, "type": "expression", "expression": "smug", "hold_s": 1.5},
                {"time": 3.0, "type": "look", "target": {"at": "entity", "name": "chat"}, "hold_s": 2.0},
                {"time": 3.0, "type": "marker", "name": "scene-2"}
            ]
        }"#;
        let sc: Score = serde_json::from_str(json).unwrap();
        assert_eq!(sc.tracks[0].keys[0].ease, Ease::OutCubic);
        assert!(matches!(&sc.cues[0].kind, CueKind::Clip { weight, fade_s, .. } if *weight == 1.0 && *fade_s == 0.2));
        assert_eq!(sc.cues[2].kind, CueKind::look(LookTarget::entity("chat"), 2.0));
        assert_eq!(serde_json::from_str::<Score>(&serde_json::to_string(&sc).unwrap()).unwrap(), sc);
        assert!(serde_json::from_str::<Score>(r#"{"duration":1,"tracks":[{"channel":"jawOpne","keys":[]}]}"#).is_err());
        assert!(serde_json::from_str::<ClipId>("\"Not A Slug\"").is_err());
    }
}
