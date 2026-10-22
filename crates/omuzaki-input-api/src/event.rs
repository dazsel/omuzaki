//! What the frame thread sees (design §3): zone edges, mouse and pen buttons, relative pointer motion, the coarse pen,
//! pad edges and levels, and devices coming and going. Every type here is `Copy` and holds no key identity: a key is
//! its [`KeyZone`], a pointer is relative motion (never a screen position), a pen is a 32 x 32 cell of its tablet.
//!
//! Times are pipeline time ([`Seconds`]), quantised to the 1/120 s coalescing period on the capture thread.

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::zones::KeyZone;

/// A kind of input device.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClass {
    /// A keyboard.
    Keyboard,
    /// A mouse or trackpad (relative motion only).
    Mouse,
    /// A pen on a tablet.
    Pen,
    /// A gamepad.
    Gamepad,
    /// A MIDI controller: its notes, controls and program changes, for bindings only (#469).
    Midi,
    /// OSC messages on this machine that press bindings by id (#469): a control surface, a Stream Deck through its
    /// OSC plugin or Companion, a script.
    Osc,
}

impl DeviceClass {
    /// Every class, in a fixed order (their bits in [`DeviceMask`]).
    pub const ALL: [Self; 6] = [Self::Keyboard, Self::Mouse, Self::Pen, Self::Gamepad, Self::Midi, Self::Osc];

    /// The class's bit in a [`DeviceMask`].
    pub const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A set of device classes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct DeviceMask(u8);

impl DeviceMask {
    /// No class.
    pub const NONE: Self = Self(0);
    /// Every class.
    pub const ALL: Self = Self(0b11_1111);

    /// The set holding `classes`.
    pub fn of(classes: &[DeviceClass]) -> Self {
        Self(classes.iter().fold(0, |bits, c| bits | c.bit()))
    }

    /// Whether `class` is in the set.
    pub const fn contains(self, class: DeviceClass) -> bool {
        self.0 & class.bit() != 0
    }

    /// The set with `class` added.
    #[must_use]
    pub const fn with(self, class: DeviceClass) -> Self {
        Self(self.0 | class.bit())
    }

    /// The classes in both sets.
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// The classes in this set and not in `other`.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0 & Self::ALL.0)
    }

    /// The classes in either set.
    #[must_use]
    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether the set is empty.
    pub const fn is_empty(self) -> bool {
        self.0 & Self::ALL.0 == 0
    }

    /// The classes in the set, in [`DeviceClass::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = DeviceClass> {
        DeviceClass::ALL.into_iter().filter(move |c| self.contains(*c))
    }
}

/// One connected device (a keyboard, a mouse, a pad), opaque. A tag is reused only after its [`MotionEvent::SourceGone`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct SourceTag(pub(crate) u8);

impl SourceTag {
    /// The most sources connected at once (each is one bit of a [`SourceMask`]).
    pub const MAX: usize = 64;

    /// The tag with index `index`, below [`SourceTag::MAX`].
    pub const fn new(index: u8) -> Option<Self> {
        if (index as usize) < Self::MAX { Some(Self(index)) } else { None }
    }

    /// Its index.
    pub const fn index(self) -> u8 {
        self.0
    }
}

/// A set of sources, one bit per [`SourceTag`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct SourceMask(u64);

impl SourceMask {
    /// No source.
    pub const NONE: Self = Self(0);
    /// Every source.
    pub const ALL: Self = Self(u64::MAX);

    /// The set holding `src` only.
    pub const fn of(src: SourceTag) -> Self {
        Self(1 << src.0)
    }

    /// The set from its bits.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Its bits.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether `src` is in the set.
    pub const fn contains(self, src: SourceTag) -> bool {
        self.0 & (1 << src.0) != 0
    }

    /// The set with `src` added.
    #[must_use]
    pub const fn with(self, src: SourceTag) -> Self {
        Self(self.0 | (1 << src.0))
    }

    /// The set with `src` removed.
    #[must_use]
    pub const fn without(self, src: SourceTag) -> Self {
        Self(self.0 & !(1 << src.0))
    }

    /// The sources in either set.
    #[must_use]
    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether the set is empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The sources in the set, lowest first.
    pub fn iter(self) -> impl Iterator<Item = SourceTag> {
        (0..64u8).filter(move |i| self.0 & (1 << i) != 0).map(SourceTag)
    }
}

/// The most pads connected at once.
pub const MAX_PADS: usize = 4;

/// A connected pad, stable while it stays connected (a pad's [`SourceTag`] maps to it).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct PadId(u8);

impl PadId {
    /// The pad with index `index`, below [`MAX_PADS`].
    pub const fn new(index: u8) -> Option<Self> {
        if (index as usize) < MAX_PADS { Some(Self(index)) } else { None }
    }

    /// Its index.
    pub const fn index(self) -> u8 {
        self.0
    }
}

