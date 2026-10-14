//! The channel table: the one list of things an animation source can drive, and the fixed-size buffer that holds them.
//!
//! Every producer (tracker retarget, audio lip sync, scores, hotkeys, the life layer) writes **channels**; every
//! consumer (face painter, pose stage, recorder) reads them. Because all of them address a channel by its index in
//! this table, the table is a stable contract: **an index, once assigned, never changes meaning**, which is what keeps
//! recorded tracks, scores and golden renders valid across versions (spec §8 determinism).
//!
//! Layout (index ranges):
//!
//! | Range | What |
//! |---|---|
//! | `0..52` | the 52 `ARKit` blendshapes, in Apple's alphabetical order, so a tracker frame's `blend` array indexes straight in |
//! | `52..60` | head and body: head angles and shift, lean, body turn, breath |
//! | `60..73` | anime extras (spec FE-02): ^^ arcs, heart/star/gloom/shock/squeeze/spiral eyes, sparkle, tears, sweat, blush, fangs |
//! | `73..112` | reserved for future standard channels (finger curls, more extras) |
//! | `112..128` | 16 `customNN` slots for character-specific toggles that scores and triggers can drive |
//!
//! Naming: camelCase, `Left`/`Right` suffix for the character's own sides (`ARKit` convention). Names are what files
//! and tools use; indices are only for the hot path.
//!
//! Units: weights are `0..1`, angles are **radians**, lengths are metres. UIs and text formats may show degrees.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::ops::{BitAnd, BitOr, BitOrAssign, Index, IndexMut};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How many channels the buffers hold. A power of two so a [`ChannelMask`] is exactly one `u128`.
pub const CHANNEL_CAPACITY: usize = 128;
/// How many `ARKit` blendshapes there are (the first channels in the table).
pub const ARKIT_COUNT: usize = 52;
/// How many `customNN` channels there are.
pub const CUSTOM_COUNT: usize = 16;

/// What a channel's number means.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// A dimensionless amount, `0..1` (blendshape weights, toggles, extras).
    Weight,
    /// An angle in radians.
    Radians,
    /// A length in metres.
    Metres,
}

/// A coarse group of channels. Ownership questions are asked per region ("does a human drive the eyes?"), because
/// the life layer's behaviours (a blink, a saccade, a head nod) each need a whole region to be free.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    /// Brows, and the nose sneer that pulls them down.
    Brows,
    /// Lids: blink, squint, wide, cheek squint, ^^ arcs and the eye-style extras (heart, star, ...).
    Eyes,
    /// Where the eyes point: the eight `eyeLook*` channels.
    Gaze,
    /// Jaw, lips, cheek puff and tongue.
    Mouth,
    /// Head angles and shift.
    Head,
    /// Lean, body turn and breath.
    Body,
    /// Anime extras that are not eye styles: sparkle, tears, sweat, blush, fangs.
    Extras,
    /// The `customNN` slots.
    Custom,
}

/// What mirroring (left/right swap, for mirrored previews, spec PF-27) does to a channel.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mirror {
    /// Nothing: the channel is on the centre line.
    None,
    /// Trade values with the opposite-side channel.
    Swap(Channel),
    /// Flip the sign: the channel is a signed offset across the centre line (yaw, roll, sideways shift).
    Negate,
}

/// Static facts about one channel.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ChannelInfo {
    /// The channel.
    pub channel: Channel,
    /// Its stable name.
    pub name: &'static str,
    /// What its number means.
    pub unit: Unit,
    /// Smallest legal value (values are clamped here at the end of the pipeline).
    pub min: f32,
    /// Largest legal value.
    pub max: f32,
    /// Its region.
    pub region: Region,
    /// What mirroring does to it.
    pub mirror: Mirror,
}

/// One slot of the channel table. Cheap to copy and compare; build one from the constants (`Channel::JAW_OPEN`),
/// [`Channel::from_name`] or [`Channel::custom`], so every value is an assigned channel.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Channel(u8);

macro_rules! mirror_of {
    (none) => {
        Mirror::None
    };
    (negate) => {
        Mirror::Negate
    };
    (swap $other:ident) => {
        Mirror::Swap(Channel::$other)
    };
}

macro_rules! channels {
    ($($konst:ident = $idx:literal, $name:literal, $unit:ident, $min:literal, $max:literal, $region:ident, [$($mirror:tt)+];)*) => {
        impl Channel {
            $(
                #[doc = concat!("The `", $name, "` channel.")]
                pub const $konst: Channel = Channel($idx);
            )*
        }

        /// Every assigned channel, in index order. The first [`ARKIT_COUNT`] entries are the `ARKit` blendshapes.
        pub const CHANNELS: &[ChannelInfo] = &[
            $(ChannelInfo {
                channel: Channel::$konst,
                name: $name,
                unit: Unit::$unit,
                min: $min,
                max: $max,
                region: Region::$region,
                mirror: mirror_of!($($mirror)+),
            }),*
        ];
    };
}

