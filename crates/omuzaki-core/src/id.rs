//! Human-readable ids ("mira", "close-up") used as project-file keys, folder names and op arguments.
//!
//! One validated [`Slug`] type underlies every typed id (declared with [`slug_id!`]), so the rules for what a
//! name may look like live in exactly one place, and a `CharId` can never be passed where a `SceneId` is expected.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// A validated name: 1–64 chars of `a-z`, `0-9`, `-`, `_`, starting with a letter or digit.
///
/// Slugs are safe as file and folder names on every OS and need no quoting in TOML keys, which keeps project
/// files git-friendly (spec §7).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Slug(String);

/// Why a string isn't a valid [`Slug`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SlugError {
    /// The string is empty.
    #[error("a name can't be empty")]
    Empty,
    /// The string is longer than [`Slug::MAX_LEN`].
    #[error("a name can be at most {max} characters (got {len})", max = Slug::MAX_LEN)]
    TooLong {
        /// The length that was given.
        len: usize,
    },
    /// A character isn't allowed at that position.
    #[error("'{ch}' isn't allowed at position {at}: use a-z, 0-9, '-' or '_', starting with a letter or digit")]
    BadChar {
        /// The offending character.
        ch: char,
        /// Its index (in characters).
        at: usize,
    },
    /// The name is reserved by Windows for a device, so a file or folder with it can't exist there.
    #[error("'{0}' is reserved by Windows (a device name like con, aux or com1); pick another name")]
    Reserved(String),
}

/// Windows device names: a file or folder called one of these (with any extension) can't be created on Windows, and
/// slugs become file and folder names in projects (spec §7).
const WINDOWS_RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2",
    "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

impl Slug {
    /// The longest allowed slug, in characters.
    pub const MAX_LEN: usize = 64;

    /// Validates `s` as a slug.
    ///
    /// # Errors
    /// Returns a [`SlugError`] naming the first rule that `s` breaks.
    pub fn new(s: impl Into<String>) -> Result<Self, SlugError> {
        let s = s.into();
        let len = s.chars().count();
        if len == 0 {
            return Err(SlugError::Empty);
        }
        if len > Self::MAX_LEN {
            return Err(SlugError::TooLong { len });
        }
        for (at, ch) in s.chars().enumerate() {
            let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || (at > 0 && (ch == '-' || ch == '_'));
            if !ok {
                return Err(SlugError::BadChar { ch, at });
            }
        }
        // Slugs can't contain '.', so the whole slug is the stem Windows compares; `con_x` is fine, `con` isn't.
        if WINDOWS_RESERVED.contains(&s.as_str()) {
            return Err(SlugError::Reserved(s));
        }
        Ok(Self(s))
    }

    /// The slug as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Slug {
    type Error = SlugError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<Slug> for String {
    fn from(s: Slug) -> Self {
        s.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl JsonSchema for Slug {
    // Always inline: a slug is a one-line string rule, and agents read flat tool schemas more reliably than $refs.
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "Slug".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[a-z0-9][a-z0-9_-]{0,63}$",
            "description": "A name: 1-64 chars of a-z, 0-9, '-' or '_', starting with a letter or digit."
        })
    }
}

/// Declares a typed id backed by a [`Slug`], with serde, schema, `Display` and `FromStr`.
///
/// Each subsystem declares its own ids in its API crate, for example `slug_id!(/// A character. CharId);`
/// in `omuzaki-char-api`, so ids of different kinds can't be mixed up.
#[macro_export]
macro_rules! slug_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        #[derive($crate::__serde::Serialize, $crate::__serde::Deserialize, $crate::__schemars::JsonSchema)]
        #[serde(transparent, crate = "::omuzaki_core::__serde")]
        #[schemars(transparent, crate = "::omuzaki_core::__schemars")]
        pub struct $name(pub $crate::id::Slug);

        impl $name {
            /// Validates `s` and wraps it.
            ///
            /// # Errors
            /// Returns a [`SlugError`]($crate::id::SlugError) if `s` isn't a valid slug.
            pub fn new(s: impl Into<String>) -> Result<Self, $crate::id::SlugError> {
                $crate::id::Slug::new(s).map(Self)
            }

            /// The id as a string slice.
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::id::SlugError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_typical_names() {
        for s in ["mira", "close-up", "vrchat_quest", "a", "2d-flat"] {
            assert_eq!(Slug::new(s).unwrap().as_str(), s);
        }
    }

    #[test]
    fn rejects_bad_names_with_a_reason() {
        assert_eq!(Slug::new(""), Err(SlugError::Empty));
        assert_eq!(Slug::new("Mira"), Err(SlugError::BadChar { ch: 'M', at: 0 }));
        assert_eq!(Slug::new("-x"), Err(SlugError::BadChar { ch: '-', at: 0 }));
        assert_eq!(Slug::new("a b"), Err(SlugError::BadChar { ch: ' ', at: 1 }));
        assert_eq!(Slug::new("x".repeat(65)), Err(SlugError::TooLong { len: 65 }));
        assert!(Slug::new("x".repeat(64)).is_ok());
        assert_eq!(Slug::new("aux"), Err(SlugError::Reserved("aux".into())));
        assert_eq!(Slug::new("com1"), Err(SlugError::Reserved("com1".into())));
        assert!(Slug::new("console").is_ok() && Slug::new("con-1").is_ok() && Slug::new("com10").is_ok());
    }

    #[test]
    fn serde_round_trip_and_validation() {
        let s: Slug = serde_json::from_str("\"close-up\"").unwrap();
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"close-up\"");
        assert!(serde_json::from_str::<Slug>("\"Close Up\"").is_err());
    }

    crate::slug_id!(
        /// A test id.
        TestId
    );

    #[test]
    fn typed_ids_parse_display_and_serialize() {
        let id: TestId = "mira".parse().unwrap();
        assert_eq!((id.to_string().as_str(), id.as_str()), ("mira", "mira"));
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"mira\"");
        let schema = serde_json::to_value(schemars::schema_for!(TestId)).unwrap();
        assert_eq!(schema["type"], "string");
    }
}
