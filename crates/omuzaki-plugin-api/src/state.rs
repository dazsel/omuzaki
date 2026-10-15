//! A plugin's identity and where it is in its life.

use std::fmt;

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{Scope, SessionId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The longest plugin id, in characters.
pub const MAX_ID_LEN: usize = 32;

/// The id of an installed plugin: the lower-case slug of the manifest (`^[a-z][a-z0-9_]{1,31}$`). Reserved words are
/// the installer's check, because they depend on the op registry.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct PluginId(String);

impl PluginId {
    /// Checks `slug` against the id rules.
    ///
    /// # Errors
    /// [`PluginIdError`] when `slug` breaks them.
    pub fn new(slug: impl Into<String>) -> Result<Self, PluginIdError> {
        let slug = slug.into();
        let mut chars = slug.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
        let rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if first_ok && rest_ok && (2..=MAX_ID_LEN).contains(&slug.len()) {
            Ok(Self(slug))
        } else {
            Err(PluginIdError { given: slug })
        }
    }

    /// The slug.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for PluginId {
    type Error = PluginIdError;

    fn try_from(slug: String) -> Result<Self, Self::Error> {
        Self::new(slug)
    }
}

impl From<PluginId> for String {
    fn from(id: PluginId) -> Self {
        id.0
    }
}

/// A string that is not a plugin id.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "`{given}` is not a plugin id: use 2 to {MAX_ID_LEN} characters, a lower-case letter first, then lower-case letters, digits or `_`"
)]
pub struct PluginIdError {
    /// What was given.
    pub given: String,
}

/// Why a plugin stopped. Names the trap, not the entry; the host's log says which entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FaultKind {
    /// A trap: out of bounds, `unreachable`, stack overflow or a guest panic.
    Trap,
    /// The call used up its fuel.
    OutOfFuel,
    /// The instance hit its memory or table cap.
    OutOfMemory,
    /// The call passed its deadline.
    Timeout,
    /// More host calls a second than the plugin's bucket allows.
    HostCallFlood,
    /// The guest returned a value the host refuses (invalid UTF-8, over the size cap).
    BadReturn,
    /// `init` returned an error; the fault's detail is its text.
    InitError,
    /// The guest called `wasi:cli/exit`.
    Exited,
}

/// Where a plugin is in its life (design §6.2). Nothing loads at launch that the person has not enabled.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PluginState {
    /// On disk and off: the component is not compiled.
    Installed,
    /// Off, and kept off until the person turns it on again.
    #[non_exhaustive]
    Disabled {
        /// Why.
        reason: DisabledReason,
    },
    /// Being compiled and started.
    Loading,
    /// Running, with an open session.
    Running,
    /// Stopped by a fault; the host restarts it after its backoff ([`PluginInfo::restart_at`]).
    #[non_exhaustive]
    Faulted {
        /// What went wrong.
        fault: FaultKind,
    },
    /// Being stopped: its session has ended and `shutdown` is running.
    Unloading,
}

impl PluginState {
    /// A plugin stopped by `fault`.
    pub fn faulted(fault: FaultKind) -> Self {
        Self::Faulted { fault }
    }

    /// A plugin kept off for `reason`.
    pub fn disabled(reason: DisabledReason) -> Self {
        Self::Disabled { reason }
    }
}

/// Why a plugin is kept off.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "why", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DisabledReason {
    /// The person turned it off.
    Person,
    /// It faulted [`FAULTS_TO_DISABLE`](crate::limits::FAULTS_TO_DISABLE) times within
    /// [`FAULT_WINDOW_SECONDS`](crate::limits::FAULT_WINDOW_SECONDS); only the person turns it back on.
    Faults,
    /// Its WIT version is outside this host's window: it needs an update.
    #[non_exhaustive]
    Incompatible {
        /// The version its imports name (`"0.1"`).
        built_for: String,
        /// The versions this host serves, newest first.
        host: Vec<String>,
    },
    /// The person revoked the key that signed it.
    PublisherRevoked,
}

impl DisabledReason {
    /// A plugin built for `built_for`, on a host that serves `host`.
    pub fn incompatible(built_for: impl Into<String>, host: Vec<String>) -> Self {
        Self::Incompatible { built_for: built_for.into(), host }
    }
}

/// One fault, for the manager's list of the last three.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FaultRecord {
    /// What went wrong.
    pub fault: FaultKind,
    /// The trap kind and the entry, one plain sentence: "out of fuel in init".
    pub detail: String,
    /// When, on the runner's clock.
    pub at: Seconds,
}

/// How loud a log line is; the WIT `level`, one to one.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Tracing detail.
    Trace,
    /// Debugging detail; stdout and stderr land here.
    Debug,
    /// Normal operation.
    Info,
    /// Something the person may want to know; every refused call adds one.
    Warn,
    /// A failure.
    Error,
}

