//! [`SlotSession`]: the session `ModelRegistry::open` returns. It follows its slot instead of one model (design
//! `docs/design/ai-api.md` sections 7 and 9, ADR 0017):
//!
//! - **An ordinary change** (another selection, a new default, an install): the replacement loads on the registry's
//!   loader thread while the old session keeps answering, and the first `run` after it is ready hands over. A
//!   replacement that fails to load, or whose runtime panics while loading or resetting it, counts as a load failure:
//!   the chain goes on to the next model, and `slots` says why.
//! - **The model can no longer be used** (removed, disabled, its acknowledgement withdrawn): the old session stops at
//!   once, and `run` answers `Unavailable` until its replacement is applied.
//! - **A crash or a runtime error** switches the model off for the app session and falls back down the slot's chain,
//!   with `Unavailable` meanwhile.
//!
//! **`run` never waits, allocates, resets or drops a session.** Per call it reads a few atomics, `try_lock`s the
//! mailbox only when there is something to exchange, and calls the current session. Everything slow happens on the
//! registry's one loader thread (`loader`), shared by every slot session it opened. The two sides share:
//!
//! - **the mailbox:** the loader publishes at most one replacement (`Ready`); the run side hands it the sessions it
//!   retired, to be switched off and dropped there;
//! - **the withdrawal gate:** a commit that makes the applied model unusable records its generation in
//!   `withdrawn_at`. The session answers only while `cleared`, the generation of the chain its model was last chosen
//!   from, is at least that;
//! - **the reset count:** `reset` counts. A replacement carries the count the loader reset it to, and `run` applies
//!   it only if the count hasn't moved since; otherwise the loader resets it again and republishes;
//! - **the applied record:** which answer is applied and whether the session is silent. `info`, `slots` and `models`
//!   all derive what answers from it.

mod loader;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

use omuzaki_ai_api::{
    ModelRef, RegistryError, RunError, Session, SessionInfo, Signature, SlotRef, TensorBuf, TensorRef, find_slot,
};

pub(crate) use loader::Loader;

use crate::registry::Registry;

/// How many retired sessions each side holds without allocating.
///
/// One `run` retires at most two sessions: the one it swaps out and, if the replacement fails in the same call, that
/// one. The run side keeps this invariant: *while it has a session, its queue has a free place.* It holds when the
/// session opens (the queue is empty); a failure retires the session into that free place and leaves none applied;
/// and a replacement is applied only while two places are free ([`SlotSession::take_ready`]), one for the session it
/// swaps out and one kept for the replacement. So no push ever grows the queue past this capacity.
const RETIRE_CAPACITY: usize = 2;

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

/// The loader's answer: what the slot runs from now on.
struct Ready {
    /// `None`: nothing in the chain can run.
    replacement: Option<Active>,
    /// Its place among the answers; the applied record names it.
    serial: u64,
    /// The registry generation of the chain it was chosen from (see [`Shared`]'s `cleared`).
    cleared: u64,
    /// The reset count it was reset to.
    resets: u64,
    /// A commit made its model unusable before `run` took it: it is never applied, and the loader drops it.
    void: bool,
}

struct Mailbox {
    /// Loader to run side.
    ready: Option<Ready>,
    /// Run side to loader. Its capacity is fixed at [`RETIRE_CAPACITY`].
    retire: Vec<Retired>,
    /// The serial the next answer gets.
    next_serial: u64,
}

/// What one answer says about the slot.
struct Record {
    serial: u64,
    /// `None`: the answer is "nothing runs".
    info: Option<SessionInfo>,
}

struct Records {
    /// The applied answer's record and the newest published one's; the loader prunes the rest.
    list: Vec<Record>,
    /// The info of the model that answered last, for `info` while nothing answers.
    last: SessionInfo,
}

/// What the run side, the loader and the registry share for one slot session.
pub(crate) struct Shared {
    /// The id the registry knows this session by.
    pub(crate) id: u64,
    pub(crate) slot: SlotRef,
    /// The model `open` was asked for, which heads the chain.
    explicit: Option<ModelRef>,
    mailbox: Mutex<Mailbox>,
    /// A `Ready` that isn't void waits in the mailbox, reset to `ready_resets`. Both written under the mailbox lock.
    ready: AtomicBool,
    ready_resets: AtomicU64,
    /// How many times `reset` was called. Only the caller's thread adds to it.
    resets: AtomicU64,
    /// The applied record: the applied answer's serial shifted left once, with the low bit set while the session is
    /// silent (it applied "nothing runs", or its model failed). Written only by the run side.
    applied: AtomicU64,
    /// The registry generation of the chain the applied model was last chosen from. Written under the mailbox lock.
    cleared: AtomicU64,
    /// The newest generation whose commit made the applied model unusable. Written under the mailbox lock.
    withdrawn_at: AtomicU64,
    /// The loader has something to do here.
    work: AtomicBool,
    records: Mutex<Records>,
}