/// The connected pads: a fixed array, so listing them never allocates.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PadSet {
    pads: [Option<PadId>; MAX_PADS],
    len: u8,
}

impl PadSet {
    /// An empty set.
    pub const fn new() -> Self {
        Self { pads: [None; MAX_PADS], len: 0 }
    }

    /// Empties it.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Adds `pad`; `false` when it is already there or the set is full.
    pub fn push(&mut self, pad: PadId) -> bool {
        if self.contains(pad) {
            return false;
        }
        match self.pads.get_mut(usize::from(self.len)) {
            Some(slot) => {
                *slot = Some(pad);
                self.len += 1;
                true
            }
            None => false,
        }
    }

    /// Whether `pad` is in it.
    pub fn contains(&self, pad: PadId) -> bool {
        self.iter().any(|p| p == pad)
    }

    /// How many pads it holds.
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether it holds none.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The pads, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = PadId> + '_ {
        self.pads.iter().flatten().copied()
    }
}

/// A pad's controls, grouped as the thumbs and fingers reach them; one bit each.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct PadZones(u16);

impl PadZones {
    /// None.
    pub const NONE: Self = Self(0);
    /// The d-pad.
    pub const DPAD: Self = Self(1 << 0);
    /// The face buttons.
    pub const FACE: Self = Self(1 << 1);
    /// The left stick's click.
    pub const LEFT_STICK: Self = Self(1 << 2);
    /// The right stick's click.
    pub const RIGHT_STICK: Self = Self(1 << 3);
    /// The left shoulder button.
    pub const LEFT_SHOULDER: Self = Self(1 << 4);
    /// The right shoulder button.
    pub const RIGHT_SHOULDER: Self = Self(1 << 5);
    /// The left trigger, pressed past its click point.
    pub const LEFT_TRIGGER: Self = Self(1 << 6);
    /// The right trigger, pressed past its click point.
    pub const RIGHT_TRIGGER: Self = Self(1 << 7);
    /// The centre buttons (start, select, home).
    pub const CENTRE: Self = Self(1 << 8);
    /// Every zone.
    pub const ALL: Self = Self(0x1ff);

    /// Every zone with its name, in bit order.
    pub const NAMED: [(Self, &'static str); 9] = [
        (Self::DPAD, "dpad"),
        (Self::FACE, "face"),
        (Self::LEFT_STICK, "left_stick"),
        (Self::RIGHT_STICK, "right_stick"),
        (Self::LEFT_SHOULDER, "left_shoulder"),
        (Self::RIGHT_SHOULDER, "right_shoulder"),
        (Self::LEFT_TRIGGER, "left_trigger"),
        (Self::RIGHT_TRIGGER, "right_trigger"),
        (Self::CENTRE, "centre"),
    ];

    /// The set from its bits (bits past [`PadZones::ALL`] are dropped).
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// Its bits.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Whether every zone of `other` is in it.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The zones in either.
    #[must_use]
    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The zones in this and not in `other`.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Whether it is empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// A pad's levels, newest wins (edges travel as [`MotionEvent::PadEdges`]).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PadState {
    /// The pad.
    pub pad: PadId,
    /// The left and right sticks, `[x, y]` each, right and up positive, after the dead zone.
    pub sticks: [[i8; 2]; 2],
    /// The left and right triggers, `0..=255`.
    pub triggers: [u8; 2],
    /// The zones held now.
    pub zones: PadZones,
    /// When it was read.
    pub at: Seconds,
}

impl PadState {
    /// A pad at rest: sticks centred, triggers up, nothing held.
    pub const fn neutral(pad: PadId, at: Seconds) -> Self {
        Self { pad, sticks: [[0; 2]; 2], triggers: [0; 2], zones: PadZones::NONE, at }
    }
}

/// One input event the frame thread drains (design §3). `Copy`, at most 32 bytes, no key identity.
///
/// Struct-like variants are `#[non_exhaustive]`; build them with the constructors of the same name.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MotionEvent {
    /// A key went down or up in `zone`.
    #[non_exhaustive]
    Key {
        /// The keyboard.
        src: SourceTag,
        /// Where the key is.
        zone: KeyZone,
        /// Whether it went down.
        down: bool,
        /// When.
        at: Seconds,
    },
    /// Mouse or pen buttons changed, one bit per button (bit 0 the primary).
    #[non_exhaustive]
    Buttons {
        /// The device.
        src: SourceTag,
        /// [`DeviceClass::Mouse`] or [`DeviceClass::Pen`].
        device: DeviceClass,
        /// The buttons that went down.
        pressed: u8,
        /// The buttons that went up.
        released: u8,
        /// When.
        at: Seconds,
    },
    /// Relative pointer motion, coalesced to one event per 1/120 s, in metres on the virtual pad.
    #[non_exhaustive]
    Pointer {
        /// The mouse.
        src: SourceTag,
        /// Rightward motion.
        dx: f32,
        /// Forward (away from the person) motion.
        dy: f32,
        /// Wheel notches, forward positive.
        wheel: f32,
        /// When.
        at: Seconds,
    },
    /// A pen sample, coarse: a 32 x 32 cell of the tablet, 8-bit pressure, 1-degree tilt.
    #[non_exhaustive]
    Pen {
        /// The pen.
        src: SourceTag,
        /// The cell across the tablet, `0..32`.
        u: u8,
        /// The cell down the tablet, `0..32`.
        v: u8,
        /// Pressure, `0..=255`.
        pressure: u8,
        /// Tilt in degrees, `[x, y]`.
        tilt: [i8; 2],
        /// Whether the pen is in range of the tablet.
        in_range: bool,
        /// Whether it touches the tablet.
        contact: bool,
        /// When.
        at: Seconds,
    },
    /// Pad zones pressed and released since the last event of this pad, queued so a tap between two drains is kept.
    #[non_exhaustive]
    PadEdges {
        /// The pad.
        pad: PadId,
        /// The zones that went down.
        pressed: PadZones,
        /// The zones that went up.
        released: PadZones,
        /// When.
        at: Seconds,
    },
    /// A device connected (a pad with its id).
    #[non_exhaustive]
    SourceUp {
        /// The device's tag.
        src: SourceTag,
        /// What it is.
        class: DeviceClass,
        /// Its pad id, for a pad.
        pad: Option<PadId>,
        /// When.
        at: Seconds,
    },
    /// That one device left: consumers drop what it held, and nothing else.
    #[non_exhaustive]
    SourceGone {
        /// The device's tag.
        src: SourceTag,
        /// When.
        at: Seconds,
    },
}