channels! {
    BROW_DOWN_LEFT = 0, "browDownLeft", Weight, 0.0, 1.0, Brows, [swap BROW_DOWN_RIGHT];
    BROW_DOWN_RIGHT = 1, "browDownRight", Weight, 0.0, 1.0, Brows, [swap BROW_DOWN_LEFT];
    BROW_INNER_UP = 2, "browInnerUp", Weight, 0.0, 1.0, Brows, [none];
    BROW_OUTER_UP_LEFT = 3, "browOuterUpLeft", Weight, 0.0, 1.0, Brows, [swap BROW_OUTER_UP_RIGHT];
    BROW_OUTER_UP_RIGHT = 4, "browOuterUpRight", Weight, 0.0, 1.0, Brows, [swap BROW_OUTER_UP_LEFT];
    CHEEK_PUFF = 5, "cheekPuff", Weight, 0.0, 1.0, Mouth, [none];
    CHEEK_SQUINT_LEFT = 6, "cheekSquintLeft", Weight, 0.0, 1.0, Eyes, [swap CHEEK_SQUINT_RIGHT];
    CHEEK_SQUINT_RIGHT = 7, "cheekSquintRight", Weight, 0.0, 1.0, Eyes, [swap CHEEK_SQUINT_LEFT];
    EYE_BLINK_LEFT = 8, "eyeBlinkLeft", Weight, 0.0, 1.0, Eyes, [swap EYE_BLINK_RIGHT];
    EYE_BLINK_RIGHT = 9, "eyeBlinkRight", Weight, 0.0, 1.0, Eyes, [swap EYE_BLINK_LEFT];
    EYE_LOOK_DOWN_LEFT = 10, "eyeLookDownLeft", Weight, 0.0, 1.0, Gaze, [swap EYE_LOOK_DOWN_RIGHT];
    EYE_LOOK_DOWN_RIGHT = 11, "eyeLookDownRight", Weight, 0.0, 1.0, Gaze, [swap EYE_LOOK_DOWN_LEFT];
    EYE_LOOK_IN_LEFT = 12, "eyeLookInLeft", Weight, 0.0, 1.0, Gaze, [swap EYE_LOOK_IN_RIGHT];
    EYE_LOOK_IN_RIGHT = 13, "eyeLookInRight", Weight, 0.0, 1.0, Gaze, [swap EYE_LOOK_IN_LEFT];
    EYE_LOOK_OUT_LEFT = 14, "eyeLookOutLeft", Weight, 0.0, 1.0, Gaze, [swap EYE_LOOK_OUT_RIGHT];
    EYE_LOOK_OUT_RIGHT = 15, "eyeLookOutRight", Weight, 0.0, 1.0, Gaze, [swap EYE_LOOK_OUT_LEFT];
    EYE_LOOK_UP_LEFT = 16, "eyeLookUpLeft", Weight, 0.0, 1.0, Gaze, [swap EYE_LOOK_UP_RIGHT];
    EYE_LOOK_UP_RIGHT = 17, "eyeLookUpRight", Weight, 0.0, 1.0, Gaze, [swap EYE_LOOK_UP_LEFT];
    EYE_SQUINT_LEFT = 18, "eyeSquintLeft", Weight, 0.0, 1.0, Eyes, [swap EYE_SQUINT_RIGHT];
    EYE_SQUINT_RIGHT = 19, "eyeSquintRight", Weight, 0.0, 1.0, Eyes, [swap EYE_SQUINT_LEFT];
    EYE_WIDE_LEFT = 20, "eyeWideLeft", Weight, 0.0, 1.0, Eyes, [swap EYE_WIDE_RIGHT];
    EYE_WIDE_RIGHT = 21, "eyeWideRight", Weight, 0.0, 1.0, Eyes, [swap EYE_WIDE_LEFT];
    JAW_FORWARD = 22, "jawForward", Weight, 0.0, 1.0, Mouth, [none];
    JAW_LEFT = 23, "jawLeft", Weight, 0.0, 1.0, Mouth, [swap JAW_RIGHT];
    JAW_OPEN = 24, "jawOpen", Weight, 0.0, 1.0, Mouth, [none];
    JAW_RIGHT = 25, "jawRight", Weight, 0.0, 1.0, Mouth, [swap JAW_LEFT];
    MOUTH_CLOSE = 26, "mouthClose", Weight, 0.0, 1.0, Mouth, [none];
    MOUTH_DIMPLE_LEFT = 27, "mouthDimpleLeft", Weight, 0.0, 1.0, Mouth, [swap MOUTH_DIMPLE_RIGHT];
    MOUTH_DIMPLE_RIGHT = 28, "mouthDimpleRight", Weight, 0.0, 1.0, Mouth, [swap MOUTH_DIMPLE_LEFT];
    MOUTH_FROWN_LEFT = 29, "mouthFrownLeft", Weight, 0.0, 1.0, Mouth, [swap MOUTH_FROWN_RIGHT];
    MOUTH_FROWN_RIGHT = 30, "mouthFrownRight", Weight, 0.0, 1.0, Mouth, [swap MOUTH_FROWN_LEFT];
    MOUTH_FUNNEL = 31, "mouthFunnel", Weight, 0.0, 1.0, Mouth, [none];
    MOUTH_LEFT = 32, "mouthLeft", Weight, 0.0, 1.0, Mouth, [swap MOUTH_RIGHT];
    MOUTH_LOWER_DOWN_LEFT = 33, "mouthLowerDownLeft", Weight, 0.0, 1.0, Mouth, [swap MOUTH_LOWER_DOWN_RIGHT];
    MOUTH_LOWER_DOWN_RIGHT = 34, "mouthLowerDownRight", Weight, 0.0, 1.0, Mouth, [swap MOUTH_LOWER_DOWN_LEFT];
    MOUTH_PRESS_LEFT = 35, "mouthPressLeft", Weight, 0.0, 1.0, Mouth, [swap MOUTH_PRESS_RIGHT];
    MOUTH_PRESS_RIGHT = 36, "mouthPressRight", Weight, 0.0, 1.0, Mouth, [swap MOUTH_PRESS_LEFT];
    MOUTH_PUCKER = 37, "mouthPucker", Weight, 0.0, 1.0, Mouth, [none];
    MOUTH_RIGHT = 38, "mouthRight", Weight, 0.0, 1.0, Mouth, [swap MOUTH_LEFT];
    MOUTH_ROLL_LOWER = 39, "mouthRollLower", Weight, 0.0, 1.0, Mouth, [none];
    MOUTH_ROLL_UPPER = 40, "mouthRollUpper", Weight, 0.0, 1.0, Mouth, [none];
    MOUTH_SHRUG_LOWER = 41, "mouthShrugLower", Weight, 0.0, 1.0, Mouth, [none];
    MOUTH_SHRUG_UPPER = 42, "mouthShrugUpper", Weight, 0.0, 1.0, Mouth, [none];
    MOUTH_SMILE_LEFT = 43, "mouthSmileLeft", Weight, 0.0, 1.0, Mouth, [swap MOUTH_SMILE_RIGHT];
    MOUTH_SMILE_RIGHT = 44, "mouthSmileRight", Weight, 0.0, 1.0, Mouth, [swap MOUTH_SMILE_LEFT];
    MOUTH_STRETCH_LEFT = 45, "mouthStretchLeft", Weight, 0.0, 1.0, Mouth, [swap MOUTH_STRETCH_RIGHT];
    MOUTH_STRETCH_RIGHT = 46, "mouthStretchRight", Weight, 0.0, 1.0, Mouth, [swap MOUTH_STRETCH_LEFT];
    MOUTH_UPPER_UP_LEFT = 47, "mouthUpperUpLeft", Weight, 0.0, 1.0, Mouth, [swap MOUTH_UPPER_UP_RIGHT];
    MOUTH_UPPER_UP_RIGHT = 48, "mouthUpperUpRight", Weight, 0.0, 1.0, Mouth, [swap MOUTH_UPPER_UP_LEFT];
    NOSE_SNEER_LEFT = 49, "noseSneerLeft", Weight, 0.0, 1.0, Brows, [swap NOSE_SNEER_RIGHT];
    NOSE_SNEER_RIGHT = 50, "noseSneerRight", Weight, 0.0, 1.0, Brows, [swap NOSE_SNEER_LEFT];
    TONGUE_OUT = 51, "tongueOut", Weight, 0.0, 1.0, Mouth, [none];
    HEAD_YAW = 52, "headYaw", Radians, -1.75, 1.75, Head, [negate];
    HEAD_PITCH = 53, "headPitch", Radians, -1.2, 1.2, Head, [none];
    HEAD_ROLL = 54, "headRoll", Radians, -1.0, 1.0, Head, [negate];
    HEAD_SHIFT_X = 55, "headShiftX", Metres, -0.5, 0.5, Head, [negate];
    HEAD_SHIFT_Y = 56, "headShiftY", Metres, -0.5, 0.5, Head, [none];
    LEAN = 57, "lean", Radians, -0.6, 0.6, Body, [none];
    BODY_TURN = 58, "bodyTurn", Radians, -12.6, 12.6, Body, [negate];
    BREATH = 59, "breath", Weight, 0.0, 1.0, Body, [none];
    EYE_ARC_LEFT = 60, "eyeArcLeft", Weight, 0.0, 1.0, Eyes, [swap EYE_ARC_RIGHT];
    EYE_ARC_RIGHT = 61, "eyeArcRight", Weight, 0.0, 1.0, Eyes, [swap EYE_ARC_LEFT];
    EYES_HEART = 62, "eyesHeart", Weight, 0.0, 1.0, Eyes, [none];
    EYES_STAR = 63, "eyesStar", Weight, 0.0, 1.0, Eyes, [none];
    EYES_GLOOM = 64, "eyesGloom", Weight, 0.0, 1.0, Eyes, [none];
    EYES_SHOCK = 65, "eyesShock", Weight, 0.0, 1.0, Eyes, [none];
    EYES_SQUEEZE = 66, "eyesSqueeze", Weight, 0.0, 1.0, Eyes, [none];
    EYES_SPIRAL = 67, "eyesSpiral", Weight, 0.0, 1.0, Eyes, [none];
    SPARKLE = 68, "sparkle", Weight, 0.0, 1.0, Extras, [none];
    TEARS = 69, "tears", Weight, 0.0, 1.0, Extras, [none];
    SWEAT_DROP = 70, "sweatDrop", Weight, 0.0, 1.0, Extras, [none];
    BLUSH = 71, "blush", Weight, 0.0, 1.0, Extras, [none];
    FANGS = 72, "fangs", Weight, 0.0, 1.0, Extras, [none];
    CUSTOM_00 = 112, "custom00", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_01 = 113, "custom01", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_02 = 114, "custom02", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_03 = 115, "custom03", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_04 = 116, "custom04", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_05 = 117, "custom05", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_06 = 118, "custom06", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_07 = 119, "custom07", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_08 = 120, "custom08", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_09 = 121, "custom09", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_10 = 122, "custom10", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_11 = 123, "custom11", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_12 = 124, "custom12", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_13 = 125, "custom13", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_14 = 126, "custom14", Weight, 0.0, 1.0, Custom, [none];
    CUSTOM_15 = 127, "custom15", Weight, 0.0, 1.0, Custom, [none];
}

