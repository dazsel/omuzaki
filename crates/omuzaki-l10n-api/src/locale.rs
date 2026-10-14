//! `Locale`: a validated BCP 47 language tag, and the list of languages Omuzaki ships (SE-03, design 3.3).

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use unic_langid::LanguageIdentifier;

/// A language tag that parsed as BCP 47, in canonical case (`pt-BR`, `zh-Hans`, `es-419`).
///
/// Used by the catalog to pick resources, by Settings to store the language and by the CLI for `--lang`. The shipped
/// languages and the pseudo-locale are the constants below.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Locale(Cow<'static, str>);

/// Why a tag was refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("`{tag}` is not a valid BCP 47 language tag")]
pub struct LocaleError {
    /// The text that was refused.
    pub tag: String,
}

impl Locale {
    /// English, the source language and the last fallback.
    pub const EN: Self = Self(Cow::Borrowed("en"));
    /// Japanese.
    pub const JA: Self = Self(Cow::Borrowed("ja"));
    /// Korean.
    pub const KO: Self = Self(Cow::Borrowed("ko"));
    /// Simplified Chinese.
    pub const ZH_HANS: Self = Self(Cow::Borrowed("zh-Hans"));
    /// Latin American Spanish, the one neutral Spanish of v1.0.
    pub const ES_419: Self = Self(Cow::Borrowed("es-419"));
    /// Brazilian Portuguese.
    pub const PT_BR: Self = Self(Cow::Borrowed("pt-BR"));
    /// The pseudo-locale ("English with accents"): never shipped, used to find hard-coded text.
    pub const PSEUDO: Self = Self(Cow::Borrowed("en-XA"));

    /// Parses and canonicalises a tag.
    ///
    /// # Errors
    /// [`LocaleError`] when the tag is not valid BCP 47.
    pub fn new(tag: &str) -> Result<Self, LocaleError> {
        // BCP 47 separates with `-`; the parser also takes the POSIX `en_US`, which the OS-locale code must convert first.
        let id = LanguageIdentifier::from_str(tag)
            .ok()
            .filter(|_| !tag.contains('_'))
            .ok_or_else(|| LocaleError { tag: tag.to_owned() })?;
        Ok(Self(Cow::Owned(id.to_string())))
    }

    /// The canonical tag.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The locale this one overrides, if it is a regional override listed in [`OVERRIDES`]: `es-ES` gives `es-419`.
    /// The catalog's chain is the locale, then its parent, then English.
    pub fn parent(&self) -> Option<Self> {
        OVERRIDES.iter().find(|(over, _)| over == self).map(|(_, parent)| parent.clone())
    }
}

/// The six languages that ship at v1.0, English first. Defined once: `register_locales!` embeds exactly these (plus
/// [`OVERRIDES`]), and blessing creates stubs for a language added here (SE-09 adds `zh-Hant`).
pub const SHIPPED: &[Locale] = &[Locale::EN, Locale::JA, Locale::KO, Locale::ZH_HANS, Locale::ES_419, Locale::PT_BR];

/// Regional overrides as `(override, parent)`: a locale whose files hold only the entries that differ from its parent
/// in [`SHIPPED`]. Empty at v1.0; `es-ES` over `es-419` is the expected first entry.
pub const OVERRIDES: &[(Locale, Locale)] = &[];

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Locale {
    type Err = LocaleError;

    fn from_str(tag: &str) -> Result<Self, LocaleError> {
        Self::new(tag)
    }
}

impl Serialize for Locale {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Locale {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let tag = String::deserialize(d)?;
        Self::new(&tag).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Locale {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Locale")
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "description": "A BCP 47 language tag, such as `pt-BR`." })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_constants_are_canonical_valid_tags() {
        for locale in SHIPPED.iter().chain([&Locale::PSEUDO]) {
            assert_eq!(Locale::new(locale.as_str()).as_ref(), Ok(locale), "{locale}");
        }
        assert_eq!(SHIPPED.len(), 6);
    }

    #[test]
    fn tags_are_canonicalised_and_garbage_is_refused() {
        assert_eq!(Locale::new("pt-br"), Ok(Locale::PT_BR));
        assert_eq!(Locale::new("ZH-hans"), Ok(Locale::ZH_HANS));
        assert_eq!("es-419".parse(), Ok(Locale::ES_419));
        for bad in ["", "not a tag", "en_US", "e", "12"] {
            assert!(Locale::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn serde_validates() {
        assert_eq!(serde_json::to_string(&Locale::JA).unwrap(), "\"ja\"");
        assert_eq!(serde_json::from_str::<Locale>("\"pt-br\"").unwrap(), Locale::PT_BR);
        assert!(serde_json::from_str::<Locale>("\"???\"").is_err());
    }

    #[test]
    fn a_shipped_locale_has_no_parent_and_an_override_points_at_a_shipped_one() {
        assert!(SHIPPED.iter().all(|l| l.parent().is_none()));
        for (over, parent) in OVERRIDES {
            assert_eq!(over.parent().as_ref(), Some(parent));
            assert!(SHIPPED.contains(parent), "{over} overrides {parent}, which is not shipped");
        }
    }
}