impl MotionEvent {
    /// [`MotionEvent::Key`].
    pub const fn key(src: SourceTag, zone: KeyZone, down: bool, at: Seconds) -> Self {
        Self::Key { src, zone, down, at }
    }

    /// [`MotionEvent::Buttons`].
    pub const fn buttons(src: SourceTag, device: DeviceClass, pressed: u8, released: u8, at: Seconds) -> Self {
        Self::Buttons { src, device, pressed, released, at }
    }

    /// [`MotionEvent::Pointer`].
    pub const fn pointer(src: SourceTag, dx: f32, dy: f32, wheel: f32, at: Seconds) -> Self {
        Self::Pointer { src, dx, dy, wheel, at }
    }

    /// [`MotionEvent::Pen`].
    #[allow(clippy::too_many_arguments)] // the sample's fields, as the variant holds them
    pub const fn pen(
        src: SourceTag,
        u: u8,
        v: u8,
        pressure: u8,
        tilt: [i8; 2],
        in_range: bool,
        contact: bool,
        at: Seconds,
    ) -> Self {
        Self::Pen { src, u, v, pressure, tilt, in_range, contact, at }
    }

    /// [`MotionEvent::PadEdges`].
    pub const fn pad_edges(pad: PadId, pressed: PadZones, released: PadZones, at: Seconds) -> Self {
        Self::PadEdges { pad, pressed, released, at }
    }

    /// [`MotionEvent::SourceUp`].
    pub const fn source_up(src: SourceTag, class: DeviceClass, pad: Option<PadId>, at: Seconds) -> Self {
        Self::SourceUp { src, class, pad, at }
    }

    /// [`MotionEvent::SourceGone`].
    pub const fn source_gone(src: SourceTag, at: Seconds) -> Self {
        Self::SourceGone { src, at }
    }

    /// When it happened.
    pub const fn at(&self) -> Seconds {
        match *self {
            Self::Key { at, .. }
            | Self::Buttons { at, .. }
            | Self::Pointer { at, .. }
            | Self::Pen { at, .. }
            | Self::PadEdges { at, .. }
            | Self::SourceUp { at, .. }
            | Self::SourceGone { at, .. } => at,
        }
    }

    /// The device it came from; `None` for [`MotionEvent::PadEdges`], which names its pad instead.
    pub const fn source(&self) -> Option<SourceTag> {
        match *self {
            Self::Key { src, .. }
            | Self::Buttons { src, .. }
            | Self::Pointer { src, .. }
            | Self::Pen { src, .. }
            | Self::SourceUp { src, .. }
            | Self::SourceGone { src, .. } => Some(src),
            Self::PadEdges { .. } => None,
        }
    }

    /// Whether it ends something held (a key up, a button or pad zone released, a device gone): the events a full
    /// queue must keep.
    pub const fn is_release(&self) -> bool {
        match *self {
            Self::Key { down, .. } => !down,
            Self::Buttons { released, .. } => released != 0,
            Self::PadEdges { released, .. } => !released.is_empty(),
            Self::SourceGone { .. } => true,
            Self::Pointer { .. } | Self::Pen { .. } | Self::SourceUp { .. } => false,
        }
    }
}
