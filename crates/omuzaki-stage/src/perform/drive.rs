//! [`DriveGate`]: a member's [`DriveMap`] in the form the perform thread blends with (`docs/design/drivers.md`).
//!
//! The map says, per channel group and driver kind, how strongly that driver may move the group. The gate holds the
//! same numbers as a fixed table, and [`resolve`] blends the layer stack through it: each layer mixes in region by
//! region, its weight scaled by the gate, and owns a region's channels only when the scaled weight still reaches
//! [`OWN_THRESHOLD`]. So a webcam gated off the gaze leaves the eyes to the score (an agent's `act.look`), or to the
//! life layer's saccades when nothing else drives them.
//!
//! An open gate (the default) blends exactly as [`LayerStack::resolve`] does; [`resolve`] takes that path.

use omuzaki_anim_api::{BlendMode, Bus, ChannelMask, Layer, LayerStack, OWN_THRESHOLD, Ownership, Region, Source};
use omuzaki_stage_api::drive::{DriveGroup, DriveMap, DriverKind};

/// How many channel groups there are.
const GROUPS: usize = DriveGroup::ALL.len();
/// How many driver kinds there are.
const KINDS: usize = DriverKind::ALL.len();

/// The channels of each group, in [`DriveGroup::ALL`] order.
const GROUP_MASKS: [ChannelMask; GROUPS] = [
    ChannelMask::BROWS,
    ChannelMask::EYES,
    ChannelMask::GAZE,
    ChannelMask::MOUTH,
    ChannelMask::HEAD,
    ChannelMask::BODY,
    ChannelMask::EXTRAS,
    ChannelMask::CUSTOM,
];

/// The anim-api region a group names.
pub fn region_of(group: DriveGroup) -> Region {
    match group {
        DriveGroup::Brows => Region::Brows,
        DriveGroup::Eyes => Region::Eyes,
        DriveGroup::Gaze => Region::Gaze,
        DriveGroup::Mouth => Region::Mouth,
        DriveGroup::Head => Region::Head,
        DriveGroup::Body => Region::Body,
        DriveGroup::Extras => Region::Extras,
        DriveGroup::Custom => Region::Custom,
    }
}

/// The blend source a driver kind feeds.
pub const fn source_of(kind: DriverKind) -> Source {
    match kind {
        DriverKind::Score => Source::Score,
        DriverKind::Tracker => Source::Tracker,
        DriverKind::Voice => Source::Audio,
        DriverKind::Keys => Source::Trigger,
    }
}

/// A [`DriveMap`] as a table: `weights[kind][group]`, each in `0..=1`. `Copy` and 128 bytes, so it travels to the
/// perform thread in a command and is replaced whole.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DriveGate {
    weights: [[f32; GROUPS]; KINDS],
}

impl Default for DriveGate {
    fn default() -> Self {
        Self::OPEN
    }
}

impl DriveGate {
    /// Every driver at full strength everywhere.
    pub const OPEN: Self = Self { weights: [[1.0; GROUPS]; KINDS] };

    /// The gate for `map`.
    pub fn of(map: &DriveMap) -> Self {
        let mut gate = Self::OPEN;
        for (k, kind) in DriverKind::ALL.into_iter().enumerate() {
            for (g, group) in DriveGroup::ALL.into_iter().enumerate() {
                let w = map.weight(group, kind);
                gate.weights[k][g] = if w.is_finite() { w.clamp(0.0, 1.0) } else { 1.0 };
            }
        }
        gate
    }

    /// The map this gate was made from, without the entries at full strength.
    pub fn to_map(&self) -> DriveMap {
        let mut map = DriveMap::open();
        for (k, kind) in DriverKind::ALL.into_iter().enumerate() {
            for (g, group) in DriveGroup::ALL.into_iter().enumerate() {
                map.set(group, kind, self.weights[k][g]);
            }
        }
        map
    }

    /// The weight of `kind` on `group`.
    pub fn weight(&self, kind: DriverKind, group: DriveGroup) -> f32 {
        let (k, g) = (kind as usize, group_index(group));
        self.weights[k][g]
    }

    /// Whether every weight is 1.
    pub fn is_open(&self) -> bool {
        self.weights.iter().flatten().all(|w| *w >= 1.0)
    }
}

fn group_index(group: DriveGroup) -> usize {
    DriveGroup::ALL.iter().position(|g| *g == group).unwrap_or(0)
}

