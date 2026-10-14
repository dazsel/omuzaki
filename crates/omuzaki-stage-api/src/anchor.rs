//! Anchors: what a light or a free camera is measured against (ADR 0024 D3, design D4).

use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::error::SceneError;
use crate::ids::MemberId;

/// What a length is measured against. Written as a string in the files: `"stage"`, `"subject"` or `"member:<id>"`.
///
/// With `Subject` or `Member`, lengths are in that member's units (as if it were 1 m tall, in its frame) and point-light
/// intensities scale with its height squared; with `Stage` they are metres in fixed axes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Anchor {
    /// The stage origin, axes fixed, lengths in metres.
    Stage,
    /// The scene's subject member.
    #[default]
    Subject,
    /// A named member of the cast.
    Member(MemberId),
}

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stage => f.write_str("stage"),
            Self::Subject => f.write_str("subject"),
            Self::Member(id) => write!(f, "member:{id}"),
        }
    }
}

impl FromStr for Anchor {
    type Err = SceneError;

    fn from_str(s: &str) -> Result<Self, SceneError> {
        match s {
            "stage" => Ok(Self::Stage),
            "subject" => Ok(Self::Subject),
            _ => match s.strip_prefix("member:") {
                Some(id) => MemberId::checked(id).map(Self::Member),
                None => Err(SceneError::new("anchor", format!("`{s}` isn't `stage`, `subject` or `member:<id>`"))),
            },
        }
    }
}

impl Serialize for Anchor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Anchor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Anchor {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Anchor".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "`stage`, `subject` or `member:<id>`",
            "pattern": "^(stage|subject|member:[a-z0-9_-]+)$"
        })
    }
}

impl Anchor {
    /// The member this anchor names, if it names one.
    #[must_use]
    pub fn member(&self) -> Option<&MemberId> {
        match self {
            Self::Member(id) => Some(id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_read_and_write_as_strings() {
        for text in ["stage", "subject", "member:kiri"] {
            let anchor: Anchor = text.parse().unwrap();
            assert_eq!(anchor.to_string(), text);
            assert_eq!(serde_json::to_value(&anchor).unwrap(), serde_json::json!(text));
        }
        assert_eq!(Anchor::default(), Anchor::Subject);
    }

    #[test]
    fn a_bad_anchor_is_refused_with_the_rule() {
        for bad in ["", "Stage", "member:", "member:subject", "member:stage", "members:kiri", "kiri"] {
            assert!(bad.parse::<Anchor>().is_err(), "{bad:?}");
        }
        assert!(serde_json::from_str::<Anchor>("\"nope\"").is_err());
    }
}
