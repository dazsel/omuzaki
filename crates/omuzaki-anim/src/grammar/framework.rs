//! What a grammar rule is, what it sees of one frame, and how it registers itself.

use std::collections::BTreeMap;
use std::fmt::Debug;

use omuzaki_anim_api::{Channel, ChannelBuf, ChannelMask, Cues, Tick};
use omuzaki_core::id::{Slug, SlugError};

/// Registry plumbing for `register_grammar_rule!`: the macro expands in the caller's crate, so it reaches `inventory`
/// through this path instead of requiring the caller to depend on it.
#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;

/// A frame longer than this counts as this long, the same cap the life layer applies, so a stall cannot make a rule
/// that reads [`RuleCtx::dt`] jump.
pub const MAX_DT: f32 = 0.25;

/// One anime grammar rule (spec FE-05): a small decision on the channels after the life layer, such as "a blink
/// while smiling closes into `^`". A character gets its own instance of each rule, so a rule may keep state.
///
/// A rule keeps these promises, which is what lets the stage run any set of them:
///
/// 1. **It writes only through [`RuleCtx::put`].** That is where a human-driven channel is refused, the character's
///    strength is applied and the legal range is kept.
/// 2. **It allocates and locks nothing in [`step`](GrammarRule::step).** The stage is on the real-time path.
/// 3. **It is deterministic.** Its output is a function of its state and the values it reads, never of a clock or a
///    global random source.
/// 4. **Its memory follows the incoming values even while its writes are refused.** When a person drives the eyes,
///    [`put`](RuleCtx::put) does nothing, but the rule still reads and remembers as usual, so on hand-back it
///    carries on from the truth and nothing jumps.
pub trait GrammarRule: Send + Debug {
    /// The rule's slug: the key of its switches in `FaceProfile.grammar` (`smile-blink-arc`). Fixed for the type.
    fn id(&self) -> &'static str;

    /// Where the rule runs: lower first, and rules with equal priorities in id order. A rule that reads what another
    /// writes goes after it.
    fn priority(&self) -> i32;

    /// Runs the rule for one frame.
    fn step(&mut self, ctx: &mut RuleCtx<'_>);

    /// Forgets everything, as a newly constructed rule would. Called when the character switches the rule off, so it
    /// starts clean when it is switched on again.
    fn reset(&mut self);

    /// Takes the character's own numbers for this rule (thresholds and the like, `FaceProfile.grammar.<id>.params`).
    /// The stage calls it once when it is built and again whenever its config changes, never from [`step`](GrammarRule::step).
    ///
    /// A rule reads the numbers it documents, keeps the rest of its state as it is (a hysteresis latch does not drop
    /// because a threshold moved) and ignores names it does not know. The default takes none.
    fn configure(&mut self, params: RuleParams<'_>) {
        let _ = params;
    }
}

/// A rule's own numbers by name, as the character's face profile sets them. Each rule documents the names it reads.
#[derive(Copy, Clone, Debug)]
pub struct RuleParams<'a>(&'a BTreeMap<Slug, f64>);

impl<'a> RuleParams<'a> {
    /// The numbers of one rule's entry.
    pub(super) fn new(params: &'a BTreeMap<Slug, f64>) -> Self {
        Self(params)
    }

    /// The number called `name`, kept within `lo..=hi`. A name the character does not set, or sets to something that is
    /// not a finite number, gives `default`. Meant for [`GrammarRule::configure`]: it walks the (tiny) map and
    /// compares names, so it never allocates, but it is not a per-frame lookup.
    #[allow(clippy::cast_possible_truncation)] // a value clamped to the f32 range `lo..=hi` first
    pub fn number(&self, name: &str, default: f32, lo: f32, hi: f32) -> f32 {
        let set = self.0.iter().find(|(key, _)| key.as_str() == name).map(|(_, value)| *value);
        match set {
            Some(value) if value.is_finite() => value.clamp(f64::from(lo), f64::from(hi)) as f32,
            _ => default,
        }
    }
}

