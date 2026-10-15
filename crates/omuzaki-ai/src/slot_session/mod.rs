//! [`SlotSession`]: the session `ModelRegistry::open` returns. It follows its slot instead of one model (design
//! `docs/design/ai-api.md` section 7, ADR 0017):
//!
//! - a new selection, an install or a removal is noticed between runs and swapped in once it has loaded;
//! - a crash, a stall or any runtime error switches the model off for the app session and falls back down the
//!   slot's chain.
//!
//! **`run` never waits, allocates or drops a session.** Per call it reads the registry's generation, a flag and a
//! counter (atomics), `try_lock`s a small mailbox at most, and calls the current session. Everything slow or
//! allocating happens on the session's loader thread (`loader`): reading the chain, loading, writing the state,
//! releasing retired sessions. The two sides exchange only preallocated storage:
//!
//! - **gate:** the session answers only while the loader has confirmed the registry's current generation for the
//!   session it runs. After any change the answer is `Unavailable` until the loader says the same model is still
//!   right (a moment) or its replacement is in (a load), so a deselected model never answers. A generation is
//!   confirmed only by such an answer: with no loader, or after a load that panicked, the gate stays shut for every
//!   newer generation until a later load succeeds (`reset` asks again);
//! - **mailbox:** the loader publishes at most one `Ready` replacement; the run side hands it the sessions that
//!   were swapped out or crashed, to be switched off and dropped there. If the mailbox is busy the run side keeps
//!   them in its own preallocated queue and tries again on a later call.

mod loader;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::Thread;

use omuzaki_ai_api::{
    ModelRef, RegistryError, RunError, Session, SessionInfo, Signature, SlotRef, TensorBuf, TensorRef, find_slot,
};

use crate::registry::Registry;

/// How many retired sessions each side can hold without allocating: the crashed one and the swapped-out one are the
/// most that exist at once.
const RETIRE_CAPACITY: usize = 4;

/// A session and the model it runs.
struct Active {
    session: Box<dyn Session>,
    model: ModelRef,
}

/// Why a session was handed to the loader.
enum Why {
    /// It was swapped out.
    Replaced,
    /// It failed in `run`; the loader switches its model off.
    Failed(RunError),
}

struct Retired {
    active: Active,
    why: Why,
}

/// The loader's answer to a change: what the slot runs now, as of registry generation `generation`.
struct Ready {
    /// `None`: nothing in the chain can run (the model was removed, or every candidate failed to load).
    replacement: Option<Active>,
    generation: u64,
}

struct Mailbox {
    /// Loader to run side.
    ready: Option<Ready>,
    /// Run side to loader. Its capacity is fixed at [`RETIRE_CAPACITY`].
    retire: Vec<Retired>,
}

/// What the run side and the loader thread share.
struct Shared {
    registry: Registry,
    slot: SlotRef,
    /// The id the registry knows this session by, for `slots`.
    id: u64,
    mailbox: Mutex<Mailbox>,
    /// A `ready` is waiting in the mailbox. Set and cleared only under the mailbox lock.
    ready: AtomicBool,
    /// The newest generation at which the loader found the model it published (or the initial one) still right.
    confirmed: AtomicU64,
    /// The newest generation of a `Ready` the run side took in. The loader writes the health metadata (`info`, the
    /// registry's `running`) only for answers the run side has applied.
    applied: AtomicU64,
    /// A `reset` the session that is published next must have: the loader replays it before publishing, so the
    /// caller's thread never runs a runtime's `reset` on a session it swaps in.
    reset_owed: AtomicBool,
    /// The registry generation at the last `reset`.
    reset_gen: AtomicU64,
    /// Work for the loader: the run side sets it and unparks the thread.
    work: AtomicBool,
    closed: AtomicBool,
    /// For `info`, which is cold. The loader writes it when it publishes or confirms.
    info: Mutex<SessionInfo>,
}

/// A session that follows its slot: see the [module documentation](self).
pub(crate) struct SlotSession {
    shared: Arc<Shared>,
    explicit: Option<ModelRef>,
    signature: Signature,
    active: Option<Active>,
    /// Handle for waking the loader; `None` if its thread couldn't be started.
    loader: Option<Thread>,
    /// The generation the loader was last woken for.
    asked: u64,
    /// The generation of the last answer taken from the mailbox (or of `open`).
    applied: u64,
    /// Sessions waiting for room in the mailbox. Never grows past its capacity, so pushing never allocates.
    retiring: Vec<Retired>,
}

