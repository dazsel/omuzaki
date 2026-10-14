//! Palette slots (spec LK-03): characters name colours by *slot*, so a colourway swaps in one edit.
//!
//! A [`ColourRef`] is what a parameter struct stores: a fixed [`Colour`] or `@slot`. The renderer's materials take
//! plain `Colour`s, so whoever assembles a material resolves its refs through the character's [`Palette`].

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use omuzaki_core::colour::Colour;
use omuzaki_core::slug_id;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

slug_id!(
    /// A named colour slot of a character's palette: `hair_root`, `skin`, `eyes`, `outfit_a`, `trim`...
    PaletteSlot
);

/// Why a colour reference isn't usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ColourError {
    /// Not `#rrggbb`, `#rrggbbaa` or `@slot_name`.
    #[error("'{0}' isn't a colour: use #rrggbb or #rrggbbaa, or @slot_name for a palette slot")]
    Syntax(String),
    /// A `@slot` that the palette doesn't define.
    #[error("the palette has no slot '{0}'")]
    UnknownSlot(PaletteSlot),
}

/// A colour in a parameter struct: either a fixed value or a palette slot. Written as `#rrggbb` or `@slot_name`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ColourRef {
    /// This exact colour.
    Fixed(Colour),
    /// Whatever the character's palette says for the slot.
    Slot(PaletteSlot),
}

impl ColourRef {
    /// A fixed opaque colour; `const` so style defaults read as plain constants.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::Fixed(Colour::rgb(r, g, b))
    }

    /// A fixed colour with alpha.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::Fixed(Colour::rgba(r, g, b, a))
    }

    /// A palette slot, or `fallback` if `name` isn't a valid slot name (defaults use it so they never panic).
    pub fn slot_or(name: &str, fallback: Self) -> Self {
        PaletteSlot::new(name).map_or(fallback, Self::Slot)
    }

    /// Parses `#rrggbb[aa]` or `@slot`.
    ///
    /// # Errors
    /// [`ColourError::Syntax`] for anything else.
    pub fn parse(s: &str) -> Result<Self, ColourError> {
        match s.strip_prefix('@') {
            Some(slot) => PaletteSlot::new(slot).map(Self::Slot).map_err(|_| ColourError::Syntax(s.to_owned())),
            None => s.parse::<Colour>().map(Self::Fixed).map_err(|_| ColourError::Syntax(s.to_owned())),
        }
    }
}

impl fmt::Display for ColourRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed(c) => c.fmt(f),
            Self::Slot(s) => write!(f, "@{s}"),
        }
    }
}

impl Serialize for ColourRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ColourRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for ColourRef {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "ColourRef".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^(#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?|@[a-z0-9][a-z0-9_-]{0,63})$",
            "description": "A colour: #rrggbb (or #rrggbbaa), or @slot_name to use the character's palette."
        })
    }
}

/// A character's named colours (LK-03). Parts refer to slots; changing a slot recolours everything that uses it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Palette(pub BTreeMap<PaletteSlot, Colour>);

impl Palette {
    /// The concrete colour for `r`.
    ///
    /// # Errors
    /// [`ColourError::UnknownSlot`] if `r` names a slot the palette doesn't define.
    pub fn resolve(&self, r: &ColourRef) -> Result<Colour, ColourError> {
        match r {
            ColourRef::Fixed(c) => Ok(*c),
            ColourRef::Slot(s) => self.0.get(s).copied().ok_or_else(|| ColourError::UnknownSlot(s.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_references_say_what_is_accepted() {
        for bad in ["f3d8c6", "#fff", "red", "", "@Bad Slot", "@"] {
            let e = ColourRef::parse(bad).unwrap_err().to_string();
            assert!(e.contains("#rrggbb") && e.contains("@slot_name"), "{e}");
        }
    }

    #[test]
    fn refs_resolve_through_the_palette() {
        let mut p = Palette::default();
        p.0.insert(PaletteSlot::new("eyes").unwrap(), Colour::rgb(1, 2, 3));
        assert_eq!(p.resolve(&ColourRef::parse("@eyes").unwrap()), Ok(Colour::rgb(1, 2, 3)));
        assert_eq!(p.resolve(&ColourRef::rgb(10, 11, 12)), Ok(Colour::rgb(10, 11, 12)));
        assert!(matches!(p.resolve(&ColourRef::parse("@hair").unwrap()), Err(ColourError::UnknownSlot(_))));
    }

    #[test]
    fn serde_uses_the_text_form() {
        let r: ColourRef = serde_json::from_str("\"@hair_root\"").unwrap();
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"@hair_root\"");
        let fixed: ColourRef = serde_json::from_str("\"#3a2420\"").unwrap();
        assert_eq!(fixed, ColourRef::rgb(0x3a, 0x24, 0x20));
        assert!(serde_json::from_str::<ColourRef>("\"nope\"").is_err());
        assert_eq!(ColourRef::slot_or("not a slot!", ColourRef::rgb(1, 1, 1)), ColourRef::rgb(1, 1, 1));
    }
}
