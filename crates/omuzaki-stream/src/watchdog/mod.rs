//! The watchdog: keeps a stream alive by restarting the process or thread that carries it (spec v3 §25, BR-23).
//!
//! [`policy`] decides (a pure state machine: observations and time in, actions out); [`supervisor`] runs it against
//! real workers started by a [`launch`]er, on a thread of its own.
pub mod launch;
pub mod policy;
pub mod supervisor;

pub use policy::{
    Action, Health, HealthEvent, HeartbeatCell, Monitor, Observation, RestartLog, RestartPolicy, RestartReason,
    RestartRecord, WorkerId, WorkerKind, WorkerSpec,
};