/// The low bit of [`Shared`]'s `applied`.
const SILENT: u64 = 1;

impl Shared {
    fn new(id: u64, slot: SlotRef, explicit: Option<ModelRef>, generation: u64, info: SessionInfo) -> Self {
        Self {
            id,
            slot,
            explicit,
            mailbox: Mutex::new(Mailbox { ready: None, retire: Vec::with_capacity(RETIRE_CAPACITY), next_serial: 1 }),
            ready: AtomicBool::new(false),
            ready_resets: AtomicU64::new(0),
            resets: AtomicU64::new(0),
            applied: AtomicU64::new(0),
            cleared: AtomicU64::new(generation),
            withdrawn_at: AtomicU64::new(0),
            work: AtomicBool::new(false),
            records: Mutex::new(Records { list: vec![Record { serial: 0, info: Some(info.clone()) }], last: info }),
        }
    }

    /// Locks the mailbox. Only the loader and commits wait for it, and only for a moment.
    fn mailbox(&self) -> MutexGuard<'_, Mailbox> {
        self.mailbox.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The run side's `try_lock`: `None` if someone else holds the mailbox right now.
    fn try_mailbox(&self) -> Option<MutexGuard<'_, Mailbox>> {
        match self.mailbox.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    fn records(&self) -> MutexGuard<'_, Records> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// True while the applied model was made unusable by a commit newer than the chain it was chosen from.
    fn gated(&self) -> bool {
        self.withdrawn_at.load(Ordering::Acquire) > self.cleared.load(Ordering::Acquire)
    }

    /// The model of the applied answer, silent or not.
    fn applied_model(&self, records: &Records) -> Option<ModelRef> {
        let serial = self.applied.load(Ordering::Acquire) >> 1;
        records.list.iter().find(|r| r.serial == serial)?.info.as_ref().map(|i| i.model.clone())
    }

    /// The model that answers `run` now, if any: what `slots` and `models` report as active.
    pub(crate) fn answering(&self) -> Option<ModelRef> {
        let records = self.records();
        if self.applied.load(Ordering::Acquire) & SILENT != 0 || self.gated() {
            return None;
        }
        self.applied_model(&records)
    }

    /// The applied answer's info, or the last model's while nothing answers. `fallback_for` is left to the caller.
    fn applied_info(&self) -> SessionInfo {
        let records = self.records();
        let serial = self.applied.load(Ordering::Acquire) >> 1;
        let applied = records.list.iter().find(|r| r.serial == serial).and_then(|r| r.info.clone());
        applied.unwrap_or_else(|| records.last.clone())
    }

    /// A commit at `generation` changed what `usable` allows. A waiting replacement whose model it rules out is never
    /// applied; an applied one stops answering at once, until the loader applies its replacement.
    pub(crate) fn withdraw(&self, generation: u64, usable: &dyn Fn(&ModelRef) -> bool) {
        let mut mailbox = self.mailbox();
        if let Some(ready) = mailbox.ready.as_mut().filter(|r| !r.void)
            && ready.replacement.as_ref().is_some_and(|a| !usable(&a.model))
        {
            ready.void = true;
            self.ready.store(false, Ordering::Release);
        }
        if self.applied_model(&self.records()).is_some_and(|m| !usable(&m)) {
            self.withdrawn_at.fetch_max(generation, Ordering::AcqRel);
        }
    }

    /// Takes the loader's work flag.
    pub(crate) fn take_work(&self) -> bool {
        self.work.swap(false, Ordering::AcqRel)
    }
}

/// A session that follows its slot: see the [module documentation](self).
pub(crate) struct SlotSession {
    registry: Registry,
    shared: Arc<Shared>,
    loader: Arc<Loader>,
    signature: Signature,
    active: Option<Active>,
    /// Sessions waiting for room in the mailbox. Never grows past its capacity (see [`RETIRE_CAPACITY`]).
    retiring: Vec<Retired>,
}

