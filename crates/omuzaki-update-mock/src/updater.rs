//! A scripted [`Updater`] with a real hold registry.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use omuzaki_update_api::error::UpdateError;
use omuzaki_update_api::event::{EventSender, UpdateEvent, UpdateEvents, event_queue};
use omuzaki_update_api::hold::{Hold, HoldReason};
use omuzaki_update_api::progress::Progress;
use omuzaki_update_api::release::{Channel, Release};
use omuzaki_update_api::status::UpdateStatus;
use omuzaki_update_api::updater::Updater;

use crate::locked;

/// What a scripted step answers; the queue is consumed one per call.
type Script<T> = VecDeque<Result<T, UpdateError>>;

#[derive(Default)]
struct Scripts {
    check: Script<Option<Release>>,
    download: Script<()>,
    stage: Script<()>,
    rollback: Script<()>,
    healthy: Script<()>,
}

/// Holds in the order they were taken, each with an id so a drop removes its own.
#[derive(Default)]
struct Holds {
    next_id: u64,
    live: Vec<(u64, HoldReason)>,
}

struct Inner {
    status: UpdateStatus,
    scripts: Scripts,
    senders: Vec<EventSender>,
    healthy_calls: u32,
    checks: Vec<Channel>,
}

/// An [`Updater`] for tests. Each of `check`, `download`, `stage`, `rollback` and `healthy` answers from a queue the
/// test fills with the `script_*` methods; an empty queue answers with a default (no update, success, success,
/// [`UpdateError::NothingToRollBack`], and success).
///
/// Status follows the answers: a `check` that finds a release is `Available`, one that finds none is `Idle`, a `stage`
/// that succeeds is `Staged` with the holds taken at that moment; failures leave it alone. [`MockUpdater::set_status`]
/// sets it directly. Holds are real: [`Updater::hold`] registers one and dropping the [`Hold`] removes it.
pub struct MockUpdater {
    inner: Mutex<Inner>,
    holds: Arc<Mutex<Holds>>,
}

impl Default for MockUpdater {
    fn default() -> Self {
        Self::new()
    }
}

impl MockUpdater {
    /// An updater that is `Idle`, has no holds and no scripted answers.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                status: UpdateStatus::Idle { last_checked: None },
                scripts: Scripts::default(),
                senders: Vec::new(),
                healthy_calls: 0,
                checks: Vec::new(),
            }),
            holds: Arc::default(),
        }
    }

    /// Queues the answer to the next `check`.
    pub fn script_check(&self, answer: Result<Option<Release>, UpdateError>) {
        locked(&self.inner).scripts.check.push_back(answer);
    }

    /// Queues the answer to the next `download`.
    pub fn script_download(&self, answer: Result<(), UpdateError>) {
        locked(&self.inner).scripts.download.push_back(answer);
    }

    /// Queues the answer to the next `stage`.
    pub fn script_stage(&self, answer: Result<(), UpdateError>) {
        locked(&self.inner).scripts.stage.push_back(answer);
    }

    /// Queues the answer to the next `rollback`.
    pub fn script_rollback(&self, answer: Result<(), UpdateError>) {
        locked(&self.inner).scripts.rollback.push_back(answer);
    }

    /// Queues the answer to the next `healthy`, for a test of a marker that cannot be written (a full disk).
    pub fn script_healthy(&self, answer: Result<(), UpdateError>) {
        locked(&self.inner).scripts.healthy.push_back(answer);
    }

    /// Sets the status and tells the subscribers.
    pub fn set_status(&self, status: UpdateStatus) {
        let mut inner = locked(&self.inner);
        Self::change_status(&mut inner, status);
    }

    /// The live holds, in the order they were taken.
    #[must_use]
    pub fn holds(&self) -> Vec<HoldReason> {
        locked(&self.holds).live.iter().map(|(_, why)| *why).collect()
    }

    /// How many times `healthy` was called, whether it succeeded or not.
    #[must_use]
    pub fn healthy_calls(&self) -> u32 {
        locked(&self.inner).healthy_calls
    }

    /// The channel of every `check`, in order.
    #[must_use]
    pub fn checks(&self) -> Vec<Channel> {
        locked(&self.inner).checks.clone()
    }

    fn change_status(inner: &mut Inner, status: UpdateStatus) {
        inner.status = status.clone();
        Self::emit(inner, &UpdateEvent::Status(status));
    }

    fn emit(inner: &mut Inner, event: &UpdateEvent) {
        inner.senders.retain(EventSender::is_subscribed);
        for sender in &inner.senders {
            sender.send(event.clone());
        }
    }
}

impl Updater for MockUpdater {
    fn status(&self) -> UpdateStatus {
        locked(&self.inner).status.clone()
    }

    fn check(&self, channel: Channel) -> Result<Option<Release>, UpdateError> {
        let mut inner = locked(&self.inner);
        inner.checks.push(channel);
        let found = inner.scripts.check.pop_front().unwrap_or(Ok(None))?;
        let status = match &found {
            Some(release) => UpdateStatus::Available { release: release.clone() },
            None => UpdateStatus::Idle { last_checked: None },
        };
        Self::change_status(&mut inner, status);
        Ok(found)
    }

    fn download(&self, release: &Release, progress: &dyn Progress) -> Result<(), UpdateError> {
        // The guard is gone before `report` runs: a callback may call back into this updater (`status()` to draw UI).
        locked(&self.inner).scripts.download.pop_front().unwrap_or(Ok(()))?;
        let total = release.download_bytes;
        progress.report(total, total);
        Self::emit(&mut locked(&self.inner), &UpdateEvent::Progress { done: total, total });
        Ok(())
    }

    fn stage(&self, release: &Release) -> Result<(), UpdateError> {
        let mut inner = locked(&self.inner);
        inner.scripts.stage.pop_front().unwrap_or(Ok(()))?;
        let deferred_by = self.holds();
        Self::change_status(&mut inner, UpdateStatus::Staged { version: release.version.clone(), deferred_by });
        Ok(())
    }

    fn rollback(&self) -> Result<(), UpdateError> {
        locked(&self.inner).scripts.rollback.pop_front().unwrap_or(Err(UpdateError::NothingToRollBack))
    }

    fn hold(&self, why: HoldReason) -> Hold {
        let id = {
            let mut holds = locked(&self.holds);
            holds.next_id += 1;
            let id = holds.next_id;
            holds.live.push((id, why));
            id
        };
        let holds = Arc::clone(&self.holds);
        Hold::new(move || locked(&holds).live.retain(|(held, _)| *held != id))
    }

    fn healthy(&self) -> Result<(), UpdateError> {
        let mut inner = locked(&self.inner);
        inner.healthy_calls += 1;
        inner.scripts.healthy.pop_front().unwrap_or(Ok(()))
    }

    fn subscribe(&self) -> UpdateEvents {
        let (sender, events) = event_queue();
        locked(&self.inner).senders.push(sender);
        events
    }

    fn restart_allowed(&self) -> Result<(), UpdateError> {
        let held = self.holds();
        if held.is_empty() { Ok(()) } else { Err(UpdateError::Busy(held)) }
    }
}
