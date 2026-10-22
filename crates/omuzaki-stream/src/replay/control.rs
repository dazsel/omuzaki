//! The replay buffer on the stream service: started and stopped by the `record.replay_*` ops, saved by
//! `record.save_replay`.

use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::Seconds;
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::output::OutputConfig;

use super::runtime::{ReplayBuffer, ReplayError, ReplayHandle, ReplayPlan, ReplayStatus, SaveReport};
use crate::service::{OutputId, StreamService};

/// The running replay buffer.
pub(crate) struct ActiveReplay {
    id: OutputId,
    handle: ReplayHandle,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl StreamService {
    /// Starts the replay buffer on the program picture with `plan`.
    ///
    /// # Errors
    /// [`StreamError::Busy`] while it already runs, or what the buffer's start reports (an encoder that won't open).
    pub fn replay_start(&self, plan: ReplayPlan) -> Result<ReplayStatus, StreamError> {
        let mut active = lock(&self.replay);
        if active.is_some() {
            return Err(StreamError::Busy);
        }
        let program = self.program();
        let buffer = ReplayBuffer::new(plan);
        let handle = buffer.handle();
        let id = self.start(Box::new(buffer), OutputConfig::new("replay", program.size, program.fps)?)?;
        let status = handle.status();
        *active = Some(ActiveReplay { id, handle });
        Ok(status)
    }

    /// Stops the replay buffer; saves already asked for complete first.
    ///
    /// # Errors
    /// [`StreamError::Stopped`] when it isn't running.
    pub fn replay_stop(&self) -> Result<ReplayStatus, StreamError> {
        let replay = lock(&self.replay).take().ok_or(StreamError::Stopped)?;
        self.stop(replay.id)?;
        Ok(replay.handle.status())
    }

    /// Saves the last `length` seconds (the buffer's length when `None`) and waits for the clip to be complete.
    ///
    /// # Errors
    /// [`ReplayError::Disabled`] when the buffer isn't running, [`ReplayError::Empty`], [`ReplayError::Busy`], or the
    /// write's error.
    pub fn replay_save(&self, length: Option<Seconds>) -> Result<SaveReport, ReplayError> {
        let handle = lock(&self.replay).as_ref().map(|r| r.handle.clone()).ok_or(ReplayError::Disabled)?;
        handle.save(length)?.wait()
    }

    /// The replay buffer's status; `running: false` when it is off.
    pub fn replay_status(&self) -> ReplayStatus {
        lock(&self.replay).as_ref().map(|r| r.handle.status()).unwrap_or_default()
    }
}
