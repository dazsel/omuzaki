//! The fixed keyboard grid (design §3): sixteen zones by physical position, plus two side-only values for hand-only
//! detail. A zone says where on the board a finger lands, never which key it pressed: the map from a key to its zone is
//! `omuzaki-input`'s alone and is not part of this contract.
//!
//! The key plane's coordinates are `u` across the board (0 at the left edge of the main block, 1 at the right edge of
//! the numpad) and `v` from front to back (0 at the front edge of the space row, 1 at the back edge of the function
//! row). Centres are taken from a full-size board, 22.5 key units wide and six rows deep.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A hand.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The left hand.
    Left,
    /// The right hand.
    Right,
}

/// The finger a zone's keys are touch-typed with (the middle of its columns, for a zone that spans several).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Finger {
    /// The thumb.
    Thumb,
    /// The index finger.
    Index,
    /// The middle finger.
    Middle,
    /// The ring finger.
    Ring,
    /// The little finger.
    Pinky,
}

/// The row band a zone sits in, front to back.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    /// The space and modifier row, and the arrows.
    Thumb,
    /// The home and bottom letter rows.
    Lower,
    /// The number and top letter rows.
    Upper,
    /// The function row.
    Function,
}

/// What a zone means to the motion: which hand, which finger, which band, and where its centre is on the key plane.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZoneInfo {
    /// The hand that types it; `None` for a zone either hand reaches (Space, the function row, "other").
    pub side: Option<Side>,
    /// The finger that types it; `None` for the side-only values, whose finger the motion draws itself.
    pub finger: Option<Finger>,
    /// Its row band.
    pub band: Band,
    /// Its centre `(u, v)` on the key plane (module docs), each in `0..=1`.
    pub centre: [f32; 2],
}

/// A cell of the fixed keyboard grid. An index, once assigned, never changes meaning (like `Channel`).
///
/// Indices `0..KeyZone::COUNT` are the sixteen zones; [`KeyZone::LEFT_ONLY`] and [`KeyZone::RIGHT_ONLY`] are the two
/// side-only values that hand-only detail sends instead.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyZone(u8);

