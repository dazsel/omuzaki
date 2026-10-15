//! The loader thread of one slot session. It owns every decision and every slow step: reading the choice chain,
//! loading, replaying a reset on what it loaded, switching a crashed model off, dropping retired sessions and writing
//! the health metadata. The thread that calls `run` only hands it work (an unpark) and takes what it publishes (a
//! `try_lock`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard, PoisonError, TryLockError};
use std::thread::{self, Thread};

use omuzaki_ai_api::{ModelRef, Session, SessionInfo};

use super::{Active, Ready, Shared, Why};
use crate::registry::Chosen;

#[cfg(test)]
thread_local! {
    /// Makes [`spawn`] fail on this thread, to test a session that has no loader.
    pub(super) static FAIL_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Starts the loader for `shared`. `belief` is the model the session runs.
pub(super) fn spawn(shared: &Arc<Shared>, explicit: Option<ModelRef>, belief: Option<ModelRef>) -> Option<Thread> {
    #[cfg(test)]
    if FAIL_SPAWN.with(std::cell::Cell::get) {
        return None;
    }
    let shared = Arc::clone(shared);
    thread::Builder::new()
        .name(format!("slot-session {}", shared.slot))
        .spawn(move || work(&shared, Worker { explicit, belief, pending: None, last_keep: 0 }))
        .inspect_err(|e| tracing::warn!("couldn't start a slot session's loader thread: {e}"))
        .ok()
        .map(|handle| handle.thread().clone())
}

/// What a published answer will say about the slot once the run side has taken it in.
struct Pending {
    generation: u64,
    /// The model that will run, if any.
    model: Option<ModelRef>,
    /// Its `SessionInfo`; `None` when nothing will run.
    info: Option<SessionInfo>,
}

struct Worker {
    explicit: Option<ModelRef>,
    /// The model whose session the run side has, or will have once it takes what was published.
    belief: Option<ModelRef>,
    /// The metadata of the last answer published and not yet shown: it describes what runs only once the run side has
    /// applied the answer *and* nothing newer is unsettled.
    pending: Option<Pending>,
    /// The newest generation at which the loader found `belief` still right.
    last_keep: u64,
}

fn work(shared: &Shared, mut worker: Worker) {
    loop {
        while !shared.work.swap(false, Ordering::AcqRel) {
            thread::park();
        }
        let closing = shared.closed.load(Ordering::Acquire);
        let generation = shared.registry.generation();
        // A runtime that panics must not take the loader with it. The generation stays unconfirmed: the slot answers
        // `Unavailable` (an explicit answer, published here) until a later change or `reset` makes the next attempt.
        if catch_unwind(AssertUnwindSafe(|| worker.service(shared, closing))).is_err() {
            tracing::error!(slot = %shared.slot, "a model runtime panicked in the loader thread");
            worker.nothing_runs(shared, generation);
        }
        if closing {
            return;
        }
    }
}

impl Worker {
    fn service(&mut self, shared: &Shared, closing: bool) {
        // Dropped below, with the lock released: releasing a runtime's session can take milliseconds.
        // (Emptied by popping, not by taking the vector: the run side relies on its fixed capacity.)
        let mut retired = Vec::new();
        {
            let mut mailbox = shared.mailbox();
            while let Some(entry) = mailbox.retire.pop() {
                retired.push(entry);
            }
        }
        let mut graveyard: Vec<Box<dyn Session>> = Vec::new();
        for retired in retired {
            let Active { session, model } = retired.active;
            if let Why::Failed(error) = retired.why {
                self.switch_off(shared, &model, &error.to_string());
            }
            graveyard.push(session);
        }
        if !closing {
            self.choose(shared, &mut graveyard);
            self.show_applied(shared);
        }
        drop(graveyard);
    }

    /// `model` failed in `run`: it is off for this app session and the slot has nothing loaded until a fallback is.
    fn switch_off(&mut self, shared: &Shared, model: &ModelRef, error: &str) {
        let reason = format!("it stopped working ({error})");
        tracing::warn!(slot = %shared.slot, %model, "{reason}; switching it off until Omuzaki restarts");
        // It was what ran, whatever an unapplied answer may say.
        shared.registry.report_running(shared.id, None);
        if self.belief.as_ref() == Some(model) {
            self.belief = None;
        }
        shared.registry.disable_for_session(model, &reason);
    }

    /// Settles what the slot should run now and publishes the answer.
    fn choose(&mut self, shared: &Shared, graveyard: &mut Vec<Box<dyn Session>>) {
        loop {
            // Before the chain is read, so a change that lands meanwhile makes this answer stale.
            let generation = shared.registry.generation();
            let chosen = shared.registry.load_chain(&shared.slot, self.explicit.as_ref(), self.belief.as_ref());
            if shared.registry.generation() != generation
                && let Ok(Chosen::Loaded { session, .. }) = chosen
            {
                // The choice moved on while this loaded: it is never published, so it never shows up as running.
                graveyard.push(session);
                continue;
            }
            match chosen {
                Ok(Chosen::Keep { wanted }) => self.keep(shared, generation, wanted),
                Ok(Chosen::Loaded { mut session, model, wanted }) => {
                    if shared.reset_owed.swap(false, Ordering::AcqRel) {
                        session.reset();
                    }
                    self.publish_loaded(shared, generation, Active { session, model }, wanted);
                }
                Err(e) => {
                    tracing::warn!(slot = %shared.slot, "no model can run: {e}");
                    self.nothing_runs(shared, generation);
                }
            }
            return;
        }
    }

    /// The model the session has is still the best one: refresh what it stands in for, and open the gate.
    fn keep(&mut self, shared: &Shared, generation: u64, wanted: Option<ModelRef>) {
        let fallback_for = wanted.filter(|w| Some(w) != self.belief.as_ref());
        match &mut self.pending {
            Some(pending) => {
                if let Some(info) = &mut pending.info {
                    info.fallback_for = fallback_for;
                }
            }
            None => shared.info.lock().unwrap_or_else(PoisonError::into_inner).fallback_for = fallback_for,
        }
        if generation >= shared.reset_gen.load(Ordering::Acquire) {
            // Nothing is swapped in for the change that preceded the reset, so no session needs it replayed.
            shared.reset_owed.store(false, Ordering::Release);
        }
        self.last_keep = self.last_keep.max(generation);
        shared.confirmed.fetch_max(generation, Ordering::Release);
    }

    fn publish_loaded(&mut self, shared: &Shared, generation: u64, active: Active, wanted: Option<ModelRef>) {
        if let Some(original) = wanted.as_ref().filter(|w| **w != active.model) {
            tracing::warn!(slot = %shared.slot, model = %active.model, %original, "the slot runs a fallback");
        }
        let mut info = active.session.info();
        info.fallback_for = wanted.filter(|w| *w != active.model);
        self.belief = Some(active.model.clone());
        self.pending = Some(Pending { generation, model: Some(active.model.clone()), info: Some(info) });
        shared.publish(Ready { replacement: Some(active), generation });
    }

    /// Publishes the explicit answer "nothing can run" for `generation`.
    fn nothing_runs(&mut self, shared: &Shared, generation: u64) {
        self.belief = None;
        self.pending = Some(Pending { generation, model: None, info: None });
        shared.publish(Ready { replacement: None, generation });
    }

    /// Shows the last answer in `info` and in the registry's `slots` once it is what runs: the run side has applied
    /// it and no newer change is unsettled. An answer that was overtaken is simply never shown.
    fn show_applied(&mut self, shared: &Shared) {
        let Some(pending) = &self.pending else { return };
        let settled = pending.generation.max(self.last_keep) >= shared.registry.generation();
        if !settled || shared.applied.load(Ordering::Acquire) < pending.generation {
            return;
        }
        if let Some(Pending { model, info, .. }) = self.pending.take() {
            shared.registry.report_running(shared.id, model);
            if let Some(info) = info {
                *shared.info.lock().unwrap_or_else(PoisonError::into_inner) = info;
            }
        }
    }
}

impl Shared {
    /// Locks the mailbox. Only the loader waits for it, and only for a moment.
    fn mailbox(&self) -> MutexGuard<'_, super::Mailbox> {
        self.mailbox.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The run side's `try_lock`: `None` if the loader holds the mailbox right now.
    pub(super) fn try_mailbox(&self) -> Option<MutexGuard<'_, super::Mailbox>> {
        match self.mailbox.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// Hands the run side its answer. One nobody took yet is superseded and dropped here, off the lock.
    fn publish(&self, ready: Ready) {
        let mut mailbox = self.mailbox();
        let superseded = mailbox.ready.replace(ready);
        // Set under the lock, as the run side clears it under the lock, so neither can undo the other.
        self.ready.store(true, Ordering::Release);
        drop(mailbox);
        drop(superseded);
    }
}