/// Table position of each channel index, or [`NO_SLOT`] for reserved indices.
const SLOT: [u8; CHANNEL_CAPACITY] = {
    let mut slot = [NO_SLOT; CHANNEL_CAPACITY];
    let mut i = 0;
    while i < CHANNELS.len() {
        #[allow(clippy::cast_possible_truncation)] // the table has far fewer than 255 entries (checked by a test)
        {
            slot[CHANNELS[i].channel.0 as usize] = i as u8;
        }
        i += 1;
    }
    slot
};
const NO_SLOT: u8 = u8::MAX;

// `ChannelMask` is one `u128`, so the capacity cannot grow past 128 without changing its representation.
const _: () = assert!(CHANNEL_CAPACITY == 128);

impl Channel {
    /// The index into a [`ChannelBuf`] (and the bit in a [`ChannelMask`]).
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The channel at table index `i`, if one is assigned there.
    pub fn from_index(i: usize) -> Option<Self> {
        let slot = *SLOT.get(i)?;
        (slot != NO_SLOT).then(|| CHANNELS[usize::from(slot)].channel)
    }

    /// The channel called `name` (`"jawOpen"`), if there is one.
    pub fn from_name(name: &str) -> Option<Self> {
        CHANNELS.iter().find(|c| c.name == name).map(|c| c.channel)
    }

