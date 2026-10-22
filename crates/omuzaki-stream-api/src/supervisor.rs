//! What everyone reads about the live show's supervised workers (`docs/design/supervisor.md` §15.1): their ids,
//! kinds, health, restart reasons, health events, the restart log's records and the health meter's rows.
//!
//! Every [`Seconds`] here is on the **show clock**: seconds since the show went live, read from the OS monotonic clock
//! that every process of the show reads alike, so a time means the same instant in the output host and in the studio.

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which supervisor runs a worker: the output host's (processes) or the studio's (adapter tasks).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Scope {
    /// The output host's supervisor: the stage, tracker, audio and encoder workers.
    Host,
    /// The studio's supervisor: the platform adapters.
    Studio,
}

/// A supervised worker: its supervisor, that supervisor's instance and its index in that supervisor's spec list.
///
/// Unique across the show (the host's worker 0 is never the studio's) and across monitors (an id kept from a monitor
/// that has since been rebuilt never names a worker of the new one). An id is made by a monitor; one that arrives from
/// outside (deserialized from the local socket or an op) is checked by every method that takes it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct WorkerId {
    scope: Scope,
    instance: u64,
    index: u16,
}

impl WorkerId {
    /// The id of worker `index` of the monitor `instance` in `scope`. Only a monitor makes ids; anything else gets
    /// them from it.
    pub fn new(scope: Scope, instance: u64, index: u16) -> Self {
        Self { scope, instance, index }
    }

    /// Its supervisor.
    pub fn scope(self) -> Scope {
        self.scope
    }

    /// Its monitor's nonce: 64 random bits, never 0.
    pub fn instance(self) -> u64 {
        self.instance
    }

    /// Its index in its monitor's spec list.
    pub fn index(self) -> u16 {
        self.index
    }
}

/// An id that no worker of this supervisor has.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("no worker {id:?} in this supervisor")]
pub struct UnknownWorker {
    /// The id asked for.
    pub id: WorkerId,
}

/// What a worker does.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WorkerKind {
    /// The stage worker: the stage and the renderer.
    Renderer,
    /// The face tracker.
    Tracker,
    /// The audio engine.
    Audio,
    /// An encoder.
    Encoder,
    /// A platform adapter (a studio task).
    Adapter,
}

/// A worker's health.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "state")]
#[non_exhaustive]
pub enum Health {
    /// Spawned (or about to be) and not making progress yet.
    Starting,
    /// Making progress.
    Healthy,
    /// Being replaced: the old instance is ending or the new one has not made progress yet.
    #[non_exhaustive]
    Restarting {
        /// The attempt within the window, from 1.
        attempt: u32,
    },
    /// Waiting before the next spawn.
    #[non_exhaustive]
    Backoff {
        /// The attempt within the window, from 1.
        attempt: u32,
        /// When it spawns, on the show clock.
        until: Seconds,
    },
    /// Out of restarts: it stays down until a person restarts it.
    #[non_exhaustive]
    Failed {
        /// The restarts made in the window.
        restarts: u32,
    },
}

impl Health {
    /// `Restarting { attempt }`.
    pub fn restarting(attempt: u32) -> Self {
        Self::Restarting { attempt }
    }

    /// `Backoff { attempt, until }`.
    pub fn backoff(attempt: u32, until: Seconds) -> Self {
        Self::Backoff { attempt, until }
    }

    /// `Failed { restarts }`.
    pub fn failed(restarts: u32) -> Self {
        Self::Failed { restarts }
    }
}

/// Why a worker was restarted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "reason")]
#[non_exhaustive]
pub enum RestartReason {
    /// It ended on its own.
    #[non_exhaustive]
    Crashed {
        /// Its exit code; `None` when a signal killed it.
        exit_code: Option<i32>,
    },
    /// It stopped making progress.
    #[non_exhaustive]
    Hung {
        /// How long it had been silent.
        silent_for: Seconds,
    },
    /// A person or an agent asked (`supervisor.restart`).
    Requested,
    /// It sent a malformed control frame: killed and restarted.
    #[non_exhaustive]
    Protocol {
        /// What was wrong.
        what: String,
    },
    /// The spawn or the handshake failed.
    #[non_exhaustive]
    LaunchFailed {
        /// What failed.
        what: String,
    },
}

impl RestartReason {
    /// `Crashed { exit_code }`.
    pub fn crashed(exit_code: Option<i32>) -> Self {
        Self::Crashed { exit_code }
    }

    /// `Hung { silent_for }`.
    pub fn hung(silent_for: Seconds) -> Self {
        Self::Hung { silent_for }
    }

    /// `Protocol { what }`.
    pub fn protocol(what: impl Into<String>) -> Self {
        Self::Protocol { what: what.into() }
    }

    /// `LaunchFailed { what }`.
    pub fn launch_failed(what: impl Into<String>) -> Self {
        Self::LaunchFailed { what: what.into() }
    }
}

