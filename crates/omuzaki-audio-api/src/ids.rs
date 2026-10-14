//! The typed ids of the audio contract.
//!
//! Every id except [`NodeKindId`] is a [`omuzaki_core::id::Slug`], so a bus can never be passed where a device role is
//! expected, and each is safe as a project-file key.

use std::borrow::Cow;
use std::fmt;

use omuzaki_core::id::{Slug, SlugError};
use omuzaki_core::slug_id;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

slug_id!(
    /// A bus in the mixer graph (`mic`, `ai_voice`, `program`, or any the user adds).
    BusId
);
slug_id!(
    /// A node inside a bus's chain. Unique within its bus: the engine moves a surviving node, with its state, by
    /// `(bus, node id)` when the graph is swapped.
    NodeId
);
slug_id!(
    /// A role a project names instead of a device (`voice`, `raw_voice`, `program_out`, `monitor_out`, or any user
    /// role). A machine profile binds roles to devices.
    DeviceRole
);
slug_id!(
    /// A feed: audio written by a producer thread (TTS, a music decoder) into a bus.
    FeedId
);
slug_id!(
    /// A player: a source whose decoded samples are shared buffers made off-thread (the soundboard, music).
    PlayerId
);
slug_id!(
    /// A ducking gate a source raises (the soundboard raises one for each sound marked "ducks").
    GateId
);

/// The id of a node kind: dot-separated slugs such as `fx.eq` or `model.stream`.
///
/// Kinds are namespaced (`fx.` for effects, `model.` for model nodes), which a plain [`Slug`] cannot express because
/// slugs have no `.`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NodeKindId(String);

impl NodeKindId {
    /// Validates `s`: one or more slugs separated by single dots.
    ///
    /// # Errors
    /// Returns the [`SlugError`] of the first segment that is not a valid slug (an empty segment is
    /// [`SlugError::Empty`]).
    pub fn new(s: impl Into<String>) -> Result<Self, SlugError> {
        let s = s.into();
        for segment in s.split('.') {
            Slug::new(segment)?;
        }
        Ok(Self(s))
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for NodeKindId {
    type Error = SlugError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<NodeKindId> for String {
    fn from(id: NodeKindId) -> Self {
        id.0
    }
}

impl fmt::Display for NodeKindId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for NodeKindId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl std::str::FromStr for NodeKindId {
    type Err = SlugError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl JsonSchema for NodeKindId {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "NodeKindId".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[a-z0-9][a-z0-9_-]{0,63}(\\.[a-z0-9][a-z0-9_-]{0,63})*$",
            "description": "A node kind: dot-separated names such as fx.eq or model.stream."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_ids_are_dotted_slugs() {
        for ok in ["fx.eq", "model.stream", "fx.aec", "gain"] {
            assert_eq!(NodeKindId::new(ok).map(|k| k.to_string()).as_deref(), Ok(ok));
        }
        for bad in ["", ".", "fx.", ".eq", "fx..eq", "Fx.eq", "fx.e q"] {
            assert!(NodeKindId::new(bad).is_err(), "{bad:?}");
        }
        assert!(serde_json::from_str::<NodeKindId>("\"fx..eq\"").is_err());
    }
}