    /// Custom slot `n` (`0..16`), named `custom00`..`custom15`.
    pub fn custom(n: usize) -> Option<Self> {
        (n < CUSTOM_COUNT).then(|| Self::CUSTOM_00.0 as usize + n).and_then(Self::from_index)
    }

    /// True for the 52 `ARKit` blendshapes.
    pub const fn is_arkit(self) -> bool {
        (self.0 as usize) < ARKIT_COUNT
    }

    /// The channel's static facts.
    pub fn info(self) -> &'static ChannelInfo {
        // Every `Channel` is built from the table (private field), so its slot is always assigned.
        &CHANNELS[usize::from(SLOT[self.index()])]
    }

    /// The stable name, e.g. `"eyeBlinkLeft"`.
    pub fn name(self) -> &'static str {
        self.info().name
    }

    /// The channel's region.
    pub fn region(self) -> Region {
        self.info().region
    }

    /// What mirroring does to this channel.
    pub fn mirror(self) -> Mirror {
        self.info().mirror
    }

    /// The legal value range, `(min, max)`.
    pub fn range(self) -> (f32, f32) {
        let i = self.info();
        (i.min, i.max)
    }

    /// The `ARKit` blendshapes, in index order (a slice of [`CHANNELS`]).
    pub fn arkit() -> &'static [ChannelInfo] {
        &CHANNELS[..ARKIT_COUNT]
    }
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Channel({})", self.name())
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl Serialize for Channel {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Channel {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = <Cow<'de, str>>::deserialize(d)?;
        Self::from_name(&name).ok_or_else(|| D::Error::custom(format!("unknown channel {name:?}")))
    }
}

impl JsonSchema for Channel {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "Channel".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let names: Vec<&str> = CHANNELS.iter().map(|c| c.name).collect();
        json_schema!({ "type": "string", "enum": names, "description": "An animation channel name (ARKit blendshape, head/body, anime extra or custom slot)." })
    }
}

/// Which table entries [`ChannelMask::collect`] keeps (an enum, because `const fn` cannot call a closure).
#[derive(Copy, Clone)]
enum Pick {
    All,
    Arkit,
    Region(Region),
}

/// A set of channels: one `u128`, so it is `Copy` and set algebra is a single instruction.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Default)]
pub struct ChannelMask(u128);

impl ChannelMask {
    /// No channels.
    pub const EMPTY: Self = Self(0);
    /// Every assigned channel.
    pub const ALL: Self = Self::collect(Pick::All);
    /// The 52 `ARKit` blendshapes.
    pub const ARKIT: Self = Self::collect(Pick::Arkit);
    /// [`Region::Brows`].
    pub const BROWS: Self = Self::region(Region::Brows);
    /// [`Region::Eyes`].
    pub const EYES: Self = Self::region(Region::Eyes);
    /// [`Region::Gaze`].
    pub const GAZE: Self = Self::region(Region::Gaze);
    /// [`Region::Mouth`].
    pub const MOUTH: Self = Self::region(Region::Mouth);
    /// [`Region::Head`].
    pub const HEAD: Self = Self::region(Region::Head);
    /// [`Region::Body`].
    pub const BODY: Self = Self::region(Region::Body);
    /// [`Region::Extras`].
    pub const EXTRAS: Self = Self::region(Region::Extras);
    /// [`Region::Custom`].
    pub const CUSTOM: Self = Self::region(Region::Custom);

    /// Every channel of `region`.
    pub const fn region(region: Region) -> Self {
        Self::collect(Pick::Region(region))
    }

    /// A mask of the table entries that match `pick`. `const` so the masks above cost nothing at run time.
    const fn collect(pick: Pick) -> Self {
        let mut bits = 0u128;
        let mut i = 0;
        while i < CHANNELS.len() {
            let info = &CHANNELS[i];
            let keep = match pick {
                Pick::All => true,
                Pick::Arkit => info.channel.is_arkit(),
                Pick::Region(r) => info.region as u8 == r as u8,
            };
            if keep {
                bits |= 1u128 << info.channel.0;
            }
            i += 1;
        }
        Self(bits)
    }

