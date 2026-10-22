//! [`ScriptedEars`]: timed `Heard` events on a fake clock.

use std::collections::VecDeque;
use std::sync::Arc;

use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_performer_api::ears::{Ears, Heard};

/// [`Ears`] that release each scripted event once the clock reaches its time. Events come out in script order;
/// `Muted`, which carries no time, is released at the time it is scripted at.
pub struct ScriptedEars {
    clock: Arc<FakeClock>,
    script: VecDeque<(Seconds, Heard)>,
}

impl ScriptedEars {
    /// Ears that play `script`, `(time, event)` in order, on `clock`.
    pub fn new(clock: Arc<FakeClock>, script: Vec<(Seconds, Heard)>) -> Self {
        Self { clock, script: script.into() }
    }

    /// The events not released yet.
    pub fn pending(&self) -> usize {
        self.script.len()
    }
}

impl Ears for ScriptedEars {
    fn next(&mut self) -> Option<Heard> {
        let now = self.clock.now();
        if self.script.front().is_some_and(|(at, _)| at.0 <= now.0) {
            self.script.pop_front().map(|(_, heard)| heard)
        } else {
            None
        }
    }
}
