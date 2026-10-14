//! Colour: how artists and project files write it (`"#f9d3c5"`), and the exact conversions to what shaders use.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use glam::Vec3;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

mod deficiency;

pub use deficiency::{Deficiency, simulate};

/// An 8-bit sRGB colour with straight (non-premultiplied) alpha.
///
/// It is written `#rrggbb` (opaque) or `#rrggbbaa` in project files, which keeps palettes readable and diffable
/// (spec §7, LK-03). Renderers work in linear light: [`Colour::to_linear`] is the one conversion, so a palette
/// colour looks the same in the mock, the GPU renderer and an exported VRM.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Colour {
    /// Red, sRGB encoded.
    pub r: u8,
    /// Green, sRGB encoded.
    pub g: u8,
    /// Blue, sRGB encoded.
    pub b: u8,
    /// Coverage, 255 = opaque. Linear (alpha is never gamma encoded).
    pub a: u8,
}

/// Why a string isn't a [`Colour`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a colour is written #rrggbb or #rrggbbaa (hex digits); got {0:?}")]
pub struct ColourParseError(pub String);

impl Colour {
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(255, 255, 255);
    /// Fully transparent black.
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    /// An opaque colour.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    /// A colour with alpha.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// An opaque colour from `0xRRGGBB`, the way the prototype writes them (`0xf9d3c5`).
    #[allow(clippy::cast_possible_truncation)] // each byte is masked to 8 bits first
    pub const fn hex(rgb: u32) -> Self {
        Self::rgb((rgb >> 16 & 0xff) as u8, (rgb >> 8 & 0xff) as u8, (rgb & 0xff) as u8)
    }

    /// The colour in linear light (alpha dropped): what lighting maths multiplies.
    pub fn to_linear(self) -> Vec3 {
        Vec3::new(decode_u8(self.r), decode_u8(self.g), decode_u8(self.b))
    }

    /// Alpha as `0.0..=1.0`.
    pub fn alpha(self) -> f32 {
        f32::from(self.a) / 255.0
    }

    /// Encodes a linear colour (clamped to `0..=1`) to sRGB bytes, rounding to nearest. The inverse of
    /// [`Colour::to_linear`] for every 8-bit value.
    pub fn from_linear(rgb: Vec3, alpha: f32) -> Self {
        Self::rgba(encode_u8(rgb.x), encode_u8(rgb.y), encode_u8(rgb.z), to_byte(alpha))
    }
}

/// Rounds a `0..=1` float to the nearest byte (clamping first), so `from_linear(to_linear(c)) == c`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255 before the cast
pub fn to_byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn decode_u8(v: u8) -> f32 {
    srgb_decode(f32::from(v) / 255.0)
}

fn encode_u8(linear: f32) -> u8 {
    to_byte(srgb_encode(linear))
}

/// The sRGB transfer function, encoded `0..=1` to linear light.
pub fn srgb_decode(c: f32) -> f32 {
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// The inverse of [`srgb_decode`]: linear light to encoded.
pub fn srgb_encode(l: f32) -> f32 {
    if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 }
}

impl FromStr for Colour {
    type Err = ColourParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || ColourParseError(s.to_owned());
        let digits = s.strip_prefix('#').ok_or_else(bad)?;
        if !(digits.len() == 6 || digits.len() == 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        let byte = |i: usize| u8::from_str_radix(&digits[2 * i..2 * i + 2], 16).map_err(|_| bad());
        let a = if digits.len() == 8 { byte(3)? } else { 255 };
        Ok(Self::rgba(byte(0)?, byte(1)?, byte(2)?, a))
    }
}

impl TryFrom<String> for Colour {
    type Error = ColourParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Colour> for String {
    fn from(c: Colour) -> Self {
        c.to_string()
    }
}

impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)?;
        if self.a != 255 {
            write!(f, "{:02x}", self.a)?;
        }
        Ok(())
    }
}

impl JsonSchema for Colour {
    // Inline, like `Slug`: a one-line string rule reads better in a flat tool schema than behind a $ref.
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "Colour".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$",
            "description": "An sRGB colour, #rrggbb or #rrggbbaa."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints_both_forms() {
        assert_eq!("#f9d3c5".parse(), Ok(Colour::hex(0x00f9_d3c5)));
        assert_eq!("#F9D3C580".parse(), Ok(Colour::rgba(0xf9, 0xd3, 0xc5, 0x80)));
        assert_eq!(Colour::hex(0x00f9_d3c5).to_string(), "#f9d3c5");
        assert_eq!(Colour::rgba(1, 2, 3, 4).to_string(), "#01020304");
    }

    #[test]
    fn rejects_malformed_text() {
        for bad in ["", "f9d3c5", "#f9d3c", "#f9d3c5f", "#gg0000", "#+9d3c5"] {
            assert!(bad.parse::<Colour>().is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn serde_uses_the_hex_string() {
        let c: Colour = serde_json::from_str("\"#3b2f36\"").unwrap();
        assert_eq!(c, Colour::hex(0x003b_2f36));
        assert_eq!(serde_json::to_string(&c).unwrap(), "\"#3b2f36\"");
        assert!(serde_json::from_str::<Colour>("\"blue\"").is_err());
    }

    #[test]
    fn linear_conversion_round_trips_every_byte() {
        for v in 0..=255u8 {
            let c = Colour::rgb(v, v, v);
            assert_eq!(Colour::from_linear(c.to_linear(), c.alpha()), c, "byte {v}");
        }
    }

    #[test]
    fn linear_values_are_the_srgb_curve() {
        assert_eq!(Colour::BLACK.to_linear(), Vec3::ZERO);
        assert!((Colour::WHITE.to_linear() - Vec3::ONE).abs().max_element() < 1e-6);
        // sRGB 188 (0.737) is the well-known mid grey of ~0.5 linear.
        assert!((decode_u8(188) - 0.5).abs() < 0.01);
    }

    #[test]
    fn schema_is_an_inline_string_pattern() {
        let schema = serde_json::to_value(schemars::schema_for!(Colour)).unwrap();
        assert_eq!(schema["type"], "string");
    }
}