    /// A mask of one channel.
    pub const fn of(channel: Channel) -> Self {
        Self(1u128 << channel.0)
    }

    /// A mask of the listed channels (`const`, for tables like "the channels exaggeration boosts").
    pub const fn from_channels(list: &[Channel]) -> Self {
        let mut bits = 0u128;
        let mut i = 0;
        while i < list.len() {
            bits |= 1u128 << list[i].0;
            i += 1;
        }
        Self(bits)
    }

    /// The raw bits (bit `i` is channel index `i`).
    pub const fn bits(self) -> u128 {
        self.0
    }

    /// A mask from raw bits; bits of unassigned indices are dropped.
    pub const fn from_bits_truncate(bits: u128) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// True if `channel` is in the set.
    pub const fn contains(self, channel: Channel) -> bool {
        self.0 & (1u128 << channel.0) != 0
    }

    /// True if the sets share a channel.
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Adds a channel.
    pub fn insert(&mut self, channel: Channel) {
        self.0 |= 1u128 << channel.0;
    }

    /// Removes a channel.
    pub fn remove(&mut self, channel: Channel) {
        self.0 &= !(1u128 << channel.0);
    }

    /// The channels in `self` but not in `other`.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// True if the set is empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// How many channels are in the set.
    pub const fn count(self) -> u32 {
        self.0.count_ones()
    }

    /// The set with every channel replaced by its mirror partner (`ChannelBuf::mirror` for masks): left/right pairs
    /// swap, everything else stays.
    #[must_use]
    pub fn mirrored(self) -> Self {
        self.iter().fold(Self::EMPTY, |m, c| m | Self::of(if let Mirror::Swap(other) = c.mirror() { other } else { c }))
    }

    /// The channels in the set, lowest index first. Allocation-free.
    pub fn iter(self) -> MaskIter {
        MaskIter(self.0)
    }
}

impl BitOr for ChannelMask {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for ChannelMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for ChannelMask {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl FromIterator<Channel> for ChannelMask {
    fn from_iter<I: IntoIterator<Item = Channel>>(iter: I) -> Self {
        iter.into_iter().fold(Self::EMPTY, |m, c| m | Self::of(c))
    }
}

impl IntoIterator for ChannelMask {
    type Item = Channel;
    type IntoIter = MaskIter;
    fn into_iter(self) -> MaskIter {
        self.iter()
    }
}

impl fmt::Debug for ChannelMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl Serialize for ChannelMask {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for ChannelMask {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Vec::<Channel>::deserialize(d)?.into_iter().collect())
    }
}

impl JsonSchema for ChannelMask {
    fn schema_name() -> Cow<'static, str> {
        "ChannelMask".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "array", "items": generator.subschema_for::<Channel>(), "uniqueItems": true })
    }
}

/// Iterator over a [`ChannelMask`], lowest index first.
#[derive(Clone, Debug)]
pub struct MaskIter(u128);

impl Iterator for MaskIter {
    type Item = Channel;
    fn next(&mut self) -> Option<Channel> {
        if self.0 == 0 {
            return None;
        }
        let i = self.0.trailing_zeros();
        self.0 &= self.0 - 1; // clear the lowest set bit; cannot underflow because the value is non-zero here
        #[allow(clippy::cast_possible_truncation)] // i < 128
        Some(Channel(i as u8))
    }
}

/// One value for every channel: the unit of data that flows between pipeline stages and into the face painter.
///
/// Fixed-size (512 bytes) and `Copy`, so a frame never allocates; pass it by reference. Unset channels are `0.0`
/// (every channel's neutral value, so a zeroed buffer is a relaxed face).
#[derive(Copy, Clone, PartialEq)]
pub struct ChannelBuf([f32; CHANNEL_CAPACITY]);

impl Default for ChannelBuf {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

impl ChannelBuf {
    /// Every channel at its neutral value (`0.0`).
    pub const NEUTRAL: Self = Self([0.0; CHANNEL_CAPACITY]);

    /// The value of `channel`.
    pub fn get(&self, channel: Channel) -> f32 {
        self.0[channel.index()]
    }

    /// Sets `channel`.
    pub fn set(&mut self, channel: Channel, value: f32) {
        self.0[channel.index()] = value;
    }

    /// All [`CHANNEL_CAPACITY`] values, by channel index: ready to copy into a GPU buffer.
    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    /// The first 52 values: the `ARKit` blendshapes.
    pub fn arkit(&self) -> &[f32] {
        &self.0[..ARKIT_COUNT]
    }

    /// Copies the channels in `mask` from `src`.
    pub fn copy_masked(&mut self, src: &Self, mask: ChannelMask) {
        for c in mask {
            self.0[c.index()] = src.0[c.index()];
        }
    }

    /// Moves every slot toward `target` by `weight` (`0` = unchanged, `1` = replaced).
    ///
    /// Unlike [`ChannelBuf::lerp_masked`] this walks the whole array without a mask, which the compiler vectorises:
    /// about 4x faster than a full-mask `lerp_masked`. Slots outside the channel table are zero in both buffers and stay
    /// zero. Use it when the mask is full, or when the caller restores the unmasked channels afterwards.
    pub fn lerp_all(&mut self, target: &Self, weight: f32) {
        for (v, t) in self.0.iter_mut().zip(&target.0) {
            *v += (*t - *v) * weight;
        }
    }

