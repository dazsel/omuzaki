//! [`ProgramFeed`]: how a render loop feeds the [`StreamService`]. The loop draws far more often than an output takes
//! frames (a UI at 60 to 120 Hz, a take at 30 fps), and reading a frame back off the GPU is the costly part, so the
//! feed asks for the pixels only while an output runs and only when a frame is due on the program's grid: the frame
//! nearest each slot, as the service's pacer picks them.

use std::sync::Arc;

use omuzaki_render_api::Image;
use omuzaki_stream_api::time::Ticks;

use crate::service::{PushOutcome, StreamService};

const NANOS: i128 = 1_000_000_000;

/// What [`ProgramFeed::offer`] did.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    /// No output runs: nothing was read.
    Idle,
    /// An output runs but no frame is due yet: nothing was read.
    NotDue,
    /// The frame was read and handed to the service.
    Pushed(PushOutcome),
}

/// Feeds a render loop's frames to a [`StreamService`] at the program's rate.
pub struct ProgramFeed {
    service: Arc<StreamService>,
    /// The grid's zero: the first frame pushed since outputs started.
    t0: Option<Ticks>,
    /// The next slot to fill.
    next: u64,
}

impl std::fmt::Debug for ProgramFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgramFeed").field("t0", &self.t0).field("next", &self.next).finish_non_exhaustive()
    }
}

impl ProgramFeed {
    /// A feed into `service`.
    pub fn new(service: Arc<StreamService>) -> Self {
        Self { service, t0: None, next: 0 }
    }

    /// The service it feeds.
    pub fn service(&self) -> &Arc<StreamService> {
        &self.service
    }

    /// Whether any output runs. While one does, the loop draws the program clean: the character alone, with alpha.
    pub fn wanted(&self) -> bool {
        self.service.active()
    }

    /// Offers the frame just drawn: when an output runs and a slot is due, calls `read` for its pixels and pushes them,
    /// stamped now on the service's clock.
    ///
    /// # Errors
    /// What `read` returns; nothing is pushed then and the slot stays due.
    pub fn offer<E>(&mut self, read: impl FnOnce() -> Result<Image, E>) -> Result<Offer, E> {
        if !self.service.active() {
            // The next run of outputs starts a new grid.
            (self.t0, self.next) = (None, 0);
            return Ok(Offer::Idle);
        }
        let now = self.service.now();
        let fps = self.service.program().fps;
        let t0 = self.t0.unwrap_or(now);
        let slot = nearest_slot(t0, now, fps);
        if slot < self.next {
            return Ok(Offer::NotDue);
        }
        let image = read()?;
        self.t0 = Some(t0);
        self.next = slot.saturating_add(1);
        Ok(Offer::Pushed(self.service.push(&image, now)))
    }
}

/// The slot of a `fps` grid starting at `t0` nearest to `t`.
fn nearest_slot(t0: Ticks, t: Ticks, fps: (u32, u32)) -> u64 {
    let elapsed = i128::from(t.0.saturating_sub(t0.0)).max(0);
    let den = (i128::from(fps.1) * NANOS).max(1);
    u64::try_from((2 * elapsed * i128::from(fps.0) + den) / (2 * den)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_slot_rounds_half_a_period_either_way() {
        let t0 = Ticks(1_000);
        let at = |ms: i64| Ticks(1_000 + ms * 1_000_000);
        assert_eq!(nearest_slot(t0, t0, (30, 1)), 0);
        assert_eq!(nearest_slot(t0, at(16), (30, 1)), 0);
        assert_eq!(nearest_slot(t0, at(17), (30, 1)), 1);
        assert_eq!(nearest_slot(t0, at(1000), (30, 1)), 30);
        assert_eq!(nearest_slot(t0, at(1000), (30_000, 1001)), 30);
        assert_eq!(nearest_slot(at(5), t0, (30, 1)), 0, "a time before the grid is its first slot");
    }
}
