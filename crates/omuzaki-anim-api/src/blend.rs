//! Blending: how the sources that can drive a face are combined into one **bus**, and who ends up owning each channel.
//!
//! The order is fixed and visible in the type of [`LayerStack`], from lowest to highest:
//!
//! 1. **rest**: the character's resting expression (the base every other layer moves away from)
//! 2. **score**: an authored or AI-generated timeline ([`Source::Score`])
//! 3. **tracker**: the retargeted human performance ([`Source::Tracker`])
//! 4. **audio**: lip sync, normally in [`BlendMode::Max`] so it only fills in where the camera reads the mouth poorly
//!    (spec FE-08), or [`BlendMode::Override`] when no tracker is present
//! 5. **trigger**: hotkeys and timed expressions ([`Source::Trigger`]); a held "smug" face wins over everything below
//!
//! The life layer then runs on the result (it only touches channels no human owns), and last come the anime grammar
//! rules. Hand-off between sources is done by **layer weights**: a source that is going away fades its own weight
//! (see [`Fade`]), so [`LayerStack::resolve`] stays a stateless pure function and can be tested and scrubbed freely.

use crate::channel::{Channel, ChannelBuf, ChannelMask};

/// Who can own a channel (spec PF-23).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Source {
    /// Nobody: the channel shows the character's resting expression.
    Rest = 0,
    /// A score: an authored timeline, or AI intents compiled into cues.
    Score = 1,
    /// The human performer, through a tracker.
    Tracker = 2,
    /// Audio-driven lip sync.
    Audio = 3,
    /// A hotkey, redeem or other explicit trigger.
    Trigger = 4,
    /// The life layer, on channels nothing else claims.
    Life = 5,
}

impl Source {
    /// How many sources there are.
    pub const COUNT: usize = 6;

    /// Every source, in index order.
    pub const ALL: [Source; Self::COUNT] =
        [Source::Rest, Source::Score, Source::Tracker, Source::Audio, Source::Trigger, Source::Life];

    /// True for sources a person is actively driving. The life layer never overrides these ("never fight a human").
    pub const fn is_human(self) -> bool {
        matches!(self, Source::Tracker | Source::Trigger)
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// How a layer combines with what is below it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// Move toward the layer's value by its weight. The only mode that can *own* a channel.
    Override,
    /// Add `weight * value` (a shake, a nudge). Never owns.
    Add,
    /// Move toward the larger of the two. Never owns. Lip sync over a tracker: the mouth opens if either says so.
    Max,
}

/// A layer's weight must reach this before it owns the channels it overrides, so a source that is fading out hands
/// ownership over (and the life layer takes up blinking) at the half-way point rather than at the very end.
pub const OWN_THRESHOLD: f32 = 0.5;

/// One source's contribution: values for the channels in `mask`, scaled by `weight`.
///
/// 544 bytes, `Copy`. Each producer fills only the channels it drives, so a layer's mask is also the answer to "what
/// does this source have an opinion on right now?".
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Layer {
    /// The values; only channels in `mask` are meaningful.
    pub values: ChannelBuf,
    /// The channels this layer drives.
    pub mask: ChannelMask,
    /// Strength, `0..=1`. `0` (or NaN) means the layer is absent.
    pub weight: f32,
    /// How it combines with the layers below.
    pub mode: BlendMode,
}

impl Layer {
    /// An absent layer that will combine with `mode` once it is given a weight and mask.
    pub const fn absent(mode: BlendMode) -> Self {
        Self { values: ChannelBuf::NEUTRAL, mask: ChannelMask::EMPTY, weight: 0.0, mode }
    }

    /// Withdraws the layer (producers call this when their source goes away).
    pub fn clear(&mut self) {
        self.mask = ChannelMask::EMPTY;
        self.weight = 0.0;
    }

    /// True if the layer changes anything.
    pub fn is_active(&self) -> bool {
        self.weight > 0.0 && !self.mask.is_empty()
    }