    /// Moves the channels in `mask` toward `target` by `weight` (`0` = unchanged, `1` = replaced).
    pub fn lerp_masked(&mut self, target: &Self, mask: ChannelMask, weight: f32) {
        for c in mask {
            let v = &mut self.0[c.index()];
            *v += (target.0[c.index()] - *v) * weight;
        }
    }

    /// Where the eyes point, in **look units**: `x` is positive toward the character's left, `y` positive up, and `1.0`
    /// is a full-weight `eyeLook*` channel. The mean of the two eyes, so a convergence offset cancels.
    pub fn gaze(&self) -> (f32, f32) {
        let g = |c: Channel| self.0[c.index()];
        let x = f32::midpoint(g(Channel::EYE_LOOK_OUT_LEFT), g(Channel::EYE_LOOK_IN_RIGHT))
            - f32::midpoint(g(Channel::EYE_LOOK_IN_LEFT), g(Channel::EYE_LOOK_OUT_RIGHT));
        let y = f32::midpoint(g(Channel::EYE_LOOK_UP_LEFT), g(Channel::EYE_LOOK_UP_RIGHT))
            - f32::midpoint(g(Channel::EYE_LOOK_DOWN_LEFT), g(Channel::EYE_LOOK_DOWN_RIGHT));
        (x, y)
    }

    /// Points both eyes the same way (parallel gaze), in look units; the inverse of [`ChannelBuf::gaze`]. Writes all
    /// eight `eyeLook*` channels.
    pub fn set_gaze(&mut self, x: f32, y: f32) {
        let (left, right, up, down) = (x.max(0.0), (-x).max(0.0), y.max(0.0), (-y).max(0.0));
        // Looking toward the character's left turns the left eye out and the right eye in.
        for (channel, value) in [
            (Channel::EYE_LOOK_OUT_LEFT, left),
            (Channel::EYE_LOOK_IN_RIGHT, left),
            (Channel::EYE_LOOK_IN_LEFT, right),
            (Channel::EYE_LOOK_OUT_RIGHT, right),
            (Channel::EYE_LOOK_UP_LEFT, up),
            (Channel::EYE_LOOK_UP_RIGHT, up),
            (Channel::EYE_LOOK_DOWN_LEFT, down),
            (Channel::EYE_LOOK_DOWN_RIGHT, down),
        ] {
            self.0[channel.index()] = value;
        }
    }

    /// Mirrors left and right (spec PF-27): swaps each sided pair and flips the sign of centre-line offsets.
    pub fn mirror(&mut self) {
        for info in CHANNELS {
            match info.mirror {
                Mirror::Negate => self.0[info.channel.index()] = -self.0[info.channel.index()],
                // Each pair appears twice in the table; swap it once, from the lower index.
                Mirror::Swap(other) if info.channel < other => self.0.swap(info.channel.index(), other.index()),
                Mirror::None | Mirror::Swap(_) => {}
            }
        }
    }

    /// Makes every value legal: NaN and infinities become neutral, everything is clamped to its channel's range,
    /// and reserved slots are zeroed. The last step before a buffer leaves the pipeline.
    pub fn sanitize(&mut self) {
        let mut out = [0.0; CHANNEL_CAPACITY];
        for info in CHANNELS {
            let v = self.0[info.channel.index()];
            out[info.channel.index()] = if v.is_finite() { v.clamp(info.min, info.max) } else { 0.0 };
        }
        self.0 = out;
    }

    /// A stable 64-bit digest (FNV-1a over the IEEE bits of every value), for determinism checks and goldens.
    /// Equal digests across runs and machines mean bit-identical buffers.
    pub fn checksum(&self) -> u64 {
        self.0.iter().fold(0xcbf2_9ce4_8422_2325, |h, v| fnv_mix(h, v.to_bits()))
    }
}

/// One FNV-1a round over a 32-bit word. Public so other buffers (poses, bus) digest the same way.
pub fn fnv_mix(hash: u64, word: u32) -> u64 {
    word.to_le_bytes().iter().fold(hash, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
}

impl Index<Channel> for ChannelBuf {
    type Output = f32;
    fn index(&self, channel: Channel) -> &f32 {
        &self.0[channel.index()]
    }
}

impl IndexMut<Channel> for ChannelBuf {
    fn index_mut(&mut self, channel: Channel) -> &mut f32 {
        &mut self.0[channel.index()]
    }
}

impl fmt::Debug for ChannelBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(
                CHANNELS
                    .iter()
                    .filter(|c| self.0[c.channel.index()] != 0.0)
                    .map(|c| (c.name, self.0[c.channel.index()])),
            )
            .finish()
    }
}

