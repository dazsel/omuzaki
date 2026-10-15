//! Drivers (design 6.3): whatever plays one attempt against a [`Host`]. The scripted driver replays a script; the
//! reference-model driver, which spends money, is a separate task and implements the same trait.

pub mod scripted;

use super::host::{Host, HostError};
use super::task::{Caps, Task};
use super::transcript::EventLog;

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// The driver had nothing more to do: the script ran out, or the model gave its final answer.
    Finished,
    /// A scripted `expect` or `assert` did not hold. The attempt ended there; the script is wrong, or the product broke.
    ScriptFailed {
        /// The script line, which is the step number a failure reports.
        line: usize,
        /// What did not hold.
        reason: String,
    },
    /// The tool-call cap was reached before the driver was done: the attempt fails.
    ToolCallCap,
    /// The wall-clock cap was reached: harness trouble, an `error` attempt, never the model's fail.
    WallClock,
}

/// What a driver leaves behind.
#[derive(Debug, Clone, PartialEq)]
pub struct Attempt {
    /// Everything that happened, with the server's answers.
    pub log: EventLog,
    /// How it ended.
    pub stop: Stop,
    /// Tool calls made.
    pub tool_calls: u64,
    /// Wall-clock milliseconds.
    pub wall_ms: u64,
}

/// Plays one attempt of a task on a host, within the caps.
pub trait Driver {
    /// Runs the attempt. The project the host serves holds the result when it returns.
    ///
    /// # Errors
    /// [`HostError`] when the host itself fails (the connection drops, the document cannot be read): harness trouble,
    /// not an outcome of the attempt.
    fn run(&self, task: &Task, host: &Host, caps: &Caps) -> Result<Attempt, HostError>;
}
