//! Holds: how a live subsystem tells the updater not to restart under it (design section 7).

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What is in progress.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    /// A live stream is on.
    Streaming,
    /// A recording is running.
    Recording,
    /// A render is running.
    Rendering,
    /// A capture is running.
    Capturing,
    /// A project is being saved.
    ProjectSaving,
}

/// An RAII token: the hold lasts until it is dropped. `mem::forget` keeps it for the life of the process.
#[must_use = "dropping a Hold releases it at once"]
pub struct Hold {
    release: Option<Box<dyn FnOnce() + Send>>,
}

impl Hold {
    /// A hold that runs `release` once, when dropped. The updater builds these.
    pub fn new(release: impl FnOnce() + Send + 'static) -> Self {
        Self { release: Some(Box::new(release)) }
    }

    /// A hold that holds nothing, for builds without an updater.
    pub fn none() -> Self {
        Self { release: None }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for Hold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hold").field("active", &self.release.is_some()).finish()
    }
}