/// At rest a buffer is written as a map of just its non-neutral channels, by name: short, diffable, and readable by
/// agents (`{"jawOpen": 0.4, "eyeBlinkLeft": 1.0}`).
impl Serialize for ChannelBuf {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let set = CHANNELS.iter().filter(|c| self.0[c.channel.index()] != 0.0);
        let mut map = s.serialize_map(None)?;
        for c in set {
            map.serialize_entry(c.name, &self.0[c.channel.index()])?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ChannelBuf {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut buf = Self::NEUTRAL;
        for (channel, value) in BTreeMap::<Channel, f32>::deserialize(d)? {
            buf.set(channel, value);
        }
        Ok(buf)
    }
}

impl JsonSchema for ChannelBuf {
    fn schema_name() -> Cow<'static, str> {
        "ChannelBuf".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "description": "Channel values by name; channels left out are neutral (0).",
            "additionalProperties": generator.subschema_for::<f32>(),
            "propertyNames": generator.subschema_for::<Channel>(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // tests compare values that are copied or exactly computed, and cast small loop counters
mod tests {
    use super::*;

    #[test]
    fn lerp_all_matches_a_full_mask_lerp_masked() {
        let (mut a, mut b) = (ChannelBuf::NEUTRAL, ChannelBuf::NEUTRAL);
        for (i, info) in CHANNELS.iter().enumerate() {
            a.set(info.channel, (i % 5) as f32 / 5.0);
            b.set(info.channel, (i % 3) as f32 / 3.0);
        }
        let mut masked = a;
        masked.lerp_masked(&b, ChannelMask::ALL, 0.3);
        let mut all = a;
        all.lerp_all(&b, 0.3);
        assert_eq!(all, masked);
    }

    #[test]
    fn indices_names_and_slots_are_consistent() {
        assert!(CHANNELS.len() < usize::from(NO_SLOT));
        assert_eq!(CHANNELS.len(), 52 + 8 + 13 + CUSTOM_COUNT);
        let mut seen = std::collections::HashSet::new();
        for (pos, info) in CHANNELS.iter().enumerate() {
            assert!(info.channel.index() < CHANNEL_CAPACITY);
            assert!(seen.insert(info.name), "duplicate name {}", info.name);
            assert_eq!(Channel::from_name(info.name), Some(info.channel));
            assert_eq!(Channel::from_index(info.channel.index()), Some(info.channel));
            assert_eq!(usize::from(SLOT[info.channel.index()]), pos);
            assert!(info.min < info.max && info.min <= 0.0 && info.max >= 0.0, "neutral must be legal: {}", info.name);
        }
        assert!(CHANNELS.windows(2).all(|w| w[0].channel < w[1].channel), "table is in index order");
        assert_eq!(Channel::from_index(100), None, "reserved");
        assert_eq!(Channel::from_index(128), None);
    }

    #[test]
    fn arkit_block_is_apples_52_in_order() {
        let arkit = Channel::arkit();
        assert_eq!(arkit.len(), ARKIT_COUNT);
        assert_eq!(arkit[0].name, "browDownLeft");
        assert_eq!(arkit[8].name, "eyeBlinkLeft");
        assert_eq!(arkit[24].name, "jawOpen");
        assert_eq!(arkit[51].name, "tongueOut");
        assert!(arkit.iter().enumerate().all(|(i, c)| c.channel.index() == i && c.unit == Unit::Weight));
        assert_eq!(ChannelMask::ARKIT.count() as usize, ARKIT_COUNT);
    }

    #[test]
    fn mirror_pairs_are_symmetric_and_same_region() {
        for info in CHANNELS {
            if let Mirror::Swap(other) = info.mirror {
                assert_eq!(other.mirror(), Mirror::Swap(info.channel), "{} pairs back", info.name);
                assert_eq!(other.region(), info.region);
                assert_ne!(info.name.ends_with("Left"), other.name().ends_with("Left"));
            }
        }
    }

    #[test]
    fn regions_partition_the_table() {
        let regions = [
            ChannelMask::BROWS,
            ChannelMask::EYES,
            ChannelMask::GAZE,
            ChannelMask::MOUTH,
            ChannelMask::HEAD,
            ChannelMask::BODY,
            ChannelMask::EXTRAS,
            ChannelMask::CUSTOM,
        ];
        let union = regions.iter().fold(ChannelMask::EMPTY, |a, &b| a | b);
        assert_eq!(union, ChannelMask::ALL);
        assert_eq!(regions.iter().map(|m| m.count()).sum::<u32>(), ChannelMask::ALL.count());
        assert!(ChannelMask::GAZE.contains(Channel::EYE_LOOK_UP_LEFT));
        assert!(ChannelMask::EYES.contains(Channel::EYE_BLINK_LEFT));
        assert!(ChannelMask::MOUTH.contains(Channel::TONGUE_OUT));
    }

    #[test]
    fn mask_mirror_swaps_partners_only() {
        let m = ChannelMask::of(Channel::EYE_BLINK_LEFT)
            | ChannelMask::of(Channel::HEAD_YAW)
            | ChannelMask::of(Channel::JAW_OPEN);
        let expect = ChannelMask::of(Channel::EYE_BLINK_RIGHT)
            | ChannelMask::of(Channel::HEAD_YAW)
            | ChannelMask::of(Channel::JAW_OPEN);
        assert_eq!(m.mirrored(), expect);
        assert_eq!(m.mirrored().mirrored(), m);
    }

    #[test]
    fn custom_slots() {
        assert_eq!(Channel::custom(0).map(Channel::name), Some("custom00"));
        assert_eq!(Channel::custom(15).map(Channel::name), Some("custom15"));
        assert_eq!(Channel::custom(16), None);
    }

    #[test]
    fn mask_algebra_and_iteration() {
        let mut m = ChannelMask::of(Channel::JAW_OPEN) | ChannelMask::of(Channel::HEAD_YAW);
        assert_eq!(m.iter().collect::<Vec<_>>(), [Channel::JAW_OPEN, Channel::HEAD_YAW]);
        m.insert(Channel::BLUSH);
        m.remove(Channel::JAW_OPEN);
        assert!(m.contains(Channel::BLUSH) && !m.contains(Channel::JAW_OPEN));
        assert_eq!(m.without(ChannelMask::HEAD).count(), 1);
        assert_eq!(ChannelMask::from_bits_truncate(u128::MAX), ChannelMask::ALL);
        assert!(ChannelMask::EMPTY.is_empty());
    }

    #[test]
    fn buffer_get_set_index_and_masked_ops() {
        let mut a = ChannelBuf::NEUTRAL;
        a[Channel::JAW_OPEN] = 0.5;
        a.set(Channel::BLUSH, 1.0);
        assert_eq!(a.get(Channel::JAW_OPEN), 0.5);
        let mut b = ChannelBuf::NEUTRAL;
        b.copy_masked(&a, ChannelMask::MOUTH);
        assert_eq!((b[Channel::JAW_OPEN], b[Channel::BLUSH]), (0.5, 0.0));
        b.lerp_masked(&ChannelBuf::NEUTRAL, ChannelMask::MOUTH, 0.5);
        assert_eq!(b[Channel::JAW_OPEN], 0.25);
        assert_eq!(a.arkit().len(), ARKIT_COUNT);
        assert_eq!(a.as_slice().len(), CHANNEL_CAPACITY);
    }

    #[test]
    fn mirror_swaps_pairs_and_negates_offsets_and_is_an_involution() {
        let mut b = ChannelBuf::NEUTRAL;
        b[Channel::EYE_BLINK_LEFT] = 0.9;
        b[Channel::MOUTH_SMILE_RIGHT] = 0.4;
        b[Channel::JAW_OPEN] = 0.3;
        b[Channel::HEAD_YAW] = 0.2;
        b[Channel::HEAD_PITCH] = 0.1;
        let original = b;
        b.mirror();
        assert_eq!(b[Channel::EYE_BLINK_RIGHT], 0.9);
        assert_eq!(b[Channel::EYE_BLINK_LEFT], 0.0);
        assert_eq!(b[Channel::MOUTH_SMILE_LEFT], 0.4);
        assert_eq!((b[Channel::JAW_OPEN], b[Channel::HEAD_PITCH]), (0.3, 0.1));
        assert_eq!(b[Channel::HEAD_YAW], -0.2);
        b.mirror();
        assert_eq!(b, original);
    }

    #[test]
    fn gaze_helpers_round_trip_and_follow_the_ownership_of_sides() {
        let mut b = ChannelBuf::NEUTRAL;
        b.set_gaze(0.4, -0.25);
        assert_eq!(b[Channel::EYE_LOOK_OUT_LEFT], 0.4, "toward her left: left eye out");
        assert_eq!(b[Channel::EYE_LOOK_IN_RIGHT], 0.4, "toward her left: right eye in");
        assert_eq!(b[Channel::EYE_LOOK_DOWN_LEFT], 0.25);
        assert_eq!(b.gaze(), (0.4, -0.25));
        b.set_gaze(-0.1, 0.0);
        assert_eq!((b[Channel::EYE_LOOK_OUT_LEFT], b[Channel::EYE_LOOK_IN_LEFT]), (0.0, 0.1));
        assert_eq!(b.gaze(), (-0.1, 0.0));
    }

    #[test]
    fn sanitize_clamps_and_replaces_non_finite() {
        let mut b = ChannelBuf::NEUTRAL;
        b[Channel::JAW_OPEN] = 3.0;
        b[Channel::EYE_BLINK_LEFT] = f32::NAN;
        b[Channel::HEAD_YAW] = -9.0;
        b[Channel::BLUSH] = f32::INFINITY;
        b.sanitize();
        assert_eq!(b[Channel::JAW_OPEN], 1.0);
        assert_eq!(b[Channel::EYE_BLINK_LEFT], 0.0);
        assert_eq!(b[Channel::HEAD_YAW], -1.75);
        assert_eq!(b[Channel::BLUSH], 0.0);
    }

    #[test]
    fn checksum_is_stable_and_sensitive() {
        let mut b = ChannelBuf::NEUTRAL;
        let zero = b.checksum();
        assert_eq!(zero, ChannelBuf::NEUTRAL.checksum());
        b[Channel::JAW_OPEN] = 0.5;
        assert_ne!(b.checksum(), zero);
        // Pinned: if this changes, every golden digest in the workspace changes with it.
        assert_eq!(ChannelBuf::NEUTRAL.checksum(), 0x7da1_44b9_7d05_4b25);
    }

    #[test]
    fn serde_uses_names_and_skips_neutral() {
        let mut b = ChannelBuf::NEUTRAL;
        b[Channel::JAW_OPEN] = 0.5;
        b[Channel::EYE_BLINK_LEFT] = 1.0;
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(json, r#"{"eyeBlinkLeft":1.0,"jawOpen":0.5}"#);
        assert_eq!(serde_json::from_str::<ChannelBuf>(&json).unwrap(), b);
        assert!(serde_json::from_str::<ChannelBuf>(r#"{"jawOpne":1.0}"#).is_err());
        assert_eq!(serde_json::to_string(&Channel::JAW_OPEN).unwrap(), "\"jawOpen\"");
        let mask: ChannelMask = serde_json::from_str(r#"["jawOpen","headYaw"]"#).unwrap();
        assert_eq!(mask.count(), 2);
    }

    #[test]
    fn schema_lists_the_valid_names() {
        let schema = serde_json::to_value(schemars::schema_for!(Channel)).unwrap();
        let names = schema["enum"].as_array().unwrap();
        assert_eq!(names.len(), CHANNELS.len());
        assert!(names.iter().any(|n| n == "tongueOut"));
    }
}