impl SlotSession {
    /// Wraps `session`, which `registry` just loaded for `slot` from the chain it read at `generation`.
    ///
    /// # Errors
    /// `UnknownSlot` if the slot isn't registered.
    pub fn start(
        registry: Registry,
        slot: &SlotRef,
        explicit: Option<ModelRef>,
        generation: u64,
        session: Box<dyn Session>,
        model: ModelRef,
    ) -> Result<Self, RegistryError> {
        let signature = find_slot(slot).ok_or_else(|| RegistryError::UnknownSlot(slot.clone()))?.signature();
        let shared =
            Arc::new(Shared::new(registry.next_session_id(), slot.clone(), explicit, generation, session.info()));
        registry.add_session(Arc::clone(&shared));
        // A commit that landed after the chain was read didn't see this session: check its model here instead.
        if !registry.is_usable(&model) {
            shared.withdrawn_at.fetch_max(registry.generation(), Ordering::AcqRel);
        }
        let loader = registry.loader();
        loader.poke(&shared);
        Ok(Self {
            registry,
            shared,
            loader,
            signature,
            active: Some(Active { session, model }),
            retiring: Vec::with_capacity(RETIRE_CAPACITY),
        })
    }

    /// Everything the run side does besides running: hand retired sessions over and take a replacement.
    fn service(&mut self) {
        if !self.retiring.is_empty()
            && let Some(mut mailbox) = self.shared.try_mailbox()
        {
            hand_over(&mut self.retiring, &mut mailbox);
            drop(mailbox);
            self.loader.poke(&self.shared);
        }
        if self.shared.ready.load(Ordering::Acquire)
            && self.shared.ready_resets.load(Ordering::Acquire) == self.shared.resets.load(Ordering::Acquire)
            && RETIRE_CAPACITY - self.retiring.len() >= 2
        {
            self.take_ready();
        }
    }

    /// Applies the loader's replacement if the mailbox is free and it was reset to the current count. The caller has
    /// checked that two places are free in `retiring`.
    fn take_ready(&mut self) {
        let Some(mut mailbox) = self.shared.try_mailbox() else { return };
        let resets = self.shared.resets.load(Ordering::Acquire);
        if !mailbox.ready.as_ref().is_some_and(|r| !r.void && r.resets == resets) {
            return;
        }
        let Some(Ready { replacement, serial, cleared, .. }) = mailbox.ready.take() else { return };
        self.shared.ready.store(false, Ordering::Release);
        self.shared.cleared.store(cleared, Ordering::Release);
        self.shared.applied.store(serial << 1 | u64::from(replacement.is_none()), Ordering::Release);
        if let Some(active) = std::mem::replace(&mut self.active, replacement) {
            debug_assert!(self.retiring.len() + 2 <= RETIRE_CAPACITY, "applied without two free places");
            self.retiring.push(Retired { active, why: Why::Replaced });
        }
        hand_over(&mut self.retiring, &mut mailbox);
        drop(mailbox);
        self.loader.poke(&self.shared);
    }

    /// The running model failed: it stops answering for good, and the loader switches it off and loads the next one.
    fn fail(&mut self, error: RunError) {
        let Some(active) = self.active.take() else { return };
        self.shared.applied.fetch_or(SILENT, Ordering::AcqRel);
        debug_assert!(self.retiring.len() < RETIRE_CAPACITY, "a session was applied without a free place");
        self.retiring.push(Retired { active, why: Why::Failed(error) });
        self.service();
    }
}

/// Moves retired sessions into the mailbox while it has room.
fn hand_over(retiring: &mut Vec<Retired>, mailbox: &mut Mailbox) {
    while mailbox.retire.len() < RETIRE_CAPACITY
        && let Some(retired) = retiring.pop()
    {
        mailbox.retire.push(retired);
    }
}

impl Session for SlotSession {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.service();
        if self.shared.gated() {
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

    /// Resets the session that runs now, here, and counts the reset, so a replacement the loader reset to an older
    /// count is reset again before it is applied. Cold: also starts the registry's loader if it isn't running.
    fn reset(&mut self) {
        self.shared.resets.fetch_add(1, Ordering::AcqRel);
        if let Some(active) = self.active.as_mut() {
            active.session.reset();
        }
        self.registry.start_loader();
        self.loader.poke(&self.shared);
    }

    /// The model that answers, or while nothing does, the one that answered last. `fallback_for` names the model the
    /// slot would run if nothing had crashed or failed to load, when that is another one.
    fn info(&self) -> SessionInfo {
        let mut info = self.shared.applied_info();
        info.fallback_for = self.registry.wanted(&self.shared.slot, self.shared.explicit.as_ref());
        info.fallback_for = info.fallback_for.filter(|w| *w != info.model);
        info
    }
}

impl Drop for SlotSession {
    fn drop(&mut self) {
        self.registry.forget_session(self.shared.id);
    }
}
