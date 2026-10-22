//! Ids (design D2, D4): the slugs of the document's things, the dotted ids of node kinds and event kinds, port names,
//! and the `<node>.<port>` reference a wire is written as.
//!
//! Slugs never contain `.`, which is what lets a wire be written `line.text` and a kind `flow.wait` without quoting.

use std::borrow::{Borrow, Cow};
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::limits::{MAX_EVENT_KIND_LEN, MAX_NODE_KIND_ID_LEN, MAX_PORT_NAME_LEN};

omuzaki_core::slug_id!(
    /// A show: `shows/<id>/`.
    ShowId
);
omuzaki_core::slug_id!(
    /// A graph of a show: `shows/<show>/automations/<id>/`.
    GraphId
);
omuzaki_core::slug_id!(
    /// A node of a graph: `.../nodes/<id>.toml`. Unique within its graph.
    NodeId
);
omuzaki_core::slug_id!(
    /// A group of graphs, declared in `show.toml` (design D12).
    GroupId
);
omuzaki_core::slug_id!(
    /// A variable, declared in `show.toml` (design D10).
    VarName
);
omuzaki_core::slug_id!(
    /// A streaming platform: `twitch`, `youtube`, `bilibili`, `tiktok`, or a plugin adapter's id (`kick`).
    Platform
);

/// Why a string isn't a valid [`NodeKindId`], [`EventKind`], [`PortName`] or [`PortRef`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{value:?} isn't a valid {what}: {reason}")]
pub struct IdError {
    /// What kind of id was expected (`node kind id`, `port name`, ...).
    pub what: &'static str,
    /// The string that was given.
    pub value: String,
    /// The rule it breaks.
    pub reason: &'static str,
}

/// Checks one `[a-z][a-z0-9_]*` segment of a dotted id, from `start` up to (not including) `end`.
const fn segment_ok(bytes: &[u8], start: usize, end: usize) -> bool {
    if start >= end || !bytes[start].is_ascii_lowercase() {
        return false;
    }
    let mut i = start + 1;
    while i < end {
        let b = bytes[i];
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            return false;
        }
        i += 1;
    }
    true
}

/// Checks a dotted id: segments of `[a-z][a-z0-9_]*` joined by `.`, at least `min_segments` of them, at most `max_len`
/// bytes. `const` so a registration's id is checked when it is compiled.
const fn check_dotted(s: &str, min_segments: usize, max_len: usize) -> Result<(), &'static str> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Err("it is empty");
    }
    if bytes.len() > max_len {
        return Err("it is too long");
    }
    let (mut start, mut segments, mut i) = (0, 0, 0);
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'.' {
            if !segment_ok(bytes, start, i) {
                return Err("each part between dots starts with a-z and holds only a-z, 0-9 and '_'");
            }
            segments += 1;
            start = i + 1;
        }
        i += 1;
    }
    if segments < min_segments {
        return Err("it needs a family and a name, like `flow.wait`");
    }
    Ok(())
}

/// Checks a node kind id: two or more segments.
const fn check_node_kind(s: &str) -> Result<(), &'static str> {
    check_dotted(s, 2, MAX_NODE_KIND_ID_LEN)
}

/// Checks an event kind: one or more segments.
const fn check_event_kind(s: &str) -> Result<(), &'static str> {
    check_dotted(s, 1, MAX_EVENT_KIND_LEN)
}

/// Checks a port name: `[a-z][a-z0-9_]*`, at most [`MAX_PORT_NAME_LEN`] bytes.
const fn check_port(s: &str) -> Result<(), &'static str> {
    let bytes = s.as_bytes();
    if bytes.len() > MAX_PORT_NAME_LEN {
        return Err("it is too long");
    }
    if segment_ok(bytes, 0, bytes.len()) { Ok(()) } else { Err("it starts with a-z and holds only a-z, 0-9 and '_'") }
}

/// Declares a string id checked by `$check`, with `from_static`, `FromStr`, serde as the string and a schema pattern.
macro_rules! checked_id {
    ($(#[$meta:meta])* $name:ident, $what:literal, $check:expr, $pattern:literal, $describe:literal) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Cow<'static, str>);

        impl $name {
            /// Validates `s` and wraps it.
            ///
            /// # Errors
            /// An [`IdError`] naming the rule `s` breaks.
            pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
                let s = s.into();
                match $check(&s) {
                    Ok(()) => Ok(Self(Cow::Owned(s))),
                    Err(reason) => Err(IdError { what: $what, value: s, reason }),
                }
            }

            /// Wraps a static string, for registrations: in a `const` or `static`, an invalid id is a compile error.
            ///
            /// # Panics
            /// If `s` isn't valid. Use it only on literals, in const context, where the panic is a compile error.
            pub const fn from_static(s: &'static str) -> Self {
                match $check(s) {
                    Ok(()) => Self(Cow::Borrowed(s)),
                    Err(_) => panic!(concat!("invalid ", $what, " literal")),
                }
            }

            /// The id as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = IdError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{:?}", self.0)
            }
        }

        // Ord and Hash are those of the string, so maps keyed by the id can be looked up with a `&str`.
        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                Self::new(s).map_err(serde::de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            // Inline, like slugs: a one-line string rule reads better than a `$ref` in a tool schema.
            fn inline_schema() -> bool {
                true
            }

            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "pattern": $pattern, "description": $describe })
            }
        }
    };
}

