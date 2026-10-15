//! Steps, budgets, persisted progress and per-step timers.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::{Mutex, PoisonError};

/// One screen of the flow.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// The greeting.
    Welcome,
    /// Pick or keep a character.
    Character,
    /// Pick the tracking source.
    Input,
    /// Calibrate the face tracking.
    Calibrate,
    /// Add an output.
    Output,
    /// The Go-live check.
    GoLive,
}

impl Step {
    /// Every step, in flow order.
    pub const ALL: [Step; 6] =
        [Step::Welcome, Step::Character, Step::Input, Step::Calibrate, Step::Output, Step::GoLive];

    /// Time the screen budgets for this step (`shell.md` section 14): 5, 10, 20, 45, 20 and 10 seconds.
    pub const fn budget_s(self) -> u32 {
        match self {
            Step::Welcome => 5,
            Step::Character | Step::GoLive => 10,
            Step::Input | Step::Output => 20,
            Step::Calibrate => 45,
        }
    }

    /// The stable id stored in the session file; the settings API holds it as a string.
    pub const fn id(self) -> &'static str {
        match self {
            Step::Welcome => "welcome",
            Step::Character => "character",
            Step::Input => "input",
            Step::Calibrate => "calibrate",
            Step::Output => "output",
            Step::GoLive => "go_live",
        }
    }

    /// The step an [`id`](Self::id) names; `None` for an unknown id, which the caller resumes at Welcome.
    pub fn from_id(id: &str) -> Option<Step> {
        Step::ALL.into_iter().find(|s| s.id() == id)
    }

    /// Position in [`Step::ALL`].
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// The whole first run should fit in this many seconds; the budgets sum to less (110).
pub const TOTAL_BUDGET_S: u32 = 120;

/// What survives a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// The step to resume at.
    pub step: Step,
    /// Steps the person skipped or the probes made pointless, in flow order; Go-live lists them as "Later".
    pub skipped: Vec<Step>,
    /// Setup ended, finished or skipped.
    pub finished: bool,
}

impl Default for Progress {
    fn default() -> Self {
        Progress { step: Step::Welcome, skipped: Vec::new(), finished: false }
    }
}

/// A store could not read or write the progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreError(pub String);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "setup progress store: {}", self.0)
    }
}

impl std::error::Error for StoreError {}

/// Where [`Progress`] is kept; the settings-backed one lives with the ops.
pub trait SetupStore: Send + Sync {
    /// The saved progress, or the default (Welcome, nothing skipped) when none is saved.
    fn load(&self) -> Progress;
    /// Saves the progress, writing through.
    ///
    /// # Errors
    /// [`StoreError`] when the progress could not be written.
    fn save(&self, progress: &Progress) -> Result<(), StoreError>;
}

/// An in-memory [`SetupStore`] for tests.
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<Progress>);

impl SetupStore for MemoryStore {
    fn load(&self) -> Progress {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn save(&self, progress: &Progress) -> Result<(), StoreError> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = progress.clone();
        Ok(())
    }
}

/// Time per step in one sitting (not persisted): how long each step was current, and how much of that was spent
/// waiting on the OS camera dialog or a model download.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Timers {
    active_ms: [u64; 6],
    waited_ms: [u64; 6],
}

impl Timers {
    /// Milliseconds `step` was current.
    pub fn active_ms(&self, step: Step) -> u64 {
        self.active_ms[step.index()]
    }

    /// Milliseconds of `active_ms` spent waiting on the OS or a download.
    pub fn waited_ms(&self, step: Step) -> u64 {
        self.waited_ms[step.index()]
    }

    /// Active time per step, in [`Step::ALL`] order.
    pub fn active_by_step(&self) -> [u64; 6] {
        self.active_ms
    }

    pub(crate) fn add(&mut self, step: Step, delta_ms: u64, waiting: bool) {
        let i = step.index();
        self.active_ms[i] = self.active_ms[i].saturating_add(delta_ms);
        if waiting {
            self.waited_ms[i] = self.waited_ms[i].saturating_add(delta_ms);
        }
    }
}
