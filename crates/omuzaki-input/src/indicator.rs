//! The installed indicators (design §4). A surface installs one with the hub's [`IndicatorHost`] and keeps the handle;
//! the hub only reads heartbeats and writes views, so nothing here ever calls into a surface.

use std::sync::{Arc, Mutex};

use omuzaki_core::time::Clock;
use omuzaki_input_api::capture::{IndicatorHandle, IndicatorView};

use crate::shared::lock;

/// The indicators installed so far.
#[derive(Debug, Default)]
pub(crate) struct Indicators {
    handles: Mutex<Vec<IndicatorHandle>>,
}

impl Indicators {
    /// Installs one, timing its heartbeats on `clock`, showing `view` until the next write.
    pub(crate) fn install(&self, clock: Arc<dyn Clock>, view: &IndicatorView) -> IndicatorHandle {
        let handle = IndicatorHandle::new(clock);
        handle.publish(view);
        lock(&self.handles).push(handle.clone());
        handle
    }

    /// Whether any installed indicator was drawn within the last 1.5 s.
    pub(crate) fn shown(&self) -> bool {
        lock(&self.handles).iter().any(IndicatorHandle::is_shown)
    }

    /// Writes `view` to every indicator; a surface holding its view right now gets it on the next tick.
    pub(crate) fn publish(&self, view: &IndicatorView) {
        for handle in lock(&self.handles).iter() {
            handle.publish(view);
        }
    }
}
