//! The loader thread of one slot session. It owns every decision and every slow step: reading the choice chain,
//! loading, switching a crashed model off, dropping retired sessions. The thread that calls `run` only hands it work
//! (an unpark) and takes what it publishes (a `try_lock`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard, PoisonError, TryLockError};
use std::thread::{self, Thread};

use omuzaki_ai_api::{ModelRef, Session, SessionInfo};

use super::{Active, Ready, Shared, Why};
use crate::registry::Chosen;

/// Starts the loader for `shared`. `belief` is the model the session runs.
pub(super) fn spawn(shared: &Arc<Shared>, explicit: Option<ModelRef>, belief: Option<ModelRef>) -> Option<Thread> {
    let shared = Arc::clone(shared);
    thread::Builder::new()
        .name(format!("slot-session {}", shared.slot))
        .spawn(move || work(&shared, Worker { explicit, belief }))
        .inspect_err(|e| tracing::warn!("couldn't start a slot session's loader thread: {e}"))
        .ok()
        .map(|handle| handle.thread().clone())
}

struct Worker {
    explicit: Option<ModelRef>,
    /// The model whose session the run side has, or will have once it takes what was published.
    belief: Option<ModelRef>,
}

fn work(shared: &Shared, mut worker: Worker) {
    loop {
        while !shared.work.swap(false, Ordering::AcqRel) {
            thread::park();
        }
        let generation = shared.registry.generation();
        let closing = shared.closed.load(Ordering::Acquire);
        // A runtime that panics must not take the loader with it: the session would never change model again.
        if catch_unwind(AssertUnwindSafe(|| worker.service(shared, closing))).is_err() {
            tracing::error!(slot = %shared.slot, "a model runtime panicked in the loader thread");
            worker.belief = None;
            shared.confirm(generation);
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
            self.choose(shared);
        }
        drop(graveyard);
    }

    /// `model` failed in `run`: it is off for this app session and the slot has nothing loaded until a fallback is.
    fn switch_off(&mut self, shared: &Shared, model: &ModelRef, error: &str) {
        let reason = format!("it stopped working ({error})");
        tracing::warn!(slot = %shared.slot, %model, "{reason}; switching it off until Omuzaki restarts");
        if self.belief.as_ref() == Some(model) {
            self.belief = None;
            shared.registry.report_running(shared.id, None);
        }
        shared.registry.disable_for_session(model, &reason);
    }

    /// Settles what the slot should run now and publishes the answer.
    fn choose(&mut self, shared: &Shared) {
        // Before the chain is read, so a change that lands meanwhile makes this answer stale and the run side asks again.
        let generation = shared.registry.generation();
        match shared.registry.load_chain(&shared.slot, self.explicit.as_ref(), self.belief.as_ref()) {
            Ok(Chosen::Keep { wanted }) => {
                shared.set_fallback_for(wanted, self.belief.as_ref());
                shared.confirm(generation);
            }
            Ok(Chosen::Loaded { session, model, wanted }) => {
                if let Some(fallback) = wanted.as_ref().filter(|w| *w != &model) {
                    tracing::warn!(slot = %shared.slot, %model, original = %fallback, "the slot runs a fallback");
                }
                self.belief = Some(model.clone());
                shared.registry.report_running(shared.id, Some(model.clone()));
                shared.set_info(session.info(), wanted, &model);
                shared.publish(Ready { replacement: Some(Active { session, model }), generation });
            }
            Err(e) => {
                tracing::warn!(slot = %shared.slot, "no model can run: {e}");
                self.belief = None;
                shared.registry.report_running(shared.id, None);
                shared.publish(Ready { replacement: None, generation });
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

    /// The loader checked generation `generation`: the session it has published (or the run side holds) is right.
    fn confirm(&self, generation: u64) {
        self.confirmed.fetch_max(generation, Ordering::Release);
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

    fn set_info(&self, mut info: SessionInfo, wanted: Option<ModelRef>, running: &ModelRef) {
        info.fallback_for = wanted.filter(|w| w != running);
        *self.info.lock().unwrap_or_else(PoisonError::into_inner) = info;
    }

    fn set_fallback_for(&self, wanted: Option<ModelRef>, running: Option<&ModelRef>) {
        let mut info = self.info.lock().unwrap_or_else(PoisonError::into_inner);
        info.fallback_for = wanted.filter(|w| Some(w) != running);
    }
}