checked_id!(
    /// A node kind: `family.name` (`flow.wait`, `cond.time_between`) or `plugin.<plugin>.<name>`, at most
    /// [`MAX_NODE_KIND_ID_LEN`] bytes. Its message ids replace `.` with `-` (`node-flow-wait`).
    NodeKindId,
    "node kind id",
    check_node_kind,
    "^[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)+$",
    "A node kind: `family.name`, like `flow.wait`."
);

checked_id!(
    /// An event kind: `chat.message`, `redeem`, `custom.<name>`, `plugin.<plugin>.<name>`; at most
    /// [`MAX_EVENT_KIND_LEN`] bytes.
    EventKind,
    "event kind",
    check_event_kind,
    "^[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)*$",
    "An event kind, like `chat.message` or `custom.bonk`."
);

checked_id!(
    /// A port of a node: `[a-z][a-z0-9_]*`, at most [`MAX_PORT_NAME_LEN`] bytes. Inputs and outputs are separate
    /// namespaces.
    PortName,
    "port name",
    check_port,
    "^[a-z][a-z0-9_]{0,31}$",
    "A port: a-z first, then a-z, 0-9 or '_'."
);

impl NodeKindId {
    /// The id inside message ids: `.` becomes `-` (`flow.wait` is `node-flow-wait`, design D4).
    pub fn message_stem(&self) -> String {
        self.0.replace('.', "-")
    }
}

impl EventKind {
    /// The prefix of the kinds `event.emit` and `automation.fire` may use without registering (design D9).
    pub const CUSTOM_PREFIX: &'static str = "custom.";

    /// True for `custom.<name>`: any object payload, no registration.
    pub fn is_custom(&self) -> bool {
        self.0.starts_with(Self::CUSTOM_PREFIX)
    }
}

/// An output of a node, as a wire names it: `<node>.<port>` (`line.text`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PortRef {
    /// The node whose output it is.
    pub node: NodeId,
    /// The output port.
    pub port: PortName,
}

impl PortRef {
    /// A reference to `node`'s output `port`.
    pub fn new(node: NodeId, port: PortName) -> Self {
        Self { node, port }
    }
}

impl FromStr for PortRef {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = |reason| IdError { what: "wire source", value: s.to_owned(), reason };
        let (node, port) = s.split_once('.').ok_or_else(|| bad("it is `<node>.<port>`, like `line.text`"))?;
        let node = NodeId::new(node).map_err(|_| bad("the node before the dot isn't a valid id"))?;
        let port = PortName::new(port).map_err(|_| bad("the port after the dot isn't a valid port name"))?;
        Ok(Self { node, port })
    }
}

impl fmt::Display for PortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.node, self.port)
    }
}

impl Serialize for PortRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PortRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for PortRef {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "PortRef".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[a-z0-9][a-z0-9_-]{0,63}\\.[a-z][a-z0-9_]{0,31}$",
            "description": "An output of a node: `<node>.<port>`, like `line.text`."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_ids_follow_their_patterns() {
        for ok in ["flow.wait", "cond.time_between", "plugin.kick.chat", "a.b2_c"] {
            assert!(NodeKindId::new(ok).is_ok(), "{ok}");
        }
        for bad in ["flow", "Flow.wait", "flow..wait", ".flow", "flow.", "flow.2x", "flow.wa-it", ""] {
            assert!(NodeKindId::new(bad).is_err(), "{bad}");
        }
        assert!(NodeKindId::new(format!("a.{}", "b".repeat(94))).is_ok());
        assert!(NodeKindId::new(format!("a.{}", "b".repeat(95))).is_err());
        for ok in ["redeem", "chat.message", "custom.bonk", "plugin.kick.follow"] {
            assert!(EventKind::new(ok).is_ok(), "{ok}");
        }
        assert!(EventKind::new("Redeem").is_err() && EventKind::new("chat.").is_err());
        assert!(EventKind::from_static("custom.x").is_custom() && !EventKind::from_static("redeem").is_custom());
    }

    #[test]
    fn port_names_are_short_lowercase_words() {
        assert!(PortName::new("in").is_ok() && PortName::new("user_name").is_ok());
        assert!(PortName::new("x".repeat(32)).is_ok() && PortName::new("x".repeat(33)).is_err());
        for bad in ["", "In", "1x", "a.b", "a-b", "_x"] {
            assert!(PortName::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_port_ref_reads_and_writes_node_dot_port() {
        let r: PortRef = "line.text".parse().unwrap();
        assert_eq!((r.node.as_str(), r.port.as_str()), ("line", "text"));
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"line.text\"");
        assert_eq!(serde_json::from_str::<PortRef>("\"cool.out\"").unwrap().to_string(), "cool.out");
        for bad in ["line", "line.", ".text", "Line.text", "line.te.xt", "line.Text"] {
            assert!(bad.parse::<PortRef>().is_err(), "{bad}");
        }
    }

    #[test]
    fn the_message_stem_replaces_dots() {
        assert_eq!(NodeKindId::from_static("flow.wait").message_stem(), "flow-wait");
    }
}
