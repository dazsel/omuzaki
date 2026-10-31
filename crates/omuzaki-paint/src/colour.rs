//! Colours as a painter writes them (`#rrggbb` or `#rrggbbaa`, sRGB) and as the canvas holds them (premultiplied
//! floats).
//!
//! The canvas blends in sRGB-encoded values, as Krita, Clip Studio and Photoshop do by default, so a colour picked off
//! a finished illustration lands on the same pixel value when it is painted back flat.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::PaintError;

/// An sRGB colour with straight (not premultiplied) alpha, written `#rrggbb` or `#rrggbbaa`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Colour {
    /// Red, 0 to 255.
    pub r: u8,
    /// Green, 0 to 255.
    pub g: u8,
    /// Blue, 0 to 255.
    pub b: u8,
    /// Alpha, 0 (clear) to 255 (opaque).
    pub a: u8,
}

impl Colour {
    /// Fully transparent black.
    pub const CLEAR: Self = Self { r: 0, g: 0, b: 0, a: 0 };
    /// Opaque black.
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0, a: 255 };
    /// Opaque white.
    pub const WHITE: Self = Self { r: 255, g: 255, b: 255, a: 255 };

    /// An opaque colour.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// The colour as a premultiplied float pixel.
    pub fn premul(self) -> Px {
        let a = f32::from(self.a) / 255.0;
        [f32::from(self.r) / 255.0 * a, f32::from(self.g) / 255.0 * a, f32::from(self.b) / 255.0 * a, a]
    }

    /// The colour of a premultiplied float pixel, rounded to the nearest 8-bit value.
    pub fn from_premul(px: Px) -> Self {
        let a = px[3].clamp(0.0, 1.0);
        if a <= 0.0 {
            return Self::CLEAR;
        }
        let unit = |v: f32| to_u8((v / a).clamp(0.0, 1.0));
        Self { r: unit(px[0]), g: unit(px[1]), b: unit(px[2]), a: to_u8(a) }
    }
}

/// A premultiplied pixel: red, green and blue already multiplied by alpha, each 0 to 1, in sRGB encoding.
pub type Px = [f32; 4];

/// A 0..1 value as the nearest 8-bit level.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255 first
pub(crate) fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8
}

impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.a == 255 {
            write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            write!(f, "#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }
}

impl FromStr for Colour {
    type Err = PaintError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || PaintError::Colour(s.to_owned());
        let hex = s.strip_prefix('#').ok_or_else(bad)?;
        if !hex.is_ascii() {
            return Err(bad());
        }
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2).ok_or_else(bad)?, 16).map_err(|_| bad());
        match hex.len() {
            6 => Ok(Self::rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Ok(Self { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: byte(6)? }),
            _ => Err(bad()),
        }
    }
}

impl Serialize for Colour {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Colour {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Colour {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Colour".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^#([0-9a-fA-F]{6}|[0-9a-fA-F]{8})$",
            "description": "An sRGB colour, `#rrggbb` or `#rrggbbaa` (straight alpha).",
            "examples": ["#1b1420", "#f3d9c8", "#ff5a7a80"]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints_both_forms() {
        let c: Colour = "#1B1420".parse().unwrap();
        assert_eq!(c, Colour::rgb(0x1b, 0x14, 0x20));
        assert_eq!(c.to_string(), "#1b1420");
        let t: Colour = "#ff5a7a80".parse().unwrap();
        assert_eq!(t.a, 0x80);
        assert_eq!(t.to_string(), "#ff5a7a80");
        for bad in ["1b1420", "#1b142", "#1b14200", "#gg0000", "#ééé"] {
            assert!(bad.parse::<Colour>().is_err(), "{bad}");
        }
    }

    #[test]
    fn premultiplies_and_back_exactly_for_opaque() {
        for v in [0u8, 1, 127, 128, 254, 255] {
            let c = Colour::rgb(v, 255 - v, v / 2);
            assert_eq!(Colour::from_premul(c.premul()), c);
        }
    }
}