impl SlotSession {
    /// Wraps `session`, which `registry` just loaded for `slot` at `generation`. `wanted` is the model that would
    /// run if nothing had crashed: if the session runs another one, it stands in for it.
    ///
    /// # Errors
    /// `UnknownSlot` if the slot isn't registered.
    pub fn start(
        registry: Registry,
        slot: &SlotRef,
        explicit: Option<ModelRef>,
        generation: u64,
        session: Box<dyn Session>,
        wanted: Option<ModelRef>,
    ) -> Result<Self, RegistryError> {
        let signature = find_slot(slot).ok_or_else(|| RegistryError::UnknownSlot(slot.clone()))?.signature();
        let mut info = session.info();
        info.fallback_for = wanted.filter(|w| w != &info.model);
        let model = info.model.clone();
        let shared = Arc::new(Shared {
            id: registry.register_session(slot, Some(model.clone())),
            registry,
            slot: slot.clone(),
            mailbox: Mutex::new(Mailbox { ready: None, retire: Vec::with_capacity(RETIRE_CAPACITY) }),
            ready: AtomicBool::new(false),
            confirmed: AtomicU64::new(0),
            applied: AtomicU64::new(generation),
            reset_owed: AtomicBool::new(false),
            reset_gen: AtomicU64::new(generation),
            work: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            info: Mutex::new(info),
        });
        let loader = loader::spawn(&shared, explicit.clone(), Some(model.clone()));
        Ok(Self {
            shared,
            explicit,
            signature,
            active: Some(Active { session, model }),
            loader,
            asked: generation,
            applied: generation,
            retiring: Vec::with_capacity(RETIRE_CAPACITY),
        })
    }

    /// Wakes the loader. No allocation, no lock.
    fn poke(&self) {
        if let Some(loader) = &self.loader
            && !self.shared.work.swap(true, Ordering::AcqRel)
        {
            loader.unpark();
        }
    }

    /// True if the loader has confirmed registry generation `generation` for what this session runs.
    fn confirmed_for(&self, generation: u64) -> bool {
        !self.shared.ready.load(Ordering::Acquire)
            && self.applied.max(self.shared.confirmed.load(Ordering::Acquire)) >= generation
    }

    /// Everything the run side does besides running: wake the loader after a change, pass it retired sessions and
    /// take its answer. Returns the registry generation it saw.
    fn service(&mut self) -> u64 {
        let generation = self.shared.registry.generation();
        if generation != self.asked {
            self.asked = generation;
            self.poke();
        }
        self.flush();
        self.take_ready();
        generation
    }

    /// Moves retired sessions to the loader if the mailbox is free and has room.
    fn flush(&mut self) {
        if self.retiring.is_empty() {
            return;
        }
        let Some(mut mailbox) = self.shared.try_mailbox() else { return };
        while mailbox.retire.len() < RETIRE_CAPACITY
            && let Some(retired) = self.retiring.pop()
        {
            mailbox.retire.push(retired);
        }
        drop(mailbox);
        self.poke();
    }

    /// Takes the loader's answer if there is one and the mailbox is free.
    fn take_ready(&mut self) {
        if !self.shared.ready.load(Ordering::Acquire) || self.retiring.len() >= RETIRE_CAPACITY {
            return;
        }
        let Some(mut mailbox) = self.shared.try_mailbox() else { return };
        let ready = mailbox.ready.take();
        self.shared.ready.store(false, Ordering::Release);
        drop(mailbox);
        let Some(Ready { replacement, generation }) = ready else { return };
        self.applied = generation;
        let old = match replacement {
            Some(next) => self.active.replace(next),
            None => self.active.take(),
        };
        if let Some(active) = old {
            self.retiring.push(Retired { active, why: Why::Replaced });
            self.flush();
        }
        // Tell the loader, which owns the health metadata, that this answer is in.
        self.shared.applied.fetch_max(generation, Ordering::Release);
        self.shared.work.store(true, Ordering::Release);
        if let Some(loader) = &self.loader {
            loader.unpark();
        }
    }

    /// The running model failed: it leaves `run` for good, and the loader switches it off and loads the next one.
    fn fail(&mut self, error: RunError) {
        if let Some(active) = self.active.take() {
            self.retiring.push(Retired { active, why: Why::Failed(error) });
            self.flush();
        }
    }
}

impl Session for SlotSession {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        let generation = self.service();
        if !self.confirmed_for(generation) {
            return Err(RunError::Unavailable);
        }
        let Some(active) = self.active.as_mut() else { return Err(RunError::Unavailable) };
        match active.session.run(inputs, outputs) {
            Err(error @ (RunError::Crashed | RunError::Failed(_))) => {
                self.fail(error);
                Err(RunError::Unavailable)
            }
            other => other,
        }
    }

    /// Cold: also asks the loader again for the current generation (which retries a load that panicked) and
    /// restarts a loader thread that couldn't be started, neither of which `run` may do.
    fn reset(&mut self) {
        // Owed first, so a load that is already under way can't publish a session that missed it.
        self.shared.reset_gen.store(self.shared.registry.generation(), Ordering::Release);
        self.shared.reset_owed.store(true, Ordering::Release);
        let _ = self.service();
        if let Some(active) = self.active.as_mut() {
            active.session.reset();
        }
        if self.loader.is_none() {
            let belief = self.active.as_ref().map(|a| a.model.clone());
            self.loader = loader::spawn(&self.shared, self.explicit.clone(), belief);
        }
        self.asked = self.asked.wrapping_sub(1);
        let _ = self.service();
    }

    /// While the slot has no session (after a crash with nothing to fall back to), the model that ran last.
    fn info(&self) -> SessionInfo {
        self.shared.info.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Drop for SlotSession {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.registry.forget_session(self.shared.id);
        if let Some(loader) = &self.loader {
            self.shared.work.store(true, Ordering::Release);
            loader.unpark();
        }
    }
}