macro_rules! zones {
    ($($(#[$doc:meta])* $konst:ident = $index:literal, $name:literal, $side:expr, $finger:expr, $band:ident, [$u:literal, $v:literal];)*) => {
        impl KeyZone {
            $($(#[$doc])* pub const $konst: Self = Self($index);)*

            /// The sixteen zones, in index order.
            pub const ALL: [Self; Self::COUNT] = [$(Self::$konst),*];

            const NAMES: [&'static str; Self::COUNT + 2] = [$($name,)* "left_only", "right_only"];

            const INFO: [ZoneInfo; Self::COUNT + 2] = [
                $(ZoneInfo { side: $side, finger: $finger, band: Band::$band, centre: [$u, $v] },)*
                ZoneInfo { side: Some(Side::Left), finger: None, band: Band::Lower, centre: [0.2, 0.5] },
                ZoneInfo { side: Some(Side::Right), finger: None, band: Band::Lower, centre: [0.47, 0.5] },
            ];
        }
    };
}

use Finger::{Index, Middle, Pinky, Ring, Thumb};
use Side::{Left, Right};

zones! {
    /// `1 2 3 Q W E`, Tab, Esc.
    LEFT_OUTER_UPPER = 0, "left_outer_upper", Some(Left), Some(Ring), Upper, [0.11, 0.665];
    /// `A S D Z X C`, Caps Lock, left Shift.
    LEFT_OUTER_LOWER = 1, "left_outer_lower", Some(Left), Some(Ring), Lower, [0.133, 0.335];
    /// `4 5 R T`.
    LEFT_INNER_UPPER = 2, "left_inner_upper", Some(Left), Some(Index), Upper, [0.233, 0.665];
    /// `F G V B`.
    LEFT_INNER_LOWER = 3, "left_inner_lower", Some(Left), Some(Index), Lower, [0.256, 0.335];
    /// `6 7 Y U`.
    RIGHT_INNER_UPPER = 4, "right_inner_upper", Some(Right), Some(Index), Upper, [0.322, 0.665];
    /// `H J N M`.
    RIGHT_INNER_LOWER = 5, "right_inner_lower", Some(Right), Some(Index), Lower, [0.344, 0.335];
    /// `8 9 0 - = I O P [ ] \` and Backspace.
    RIGHT_OUTER_UPPER = 6, "right_outer_upper", Some(Right), Some(Ring), Upper, [0.489, 0.665];
    /// `K L ; ' , . /`, Enter, right Shift.
    RIGHT_OUTER_LOWER = 7, "right_outer_lower", Some(Right), Some(Ring), Lower, [0.489, 0.335];
    /// The space bar: a thumb of the hand not on the mouse.
    SPACE = 8, "space", None, Some(Thumb), Thumb, [0.311, 0.08];
    /// Left Ctrl, the left system key and left Alt.
    LEFT_MODIFIERS = 9, "left_modifiers", Some(Left), Some(Pinky), Thumb, [0.067, 0.08];
    /// Right Alt, the right system key, Menu and right Ctrl.
    RIGHT_MODIFIERS = 10, "right_modifiers", Some(Right), Some(Pinky), Thumb, [0.578, 0.08];
    /// The function row.
    FUNCTION = 11, "function", None, Some(Middle), Function, [0.378, 0.92];
    /// The navigation block (Insert, Home, Page Up, Delete, End, Page Down, and the row above it).
    NAVIGATION = 12, "navigation", Some(Right), Some(Middle), Upper, [0.756, 0.7];
    /// The arrows.
    ARROWS = 13, "arrows", Some(Right), Some(Middle), Thumb, [0.778, 0.165];
    /// The numpad.
    NUMPAD = 14, "numpad", Some(Right), Some(Middle), Lower, [0.922, 0.42];
    /// Any other key (media keys, language keys): placed at the main block's centre.
    OTHER = 15, "other", None, Some(Index), Lower, [0.378, 0.5];
}

impl KeyZone {
    /// The number of zones (the side-only values are not counted).
    pub const COUNT: usize = 16;

    /// Hand-only detail (design §6.2): a key of the left hand, with no zone.
    pub const LEFT_ONLY: Self = Self(16);

    /// Hand-only detail: a key of the right hand, with no zone.
    pub const RIGHT_ONLY: Self = Self(17);

    /// The zone with index `index`: `0..16` for the zones, 16 and 17 for the side-only values.
    pub const fn new(index: u8) -> Option<Self> {
        if (index as usize) < Self::COUNT + 2 { Some(Self(index)) } else { None }
    }

    /// Its index.
    pub const fn index(self) -> u8 {
        self.0
    }

    /// Whether it is one of the two side-only values.
    pub const fn is_side_only(self) -> bool {
        self.0 as usize >= Self::COUNT
    }

    /// The side-only value for `side`.
    pub const fn side_only(side: Side) -> Self {
        match side {
            Side::Left => Self::LEFT_ONLY,
            Side::Right => Self::RIGHT_ONLY,
        }
    }

    /// Its side, finger, band and centre.
    pub fn info(self) -> ZoneInfo {
        // Every `KeyZone` indexes the table (`new` checks); the fallback only keeps this free of a panic path.
        Self::INFO.get(usize::from(self.0)).copied().unwrap_or(ZoneInfo {
            side: None,
            finger: Some(Finger::Index),
            band: Band::Lower,
            centre: [0.378, 0.5],
        })
    }

    /// Its stable name (`left_outer_upper`, `space`, `left_only`, ...), as the recorded-input text and serde write it.
    pub fn name(self) -> &'static str {
        Self::NAMES.get(usize::from(self.0)).copied().unwrap_or("other")
    }

    /// The zone named `name` (as [`name`](Self::name) writes it).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::NAMES.iter().position(|n| *n == name).and_then(|i| u8::try_from(i).ok()).map(Self)
    }
}

impl std::fmt::Debug for KeyZone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KeyZone({})", self.name())
    }
}

impl Serialize for KeyZone {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for KeyZone {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::from_name(&name).ok_or_else(|| serde::de::Error::custom(format!("no key zone is named `{name}`")))
    }
}

impl JsonSchema for KeyZone {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "KeyZone".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A cell of the fixed keyboard grid, or a side-only value under hand-only detail.",
            "type": "string",
            "enum": Self::NAMES,
        })
    }
}
