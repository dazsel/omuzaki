//! The performer's names: voices, hosted connectors and persona revisions.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use omuzaki_core::id::Slug;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::limits::PERSONA_REVISION_HEX;

omuzaki_core::slug_id!(
    /// A hosted connector (CN-10), such as a TTS provider. A local stand-in until `omuzaki-connect-api` (ADR 0030)
    /// lands; then this becomes a re-export of its id, with the same slug rules and serialised form.
    ConnectorId
);

/// Why a string isn't a [`VoiceId`] or a [`PersonaRevision`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{what} {got:?}: {rule}")]
pub struct IdError {
    /// What was being parsed.
    pub what: &'static str,
    /// The text that was given.
    pub got: String,
    /// The rule it breaks.
    pub rule: &'static str,
}

impl IdError {
    fn new(what: &'static str, got: &str, rule: &'static str) -> Self {
        Self { what, got: got.to_owned(), rule }
    }
}

/// A voice: `<source>/<voice>`, where the source is a local model id (`kokoro-82m/af_heart`) or a hosted connector id
/// (`<connector>/<voice>`), as `ai-performer-voice-face.md` §3.4 names them.
///
/// The source is a [`Slug`]. The voice part is the provider's own name, so it is wider than a slug: 1 to
/// [`VoiceId::MAX_VOICE_LEN`] ASCII letters, digits, `-`, `_` or `.`, case kept (hosted providers use mixed-case ids).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct VoiceId {
    full: String,
    split: usize,
}

impl VoiceId {
    /// The longest voice part, in characters.
    pub const MAX_VOICE_LEN: usize = 128;

    /// Parses `<source>/<voice>`.
    ///
    /// # Errors
    /// [`IdError`] naming the rule the text breaks.
    pub fn new(s: &str) -> Result<Self, IdError> {
        const WHAT: &str = "voice id";
        let (source, voice) = s.split_once('/').ok_or_else(|| IdError::new(WHAT, s, "use <source>/<voice>"))?;
        Slug::new(source).map_err(|_| IdError::new(WHAT, s, "the source must be a model or connector id"))?;
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        if voice.is_empty() || voice.len() > Self::MAX_VOICE_LEN || !voice.chars().all(allowed) {
            return Err(IdError::new(WHAT, s, "the voice must be 1 to 128 of a-z, A-Z, 0-9, '-', '_' or '.'"));
        }
        Ok(Self { full: s.to_owned(), split: source.len() })
    }

    /// The model or connector the voice belongs to.
    pub fn source(&self) -> &str {
        &self.full[..self.split]
    }

    /// The voice's name at its source.
    pub fn voice(&self) -> &str {
        &self.full[self.split + 1..]
    }

    /// The whole id.
    pub fn as_str(&self) -> &str {
        &self.full
    }
}

/// The revision of a persona: the first [`PERSONA_REVISION_HEX`] lowercase hex characters of the BLAKE3 of its
/// canonical TOML (`ai-performer.md` §4). Stamped as `p` on every transcript line, so a transcript says which persona
/// spoke. Whoever saves the persona computes it; the contract only checks its shape.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct PersonaRevision(String);

impl PersonaRevision {
    /// Checks the shape of a revision.
    ///
    /// # Errors
    /// [`IdError`] unless `s` is exactly [`PERSONA_REVISION_HEX`] characters of `0-9` and `a-f`.
    pub fn new(s: &str) -> Result<Self, IdError> {
        let hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
        if s.len() == PERSONA_REVISION_HEX && s.chars().all(hex) {
            Ok(Self(s.to_owned()))
        } else {
            Err(IdError::new("persona revision", s, "use 12 characters of 0-9 and a-f"))
        }
    }

    /// The revision as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Serde, `Display`, `FromStr` and a string schema for an id that parses with `new(&str)`.
macro_rules! string_id {
    ($ty:ident, $pattern:literal, $description:literal) => {
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $ty {
            type Err = IdError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::new(&s).map_err(serde::de::Error::custom)
            }
        }

        impl JsonSchema for $ty {
            fn schema_name() -> Cow<'static, str> {
                stringify!($ty).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "pattern": $pattern, "description": $description })
            }
        }
    };
}

string_id!(
    VoiceId,
    "^[a-z0-9][a-z0-9_-]{0,63}/[A-Za-z0-9._-]{1,128}$",
    "A voice: <model or connector id>/<voice>."
);
string_id!(PersonaRevision, "^[0-9a-f]{12}$", "A persona revision: 12 hex characters of the BLAKE3 of its TOML.");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_ids_split_into_source_and_voice() {
        let id = VoiceId::new("kokoro-82m/af_heart").unwrap();
        assert_eq!((id.source(), id.voice(), id.as_str()), ("kokoro-82m", "af_heart", "kokoro-82m/af_heart"));
        let hosted = VoiceId::new("tts-cloud/Rachel.v2").unwrap();
        assert_eq!(hosted.voice(), "Rachel.v2");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"kokoro-82m/af_heart\"");
        assert_eq!(serde_json::from_str::<VoiceId>("\"kokoro-82m/af_heart\"").unwrap(), id);
    }

    #[test]
    fn bad_voice_ids_are_refused() {
        for bad in ["af_heart", "Kokoro/af", "kokoro/", "kokoro/a b", "/af", "kokoro/a/b"] {
            assert!(VoiceId::new(bad).is_err(), "{bad} was accepted");
        }
        assert!(VoiceId::new(&format!("k/{}", "a".repeat(VoiceId::MAX_VOICE_LEN))).is_ok());
        assert!(VoiceId::new(&format!("k/{}", "a".repeat(VoiceId::MAX_VOICE_LEN + 1))).is_err());
        assert!(serde_json::from_str::<VoiceId>("\"nope\"").is_err());
    }

    #[test]
    fn persona_revisions_are_twelve_lowercase_hex() {
        assert_eq!(PersonaRevision::new("3f9a1c0e7b2d").unwrap().as_str(), "3f9a1c0e7b2d");
        for bad in ["3f9a1c0e7b2", "3f9a1c0e7b2d0", "3F9A1C0E7B2D", "3f9a1c0e7b2g"] {
            assert!(PersonaRevision::new(bad).is_err(), "{bad} was accepted");
        }
    }
}