/// What a rule sees and may change in one frame. The stage builds it; a rule only uses it.
#[derive(Debug)]
pub struct RuleCtx<'a> {
    values: &'a mut ChannelBuf,
    taken: ChannelMask,
    strength: f32,
    tick: Tick,
    dt: f32,
    loudness: f32,
}

impl<'a> RuleCtx<'a> {
    /// A context over `values`, where the channels in `taken` belong to a person, with the frame's audio `cues`.
    pub(super) fn new(values: &'a mut ChannelBuf, taken: ChannelMask, strength: f32, tick: Tick, cues: &Cues) -> Self {
        let dt = if tick.dt.is_finite() { tick.dt.clamp(0.0, MAX_DT) } else { 0.0 };
        let unit = |x: f32| if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) };
        let loudness = unit(cues.loudness).max(unit(cues.laugh));
        Self { values, taken, strength, tick, dt, loudness }
    }

    /// How loud the person is this frame, `0..=1`, as the life layer's laugh hears it: the speech loudness, raised to
    /// the audio laughter probability when that is higher ([`Cues`]). Zero when there is no audio. A non-finite cue
    /// reads as `0`.
    pub fn loudness(&self) -> f32 {
        self.loudness
    }

    /// The frame's tick, as the pipeline gave it.
    pub fn tick(&self) -> Tick {
        self.tick
    }

    /// Seconds since the last frame: finite, never negative, at most [`MAX_DT`]. Unlike the life layer, a rule still
    /// runs when this is zero, because a rule is a function of the bus and a paused or scrubbed frame must show the
    /// same grammar as a played one.
    pub fn dt(&self) -> f32 {
        self.dt
    }

    /// What `channel` holds now, after the life layer and any rule that ran earlier. A non-finite value reads as `0`
    /// (every channel's neutral), so a rule never has to guard its arithmetic.
    pub fn get(&self, channel: Channel) -> f32 {
        let value = self.values[channel];
        if value.is_finite() { value } else { 0.0 }
    }

    /// Moves `channel` toward `target` by the rule's strength: not at all at `0`, all the way at `1`, past it (up to
    /// the channel's range) at `2`. A rule therefore states the full-strength result and the character dials it.
    ///
    /// Does nothing if a person drives `channel`, if `target` is not finite, or if the strength is `0`. Call it at
    /// most once per channel per frame: a second call would move from where the first left off.
    pub fn put(&mut self, channel: Channel, target: f32) {
        if self.strength <= 0.0 || !target.is_finite() || self.taken.contains(channel) {
            return;
        }
        let from = self.get(channel);
        let (lo, hi) = channel.range();
        self.values[channel] = (from + self.strength * (target - from)).clamp(lo, hi);
    }
}

/// A rule that can be created for every character: the entry `register_grammar_rule!` submits.
#[derive(Copy, Clone)]
pub struct GrammarRuleRegistration {
    create: fn() -> Box<dyn GrammarRule>,
}

inventory::collect!(GrammarRuleRegistration);

fn construct<R: GrammarRule + Default + 'static>() -> Box<dyn GrammarRule> {
    Box::new(R::default())
}

impl GrammarRuleRegistration {
    /// The registration of rule type `R`, which is created with `R::default()`. `const`, because the registry is
    /// filled at link time.
    pub const fn of<R: GrammarRule + Default + 'static>() -> Self {
        Self { create: construct::<R> }
    }

    /// A new instance of the rule, in its initial state.
    pub fn instantiate(&self) -> Box<dyn GrammarRule> {
        (self.create)()
    }
}