    /// True if the layer takes ownership of its channels.
    pub fn claims(&self) -> bool {
        self.mode == BlendMode::Override && self.weight >= OWN_THRESHOLD && !self.mask.is_empty()
    }

    /// Mixes this layer into `base`.
    fn mix_into(&self, base: &mut ChannelBuf) {
        // NaN counts as absent (`f32::min` would otherwise turn a NaN weight into 1).
        if self.weight.is_nan() || self.weight <= 0.0 {
            return;
        }
        let w = self.weight.min(1.0);
        match self.mode {
            BlendMode::Override => base.lerp_masked(&self.values, self.mask, w),
            BlendMode::Add => {
                for c in self.mask {
                    base[c] += w * self.values[c];
                }
            }
            BlendMode::Max => {
                for c in self.mask {
                    let below = base[c];
                    base[c] = below + (below.max(self.values[c]) - below) * w;
                }
            }
        }
    }
}

/// Which source owns each channel. The sources' masks always partition the table, so there is exactly one owner per
/// channel and the questions the pipeline asks ("do humans drive the eyes?") are single mask operations.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Ownership {
    by_source: [ChannelMask; Source::COUNT],
}

impl Default for Ownership {
    fn default() -> Self {
        Self::all_rest()
    }
}

impl Ownership {
    /// Nothing claimed: every channel is [`Source::Rest`].
    pub const fn all_rest() -> Self {
        let mut by_source = [ChannelMask::EMPTY; Source::COUNT];
        by_source[Source::Rest.index()] = ChannelMask::ALL;
        Self { by_source }
    }

    /// The owner of `channel`.
    pub fn of(&self, channel: Channel) -> Source {
        Source::ALL.into_iter().find(|s| self.by_source[s.index()].contains(channel)).unwrap_or(Source::Rest)
    }

    /// Every channel `source` owns.
    pub fn mask_of(&self, source: Source) -> ChannelMask {
        self.by_source[source.index()]
    }

    /// Every channel a person is driving ([`Source::is_human`]).
    pub fn human_mask(&self) -> ChannelMask {
        self.mask_of(Source::Tracker) | self.mask_of(Source::Trigger)
    }

    /// True if a person drives any channel in `region` (e.g. [`ChannelMask::EYES`]).
    pub fn human_owns_any(&self, region: ChannelMask) -> bool {
        self.human_mask().intersects(region)
    }

    /// Gives `source` every channel in `mask`, taking them from whoever had them.
    pub fn claim(&mut self, source: Source, mask: ChannelMask) {
        for other in &mut self.by_source {
            *other = other.without(mask);
        }
        self.by_source[source.index()] |= mask;
    }
}

/// What the rest of the pipeline works on after blending: the channel values and who owns them.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Bus {
    /// The blended channel values.
    pub values: ChannelBuf,
    /// Who owns each channel.
    pub owners: Ownership,
}

/// Every layer that feeds the bus, lowest first. Fields, not an array, so the order is part of the type.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LayerStack {
    /// The resting expression everything else moves away from.
    pub rest: ChannelBuf,
    /// Authored or AI timeline.
    pub score: Layer,
    /// Retargeted human performance.
    pub tracker: Layer,
    /// Lip sync.
    pub audio: Layer,
    /// Hotkeys and timed expressions.
    pub trigger: Layer,
}

impl LayerStack {
    /// An empty stack over `rest`, with the default modes (audio blends with [`BlendMode::Max`]).
    pub const fn new(rest: ChannelBuf) -> Self {
        Self {
            rest,
            score: Layer::absent(BlendMode::Override),
            tracker: Layer::absent(BlendMode::Override),
            audio: Layer::absent(BlendMode::Max),
            trigger: Layer::absent(BlendMode::Override),
        }
    }