/// One line of a plugin's log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LogLine {
    /// Rises by one per line kept, and never repeats, so a reader pages with `after`.
    pub seq: u64,
    /// When, on the runner's clock.
    pub at: Seconds,
    /// How loud.
    pub level: LogLevel,
    /// The text, at most [`LOG_LINE_MAX_BYTES`](crate::limits::LOG_LINE_MAX_BYTES).
    pub text: String,
    /// Written during a dry run (`plan`), so not an effect.
    pub dry_run: bool,
}

/// What the manager shows about one plugin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginInfo {
    /// The id.
    pub id: PluginId,
    /// The display name from the manifest.
    pub name: String,
    /// The plugin's own version (semver text).
    pub version: String,
    /// Where it is in its life.
    pub state: PluginState,
    /// The scopes its manifest asks for.
    pub requested: Vec<Scope>,
    /// The scopes the person approved, never beyond `requested`.
    pub approved: Vec<Scope>,
    /// Its open session while it runs; none otherwise.
    pub session: Option<SessionId>,
    /// Its last faults, oldest first, at most [`FAULTS_TO_DISABLE`](crate::limits::FAULTS_TO_DISABLE).
    #[serde(default)]
    pub faults: Vec<FaultRecord>,
    /// When a faulted plugin restarts, on the runner's clock.
    #[serde(default)]
    pub restart_at: Option<Seconds>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_the_slug_rule() {
        for ok in ["ab", "example", "a_1", &"a".repeat(MAX_ID_LEN)] {
            assert_eq!(PluginId::new(ok).unwrap().as_str(), ok);
        }
        for bad in ["", "a", "1a", "Ab", "a-b", "a b", "é1", &"a".repeat(MAX_ID_LEN + 1)] {
            assert_eq!(PluginId::new(bad).unwrap_err().given, bad);
        }
    }

    #[test]
    fn an_id_round_trips_as_a_string_and_refuses_a_bad_one() {
        let id = PluginId::new("example").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"example\"");
        assert_eq!(serde_json::from_str::<PluginId>("\"example\"").unwrap(), id);
        assert!(serde_json::from_str::<PluginId>("\"Bad Id\"").is_err());
        assert_eq!(serde_json::to_value(schemars::schema_for!(PluginId)).unwrap()["type"], "string");
    }

    #[test]
    fn states_and_faults_round_trip() {
        let faults = [
            FaultKind::Trap,
            FaultKind::OutOfFuel,
            FaultKind::OutOfMemory,
            FaultKind::Timeout,
            FaultKind::HostCallFlood,
            FaultKind::BadReturn,
            FaultKind::InitError,
            FaultKind::Exited,
        ];
        let mut states = vec![
            PluginState::Installed,
            PluginState::disabled(DisabledReason::Person),
            PluginState::disabled(DisabledReason::Faults),
            PluginState::disabled(DisabledReason::incompatible("0.1", vec!["0.3".into(), "0.2".into()])),
            PluginState::disabled(DisabledReason::PublisherRevoked),
            PluginState::Loading,
            PluginState::Running,
            PluginState::Unloading,
        ];
        states.extend(faults.map(PluginState::faulted));
        for fault in faults {
            let json = serde_json::to_string(&fault).unwrap();
            assert_eq!(serde_json::from_str::<FaultKind>(&json).unwrap(), fault);
        }
        for state in states {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(serde_json::from_str::<PluginState>(&json).unwrap(), state);
        }
        let json = serde_json::to_value(PluginState::faulted(FaultKind::OutOfFuel)).unwrap();
        assert_eq!(json, serde_json::json!({ "kind": "faulted", "fault": "out_of_fuel" }));
        let json = serde_json::to_value(PluginState::disabled(DisabledReason::Faults)).unwrap();
        assert_eq!(json, serde_json::json!({ "kind": "disabled", "reason": { "why": "faults" } }));
    }

    #[test]
    fn info_round_trips_and_has_a_schema() {
        let info = PluginInfo {
            id: PluginId::new("example").unwrap(),
            name: "Example".into(),
            version: "0.1.0".into(),
            state: PluginState::Running,
            requested: vec![Scope::Read, Scope::Perform],
            approved: vec![Scope::Read],
            session: Some(SessionId(7)),
            faults: vec![FaultRecord { fault: FaultKind::Trap, detail: "trap in init".into(), at: Seconds(2.0) }],
            restart_at: Some(Seconds(3.0)),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(serde_json::from_str::<PluginInfo>(&json).unwrap(), info);
        let schema = serde_json::to_value(schemars::schema_for!(PluginInfo)).unwrap();
        assert!(schema["properties"]["state"].is_object() || schema["properties"]["state"]["$ref"].is_string());
    }
}
