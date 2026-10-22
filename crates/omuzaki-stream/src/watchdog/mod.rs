//! The watchdog: keeps a stream alive by restarting the process or thread that carries it (spec v3 §25, BR-23;
//! `docs/design/supervisor.md`).
//!
//! [`policy`] decides (a pure state machine: observations and time in, actions out); [`supervisor`] runs it against
//! real worker generations started by a [`launch`]er, on a thread of its own; [`worker`] is the worker's side of the
//! protocol, for the worker binaries.
pub mod launch;
pub mod policy;
pub mod supervisor;
pub mod worker;

pub use policy::{
    Action, Health, HealthEvent, HeartbeatCell, Monitor, Observation, RESTART_LOG_LEN, RestartLog, RestartPolicy,
    RestartReason, RestartRecord, Scope, UnknownWorker, WorkerId, WorkerKind, WorkerSpec, WorkerStatus,
};
pub use supervisor::{Handoff, NoHandoff, SendError, StopReport, Supervisor, SupervisorConfig};