    /// Blends the stack into `out`. Pure and allocation-free: the same stack always gives the same bus.
    pub fn resolve(&self, out: &mut Bus) {
        out.values = self.rest;
        out.owners = Ownership::all_rest();
        let layers = [
            (Source::Score, &self.score),
            (Source::Tracker, &self.tracker),
            (Source::Audio, &self.audio),
            (Source::Trigger, &self.trigger),
        ];
        for (source, layer) in layers {
            layer.mix_into(&mut out.values);
            if layer.claims() {
                out.owners.claim(source, layer.mask);
            }
        }
    }
}

/// A linear weight ramp for hand-offs: call [`Fade::toward`] every frame with `1.0` while a source is present and
/// `0.0` once it is gone, and use the result as the layer's weight.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Fade(pub f32);

impl Fade {
    /// Moves toward `target` (`0..=1`) at a rate that covers the whole `0..1` range in `seconds`; `seconds <= 0`
    /// jumps. Returns the new weight.
    pub fn toward(&mut self, target: f32, dt: f32, seconds: f32) -> f32 {
        let target = target.clamp(0.0, 1.0);
        self.0 = if seconds > 0.0 {
            let step = dt.max(0.0) / seconds;
            if target > self.0 { (self.0 + step).min(target) } else { (self.0 - step).max(target) }
        } else {
            target
        };
        self.0
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // tests compare values that are copied or exactly computed, and cast small loop counters
mod tests {
    use omuzaki_core::rng::Rng;

    use super::*;

    fn layer(mode: BlendMode, weight: f32, entries: &[(Channel, f32)]) -> Layer {
        let mut l = Layer::absent(mode);
        l.weight = weight;
        for &(c, v) in entries {
            l.values[c] = v;
            l.mask.insert(c);
        }
        l
    }

    fn resolved(stack: &LayerStack) -> Bus {
        let mut bus = Bus::default();
        stack.resolve(&mut bus);
        bus
    }

    #[test]
    fn empty_stack_shows_rest_and_nobody_owns_anything() {
        let mut rest = ChannelBuf::NEUTRAL;
        rest[Channel::BLUSH] = 0.3;
        let bus = resolved(&LayerStack::new(rest));
        assert_eq!(bus.values, rest);
        assert_eq!(bus.owners, Ownership::all_rest());
    }

    #[test]
    fn higher_layers_win_and_own() {
        let mut stack = LayerStack::new(ChannelBuf::NEUTRAL);
        stack.score = layer(BlendMode::Override, 1.0, &[(Channel::JAW_OPEN, 0.2), (Channel::BLUSH, 0.5)]);
        stack.tracker = layer(BlendMode::Override, 1.0, &[(Channel::JAW_OPEN, 0.7)]);
        let bus = resolved(&stack);
        assert_eq!(bus.values[Channel::JAW_OPEN], 0.7);
        assert_eq!(bus.values[Channel::BLUSH], 0.5);
        assert_eq!(bus.owners.of(Channel::JAW_OPEN), Source::Tracker);
        assert_eq!(bus.owners.of(Channel::BLUSH), Source::Score);
        assert_eq!(bus.owners.of(Channel::HEAD_YAW), Source::Rest);
        assert!(bus.owners.human_owns_any(ChannelMask::MOUTH) && !bus.owners.human_owns_any(ChannelMask::EYES));
    }

    #[test]
    fn weight_blends_and_a_fading_layer_hands_ownership_over_at_half() {
        let mut stack = LayerStack::new(ChannelBuf::NEUTRAL);
        stack.tracker = layer(BlendMode::Override, 0.75, &[(Channel::EYE_BLINK_LEFT, 1.0)]);
        let bus = resolved(&stack);
        assert_eq!(bus.values[Channel::EYE_BLINK_LEFT], 0.75);
        assert_eq!(bus.owners.of(Channel::EYE_BLINK_LEFT), Source::Tracker);
        stack.tracker.weight = 0.25;
        let bus = resolved(&stack);
        assert_eq!(bus.values[Channel::EYE_BLINK_LEFT], 0.25);
        assert!(!bus.owners.human_owns_any(ChannelMask::EYES), "below the threshold the life layer may blink again");
    }

    #[test]
    fn max_and_add_refine_without_owning() {
        let mut stack = LayerStack::new(ChannelBuf::NEUTRAL);
        stack.tracker = layer(BlendMode::Override, 1.0, &[(Channel::JAW_OPEN, 0.1)]);
        stack.audio = layer(BlendMode::Max, 1.0, &[(Channel::JAW_OPEN, 0.6)]);
        let bus = resolved(&stack);
        assert_eq!(bus.values[Channel::JAW_OPEN], 0.6, "lip sync fills in where the camera reads low");
        assert_eq!(bus.owners.of(Channel::JAW_OPEN), Source::Tracker, "and the tracker still owns the channel");

        stack.audio.values[Channel::JAW_OPEN] = 0.05;
        assert_eq!(resolved(&stack).values[Channel::JAW_OPEN], 0.1, "max never lowers");

        stack.trigger = layer(BlendMode::Add, 0.5, &[(Channel::JAW_OPEN, 0.2)]);
        let bus = resolved(&stack);
        assert!((bus.values[Channel::JAW_OPEN] - 0.2).abs() < 1e-6);
        assert_eq!(bus.owners.of(Channel::JAW_OPEN), Source::Tracker);
    }

    #[test]
    fn absent_and_nan_weights_do_nothing() {
        let mut stack = LayerStack::new(ChannelBuf::NEUTRAL);
        stack.score = layer(BlendMode::Override, f32::NAN, &[(Channel::JAW_OPEN, 1.0)]);
        stack.trigger = layer(BlendMode::Override, 0.0, &[(Channel::BLUSH, 1.0)]);
        let bus = resolved(&stack);
        assert_eq!(bus.values, ChannelBuf::NEUTRAL);
        assert_eq!(bus.owners, Ownership::all_rest());
        assert!(!stack.score.is_active() && !stack.trigger.is_active());
    }

    #[test]
    fn ownership_always_partitions_the_table() {
        // Property test with a seeded RNG: whatever the layers, every channel has exactly one owner.
        let mut rng = Rng::new(2024);
        for _ in 0..500 {
            let mut stack = LayerStack::new(ChannelBuf::NEUTRAL);
            for layer in [&mut stack.score, &mut stack.tracker, &mut stack.audio, &mut stack.trigger] {
                layer.mask =
                    ChannelMask::from_bits_truncate(u128::from(rng.next_u64()) << 64 | u128::from(rng.next_u64()));
                layer.weight = rng.next_f32();
                layer.mode = [BlendMode::Override, BlendMode::Add, BlendMode::Max][rng.next_u64() as usize % 3];
            }
            let owners = resolved(&stack).owners;
            let union = Source::ALL.iter().fold(ChannelMask::EMPTY, |m, &s| m | owners.mask_of(s));
            let total: u32 = Source::ALL.iter().map(|&s| owners.mask_of(s).count()).sum();
            assert_eq!(union, ChannelMask::ALL);
            assert_eq!(total, ChannelMask::ALL.count(), "no channel has two owners");
        }
    }

    #[test]
    fn hot_types_are_fixed_size_and_copy() {
        fn assert_copy<T: Copy + Send>() {}
        assert_copy::<(Layer, LayerStack, Bus, Ownership)>();
        assert!(std::mem::size_of::<LayerStack>() <= 4096, "the whole stack is a few KiB, on the stack");
    }

    #[test]
    fn fade_ramps_linearly_and_clamps() {
        let mut f = Fade(0.0);
        assert_eq!(f.toward(1.0, 0.25, 1.0), 0.25);
        assert_eq!(f.toward(1.0, 1.0, 1.0), 1.0);
        assert_eq!(f.toward(0.0, 0.5, 1.0), 0.5);
        assert_eq!(f.toward(0.0, 0.0, 1.0), 0.5, "no time, no movement");
        assert_eq!(f.toward(1.0, 0.1, 0.0), 1.0, "zero duration jumps");
    }
}