/// Registers a grammar rule next to its definition: `register_grammar_rule!(SmileBlinkArc);`. The type must be
/// [`GrammarRule`] and [`Default`]. There is no central list: a rule exists for every stage built after it is linked.
#[macro_export]
macro_rules! register_grammar_rule {
    ($rule:ty) => {
        $crate::grammar::__inventory::submit! { $crate::grammar::GrammarRuleRegistration::of::<$rule>() }
        $crate::grammar::__record_registration!("grammar_rule", $rule);
    };
}

/// Why a [`GrammarStage`](super::GrammarStage) could not be built from a set of rules.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrammarError {
    /// A rule's `id()` is not a valid slug, so a character file cannot name it.
    #[error("grammar rule id '{id}' is not a valid slug: {source}")]
    BadId {
        /// The id the rule gave.
        id: &'static str,
        /// What is wrong with it.
        source: SlugError,
    },
    /// Two rules have the same id, so the switches of one would silently also switch the other.
    #[error("two grammar rules are both called '{0}'; give one a different id()")]
    Duplicate(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(values: &mut ChannelBuf, taken: ChannelMask, strength: f32) -> RuleCtx<'_> {
        RuleCtx::new(values, taken, strength, Tick::at_rate(0, 60.0), &Cues::default())
    }

    #[test]
    fn put_moves_by_strength_and_stays_in_range() {
        let mut v = ChannelBuf::NEUTRAL;
        v[Channel::EYE_ARC_LEFT] = 0.2;
        ctx(&mut v, ChannelMask::EMPTY, 0.5).put(Channel::EYE_ARC_LEFT, 1.0);
        assert!((v[Channel::EYE_ARC_LEFT] - 0.6).abs() < 1e-6);
        ctx(&mut v, ChannelMask::EMPTY, 2.0).put(Channel::EYE_ARC_LEFT, 1.0);
        assert!((v[Channel::EYE_ARC_LEFT] - 1.0).abs() < 1e-6, "twice the strength cannot leave the range");
    }

    #[test]
    fn put_refuses_people_zero_strength_and_non_finite_targets() {
        let mut v = ChannelBuf::NEUTRAL;
        ctx(&mut v, ChannelMask::of(Channel::BLUSH), 1.0).put(Channel::BLUSH, 1.0);
        ctx(&mut v, ChannelMask::EMPTY, 0.0).put(Channel::SPARKLE, 1.0);
        ctx(&mut v, ChannelMask::EMPTY, 1.0).put(Channel::TEARS, f32::NAN);
        assert_eq!(v, ChannelBuf::NEUTRAL);
    }

    #[test]
    fn get_reads_non_finite_values_as_neutral_and_dt_is_capped() {
        let mut v = ChannelBuf::NEUTRAL;
        v[Channel::BLUSH] = f32::INFINITY;
        let mut tick = Tick::at_rate(0, 60.0);
        tick.dt = 5.0;
        let c = RuleCtx::new(&mut v, ChannelMask::EMPTY, 1.0, tick, &Cues::default());
        assert_eq!((c.get(Channel::BLUSH), c.dt()), (0.0, MAX_DT));
        tick.dt = f32::NAN;
        assert_eq!(RuleCtx::new(&mut v, ChannelMask::EMPTY, 1.0, tick, &Cues::default()).dt(), 0.0);
    }

    #[test]
    fn loudness_is_the_louder_of_speech_and_laughter_and_reads_broken_cues_as_silence() {
        let mut v = ChannelBuf::NEUTRAL;
        let tick = Tick::at_rate(0, 60.0);
        let mut loudness = |loudness, laugh| {
            RuleCtx::new(&mut v, ChannelMask::EMPTY, 1.0, tick, &Cues { loudness, onset: false, laugh, eyes_lead_turn: false, look_shift: false }).loudness()
        };
        assert_eq!(loudness(0.3, 0.7), 0.7);
        assert_eq!(loudness(0.6, 0.2), 0.6);
        assert_eq!(loudness(f32::NAN, f32::NAN), 0.0);
        assert_eq!(loudness(5.0, -1.0), 1.0);
    }
}
