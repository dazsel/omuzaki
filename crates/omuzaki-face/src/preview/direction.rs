//! Whether a channel moves the face the right way and on the right side (#229, VF-02): the channel sweep's second check,
//! after "does it move at all".
//!
//! Each face channel has an entry in `library/sweep/expected.toml`: the quantity of the paint parameters it moves (a
//! [`Measure`]), the sign of that move when the channel rises, and, for a sided channel, the half of the face the painted
//! change must mostly fall on. The way is measured on the paint parameters the painter draws from (the eye's aperture,
//! a brow's height, a mouth corner's lift, a pupil's position), so it reads what the face was asked to draw, not how many
//! pixels changed; the side is measured on the painted layers, so a painter that draws the left eye's parameters on the
//! right eye is caught too.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use omuzaki_anim_api::Channel;
use omuzaki_face_api::PaintParams;
use serde::{Deserialize, Serialize};

/// The expected-effect table, embedded.
const EXPECTED: &str = include_str!("../../library/sweep/expected.toml");

/// A measure must move by more than this, in its own units, to count as moving.
const MOVED: f32 = 1e-4;

/// The character's side of the face.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The character's left (`+x`, the right of the picture).
    Left,
    /// The character's right.
    Right,
}

/// One channel's entry in the table.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    /// The measure it moves, by name; `None` only for an inert channel.
    #[serde(default)]
    pub measure: Option<String>,
    /// `+1`: raising the channel raises the measure; `-1`: it lowers it.
    #[serde(default)]
    pub sign: i8,
    /// The half of the face the painted change falls mostly on, for a sided channel.
    #[serde(default)]
    pub side: Option<Side>,
    /// The contract gives the channel no painted effect: nothing is checked.
    #[serde(default)]
    pub inert: bool,
}

/// The whole table, by channel name.
pub type ExpectedTable = BTreeMap<String, Expected>;

/// The embedded table, parsed once.
///
/// # Errors
/// The table doesn't parse, or names a measure [`measure`] doesn't know, or has a sign other than ±1: a bug in the
/// embedded data, which the crate's tests catch.
pub fn expected() -> Result<&'static ExpectedTable, String> {
    static TABLE: OnceLock<Result<ExpectedTable, String>> = OnceLock::new();
    TABLE.get_or_init(|| parse(EXPECTED)).as_ref().map_err(Clone::clone)
}

/// Parses and checks a table.
///
/// # Errors
/// See [`expected`].
pub fn parse(text: &str) -> Result<ExpectedTable, String> {
    let table: ExpectedTable = toml::from_str(text).map_err(|e| format!("the expected-effect table: {e}"))?;
    let probe = omuzaki_face_api::FaceMapping::default().rest_params();
    for (channel, entry) in &table {
        if entry.inert {
            continue;
        }
        let name = entry.measure.as_deref().ok_or_else(|| format!("{channel}: no measure and not inert"))?;
        if measure(name, &probe).is_none() {
            return Err(format!("{channel}: unknown measure '{name}'"));
        }
        if entry.sign.abs() != 1 {
            return Err(format!("{channel}: sign {} is not +1 or -1", entry.sign));
        }
    }
    Ok(table)
}

/// The value of the measure called `name` in `p`, or `None` for a name the table may not use.
pub fn measure(name: &str, p: &PaintParams) -> Option<f32> {
    let eye = |e: &omuzaki_face_api::EyeParams| e.open * (1.0 - e.close.max(e.arc));
    // A brow's inner end: its height less its tilt (a positive tilt drops the inner end).
    let inner = |b: &omuzaki_face_api::BrowParams| b.height - b.tilt;
    let m = &p.mouth;
    let e = &p.effects;
    Some(match name {
        "eye_open.left" => eye(&p.left_eye),
        "eye_open.right" => eye(&p.right_eye),
        "lower_lid.left" => p.left_eye.lower_lid,
        "lower_lid.right" => p.right_eye.lower_lid,
        "brow_height.left" => p.left_brow.height,
        "brow_height.right" => p.right_brow.height,
        "brow_inner" => inner(&p.left_brow) + inner(&p.right_brow),
        "gaze_y" => p.gaze.y,
        // Positive vergence turns both irises toward the nose: the left (+x) eye moves to -x, the right one to +x.
        "pupil_x.left" => p.gaze.x - p.vergence,
        "pupil_x.right" => p.gaze.x + p.vergence,
        "mouth_open" => m.open,
        "mouth_wide" => m.wide,
        "mouth_round" => m.round,
        "mouth_press" => m.press,
        "mouth_pucker" => m.pucker,
        "mouth_shift" => m.shift,
        "corner.left" => m.smile_left,
        "corner.right" => m.smile_right,
        "teeth_upper" => m.teeth_upper,
        "tongue" => m.tongue,
        "fangs" => m.fangs,
        "blush" => p.blush,
        "cheek_puff" => p.cheek_puff,
        "sweat" => p.sweat,
        "effect.heart" => e.heart,
        "effect.star" => e.star,
        "effect.gloom" => e.gloom,
        "effect.shock" => e.shock,
        "effect.squeeze" => e.squeeze,
        "effect.spiral" => e.spiral,
        "effect.sparkle" => e.sparkle,
        "effect.tears" => e.tears,
        _ => return None,
    })
}

/// Painted pixels that changed on each of the character's sides.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SideChange {
    /// On the character's left half (`+x`).
    pub left: u32,
    /// On the character's right half.
    pub right: u32,
}

/// What the direction check found for one channel.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// It moves its measure the expected way, on the expected side.
    Right,
    /// Its measure moved the other way, or not at all.
    WrongWay,
    /// The way is right, but most of the painted change is on the other half of the face.
    WrongSide,
    /// The contract gives it no painted effect.
    Inert,
    /// The table has no entry for it.
    Missing,
    /// It is dead or switched off in the face's design (its sweep status says which): there is no move to check.
    Unchecked,
}

/// Checks one push: `rest` and `pushed` are the paint parameters before and after, `raised` whether the channel was
/// pushed up (a channel resting at its maximum is pushed down, which reverses the expected sign), `sides` where the
/// painted change fell.
pub fn check(
    expected: Option<&Expected>,
    rest: &PaintParams,
    pushed: &PaintParams,
    raised: bool,
    sides: SideChange,
) -> Direction {
    let Some(entry) = expected else {
        return Direction::Missing;
    };
    if entry.inert {
        return Direction::Inert;
    }
    let Some(name) = entry.measure.as_deref() else {
        return Direction::Missing;
    };
    let (Some(before), Some(after)) = (measure(name, rest), measure(name, pushed)) else {
        return Direction::Missing;
    };
    let want = f32::from(entry.sign) * if raised { 1.0 } else { -1.0 };
    if (after - before) * want <= MOVED {
        return Direction::WrongWay;
    }
    let wrong_side = match entry.side {
        Some(Side::Left) => sides.left <= sides.right,
        Some(Side::Right) => sides.right <= sides.left,
        None => false,
    };
    if wrong_side { Direction::WrongSide } else { Direction::Right }
}

/// The table's entry for `channel`.
pub fn entry(table: &ExpectedTable, channel: Channel) -> Option<&Expected> {
    table.get(channel.name())
}