/// A change of health worth telling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "event")]
#[non_exhaustive]
pub enum HealthEvent {
    /// A worker is being restarted.
    #[non_exhaustive]
    Restarted {
        /// Which.
        worker: WorkerId,
        /// Why.
        reason: RestartReason,
        /// The attempt within the window, from 1.
        attempt: u32,
    },
    /// A restarted worker makes progress again.
    #[non_exhaustive]
    Recovered {
        /// Which.
        worker: WorkerId,
        /// From the first failure to the first progress after it.
        downtime: Seconds,
        /// Past the worker's `slow_after`.
        slow: bool,
    },
    /// A worker ran out of restarts.
    #[non_exhaustive]
    GaveUp {
        /// Which.
        worker: WorkerId,
        /// The restarts made in the window.
        restarts: u32,
    },
    /// A process kill that has not finished after `exit_wait`: it cannot be replaced until it ends.
    #[non_exhaustive]
    Stuck {
        /// Which.
        worker: WorkerId,
    },
    /// A hung thread or task given up after `exit_wait` and replaced.
    #[non_exhaustive]
    Abandoned {
        /// Which.
        worker: WorkerId,
    },
}

impl HealthEvent {
    /// `Restarted { worker, reason, attempt }`.
    pub fn restarted(worker: WorkerId, reason: RestartReason, attempt: u32) -> Self {
        Self::Restarted { worker, reason, attempt }
    }

    /// `Recovered { worker, downtime, slow }`.
    pub fn recovered(worker: WorkerId, downtime: Seconds, slow: bool) -> Self {
        Self::Recovered { worker, downtime, slow }
    }

    /// `GaveUp { worker, restarts }`.
    pub fn gave_up(worker: WorkerId, restarts: u32) -> Self {
        Self::GaveUp { worker, restarts }
    }

    /// `Stuck { worker }`.
    pub fn stuck(worker: WorkerId) -> Self {
        Self::Stuck { worker }
    }

    /// `Abandoned { worker }`.
    pub fn abandoned(worker: WorkerId) -> Self {
        Self::Abandoned { worker }
    }

    /// The worker it is about.
    pub fn worker(&self) -> WorkerId {
        match self {
            Self::Restarted { worker, .. }
            | Self::Recovered { worker, .. }
            | Self::GaveUp { worker, .. }
            | Self::Stuck { worker }
            | Self::Abandoned { worker } => *worker,
        }
    }
}

/// One restart, for the log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RestartRecord {
    /// When it failed, on the show clock.
    pub at: Seconds,
    /// Which worker.
    pub worker: WorkerId,
    /// Why.
    pub reason: RestartReason,
    /// The attempt within the window, from 1.
    pub attempt: u32,
    /// From the failure to the first progress after it, once it recovered.
    pub downtime: Option<Seconds>,
}

impl RestartRecord {
    /// A record of a restart not yet recovered from.
    pub fn new(at: Seconds, worker: WorkerId, reason: RestartReason, attempt: u32) -> Self {
        Self { at, worker, reason, attempt, downtime: None }
    }
}

/// One row of the health meter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct WorkerStatus {
    /// Which worker.
    pub id: WorkerId,
    /// Its name, for people.
    pub name: String,
    /// What it does.
    pub kind: WorkerKind,
    /// Its health.
    pub health: Health,
    /// The restarts counted in its current window.
    pub restarts_in_window: u32,
    /// Its newest restart, if any.
    pub last: Option<RestartRecord>,
    /// Its process id, while a process instance runs.
    pub pid: Option<u32>,
    /// The kind's work counter: frames, packets or blocks.
    pub work: u64,
}

impl WorkerStatus {
    /// A row with no restart, no process and no work yet.
    pub fn new(id: WorkerId, name: impl Into<String>, kind: WorkerKind, health: Health) -> Self {
        Self { id, name: name.into(), kind, health, restarts_in_window: 0, last: None, pid: None, work: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_name_their_scope_instance_and_index() {
        let id = WorkerId::new(Scope::Studio, 7, 3);
        assert_eq!((id.scope(), id.instance(), id.index()), (Scope::Studio, 7, 3));
        assert_ne!(id, WorkerId::new(Scope::Host, 7, 3));
        assert_ne!(id, WorkerId::new(Scope::Studio, 8, 3));
    }

    #[test]
    fn health_and_events_serialize_with_a_tag() -> Result<(), serde_json::Error> {
        let id = WorkerId::new(Scope::Host, 1, 0);
        let text = serde_json::to_string(&Health::backoff(2, Seconds(1.5)))?;
        assert_eq!(text, r#"{"state":"backoff","attempt":2,"until":1.5}"#);
        let event = HealthEvent::restarted(id, RestartReason::crashed(None), 1);
        let back: HealthEvent = serde_json::from_str(&serde_json::to_string(&event)?)?;
        assert_eq!(back, event);
        assert_eq!(back.worker(), id);
        Ok(())
    }
}