/// Blends `stack` into `out` through `gate`. With an open gate this is [`LayerStack::resolve`]. Pure and allocation-free.
pub fn resolve(stack: &LayerStack, gate: &DriveGate, out: &mut Bus) {
    if gate.is_open() {
        stack.resolve(out);
        return;
    }
    out.values = stack.rest;
    out.owners = Ownership::all_rest();
    let layers = [
        (0, Source::Score, &stack.score),
        (1, Source::Tracker, &stack.tracker),
        (2, Source::Audio, &stack.audio),
        (3, Source::Trigger, &stack.trigger),
    ];
    for (k, source, layer) in layers {
        // NaN counts as absent, as in `LayerStack::resolve`.
        if layer.weight.is_nan() || layer.weight <= 0.0 || layer.mask.is_empty() {
            continue;
        }
        let base = layer.weight.min(1.0);
        for (g, group_mask) in GROUP_MASKS.iter().enumerate() {
            let mask = ChannelMask::from_bits_truncate(layer.mask.bits() & group_mask.bits());
            let w = base * gate.weights[k][g];
            if mask.is_empty() || w <= 0.0 {
                continue;
            }
            mix(layer, mask, w, out);
            if layer.mode == BlendMode::Override && w >= OWN_THRESHOLD {
                out.owners.claim(source, mask);
            }
        }
    }
}

/// Mixes `layer`'s channels in `mask` into `out` at weight `w` (`0 < w <= 1`), as the layer's mode says.
fn mix(layer: &Layer, mask: ChannelMask, w: f32, out: &mut Bus) {
    match layer.mode {
        BlendMode::Override => out.values.lerp_masked(&layer.values, mask, w),
        BlendMode::Add => {
            for c in mask {
                out.values[c] += w * layer.values[c];
            }
        }
        BlendMode::Max => {
            for c in mask {
                let below = out.values[c];
                out.values[c] = below + (below.max(layer.values[c]) - below) * w;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // values copied through, or the same arithmetic, compared exactly
mod tests {
    use omuzaki_anim_api::{Channel, ChannelBuf};

    use super::*;

    fn layer(mode: BlendMode, weight: f32, mask: ChannelMask, value: f32) -> Layer {
        let mut values = ChannelBuf::NEUTRAL;
        for c in mask {
            values[c] = value;
        }
        Layer { values, mask, weight, mode }
    }

    fn stack() -> LayerStack {
        let mut stack = LayerStack::new(ChannelBuf::NEUTRAL);
        stack.score = layer(BlendMode::Override, 1.0, ChannelMask::GAZE | ChannelMask::HEAD, 0.3);
        stack.tracker = layer(BlendMode::Override, 1.0, ChannelMask::ARKIT | ChannelMask::HEAD, 0.6);
        stack.audio = layer(BlendMode::Max, 0.8, ChannelMask::MOUTH, 0.9);
        stack.trigger = layer(BlendMode::Override, 0.4, ChannelMask::BROWS, 0.2);
        stack
    }

    #[test]
    fn a_gate_that_lets_everything_through_blends_exactly_as_the_stack_does() {
        let stack = stack();
        let (mut want, mut got) = (Bus::default(), Bus::default());
        stack.resolve(&mut want);
        resolve(&stack, &DriveGate::OPEN, &mut got);
        assert_eq!(got, want);
        // The slow path with every weight at 1 must agree too: it is what a nearly open gate runs.
        let mut nearly = DriveGate::OPEN;
        nearly.weights[3][7] = 0.999_999;
        let mut slow = Bus::default();
        resolve(&stack, &nearly, &mut slow);
        for c in ChannelMask::ALL.without(ChannelMask::CUSTOM) {
            assert!((slow.values[c] - want.values[c]).abs() < 1e-6, "{c}");
            assert_eq!(slow.owners.of(c), want.owners.of(c), "{c}");
        }
    }

    #[test]
    fn a_webcam_gated_off_the_gaze_leaves_the_eyes_to_the_score() {
        let stack = stack();
        let mut map = DriveMap::open();
        map.set(DriveGroup::Gaze, DriverKind::Tracker, 0.0);
        let (mut bus, mut open) = (Bus::default(), Bus::default());
        resolve(&stack, &DriveGate::of(&map), &mut bus);
        stack.resolve(&mut open);
        let gaze = Channel::EYE_LOOK_IN_LEFT;
        assert_eq!(bus.values[gaze], 0.3, "the score's look shows");
        assert_eq!(bus.owners.of(gaze), Source::Score);
        assert_eq!(bus.values[Channel::JAW_OPEN], open.values[Channel::JAW_OPEN], "the mouth is as before");
        assert_eq!(bus.owners.of(Channel::HEAD_YAW), Source::Tracker, "the head is still the webcam's");
    }

    #[test]
    fn half_a_driver_moves_half_as_far_and_no_longer_owns_what_it_moves() {
        let stack = stack();
        let mut map = DriveMap::open();
        map.set(DriveGroup::Head, DriverKind::Tracker, 0.4);
        let mut bus = Bus::default();
        resolve(&stack, &DriveGate::of(&map), &mut bus);
        let yaw = Channel::HEAD_YAW;
        assert!((bus.values[yaw] - (0.3 + (0.6 - 0.3) * 0.4)).abs() < 1e-6);
        assert_eq!(bus.owners.of(yaw), Source::Score, "below the threshold the score keeps the head");
        assert_eq!(DriveGate::of(&map).to_map(), map);
    }
}
