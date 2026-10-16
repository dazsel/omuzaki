//! The registry's loader: one thread for every slot session the registry opened (design `docs/design/ai-api.md`,
//! "The registry owns loader threads"), so the number of loader threads doesn't grow with the sessions. It is started
//! on first need and ends when the registry is dropped.
//!
//! It owns every decision and every slow step: reading the choice chain, loading, resetting what it loaded, switching
//! a crashed model off and dropping retired sessions. The thread that calls `run` only hands it work (atomics and an
//! unpark) and takes what it publishes (a `try_lock`).

use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::thread::{self, Thread};

use omuzaki_ai_api::{ModelRef, RunError, Session};

use super::{Active, Mailbox, Ready, Record, Retired, SILENT, Shared, Why};
use crate::registry::{self, Chosen, Registry};

#[cfg(test)]
thread_local! {
    /// Makes [`Loader::start`] fail on this thread, to test a registry whose loader couldn't be started.
    pub(super) static FAIL_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The handle every slot session of one registry shares with its loader thread.
pub(crate) struct Loader {
    thread: OnceLock<Thread>,
    /// Some session has work: set by [`poke`](Self::poke), taken by the thread.
    wake: AtomicBool,
    closed: AtomicBool,
}

impl Loader {
    /// A loader whose thread isn't started yet.
    pub(crate) fn new() -> Self {
        // `wake` starts set, so a thread started late looks at the sessions that asked for it meanwhile.
        Self { thread: OnceLock::new(), wake: AtomicBool::new(true), closed: AtomicBool::new(false) }
    }

    /// True once the thread runs.
    pub(crate) fn started(&self) -> bool {
        self.thread.get().is_some()
    }

    /// Starts the thread for the registry `registry` points at. The caller serialises calls.
    ///
    /// # Errors
    /// The OS refused the thread.
    pub(crate) fn start(self: &Arc<Self>, registry: Weak<registry::Shared>) -> io::Result<()> {
        #[cfg(test)]
        if FAIL_SPAWN.with(std::cell::Cell::get) {
            return Err(io::Error::other("refused by the test"));
        }
        let loader = Arc::clone(self);
        let handle = thread::Builder::new().name("slot-session loader".into()).spawn(move || loader.work(&registry))?;
        let fresh = self.thread.set(handle.thread().clone()).is_ok();
        debug_assert!(fresh, "the loader was started twice; the registry serialises starting it");
        Ok(())
    }

    /// Asks the thread to look at `session`. No lock, no allocation: the run side calls it.
    pub(crate) fn poke(&self, session: &Shared) {
        session.work.store(true, Ordering::Release);
        if !self.wake.swap(true, Ordering::AcqRel)
            && let Some(thread) = self.thread.get()
        {
            thread.unpark();
        }
    }

    /// Ends the thread: its registry is gone.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }

    fn work(&self, registry: &Weak<registry::Shared>) {
        loop {
            while !self.wake.swap(false, Ordering::AcqRel) {
                if self.closed.load(Ordering::Acquire) {
                    return;
                }
                thread::park();
            }
            if self.closed.load(Ordering::Acquire) {
                return;
            }
            let Some(registry) = Registry::upgrade(registry) else { return };
            for session in registry.sessions_with_work() {
                // A runtime that panics must not take the loader, and every other session, with it. Loads and the
                // reset of a replacement catch their own panics (`Registry::load_chain`, `choose`); this is for the
                // rest (a drop).
                if catch_unwind(AssertUnwindSafe(|| serve(&registry, &session))).is_err() {
                    tracing::error!(slot = %session.slot, "a model runtime panicked in the loader thread");
                }
            }
        }
    }
}

/// One pass for `session`: switch off and drop what it retired, then settle what it should run.
fn serve(registry: &Registry, session: &Shared) {
    let mut graveyard: Vec<Box<dyn Session>> = Vec::new();
    let (retired, in_hand) = collect(session, &mut graveyard);
    for Retired { active, why } in retired {
        if let Why::Failed(error) = why {
            switch_off(registry, session, &active.model, &error);
        }
        graveyard.push(active.session);
    }
    choose(registry, session, in_hand, &mut graveyard);
    // Released here, with no lock held: releasing a runtime's session can take milliseconds.
    drop(graveyard);
}

/// Empties the mailbox's retired sessions (by popping: the run side relies on its capacity), drops a void replacement
/// and takes back one reset to an older count, to reset it again.
fn collect(session: &Shared, graveyard: &mut Vec<Box<dyn Session>>) -> (Vec<Retired>, Option<Active>) {
    let mut mailbox = session.mailbox();
    let mut retired = Vec::new();
    while let Some(entry) = mailbox.retire.pop() {
        retired.push(entry);
    }
    let resets = session.resets.load(Ordering::Acquire);
    if !mailbox.ready.as_ref().is_some_and(|r| r.void || r.resets != resets) {
        return (retired, None);
    }
    session.ready.store(false, Ordering::Release);
    match mailbox.ready.take() {
        Some(Ready { replacement, void: true, .. }) => {
            graveyard.extend(replacement.map(|a| a.session));
            (retired, None)
        }
        Some(Ready { replacement, .. }) => (retired, replacement),
        None => (retired, None),
    }
}

/// `model` failed in `run`: it is off for this app session.
fn switch_off(registry: &Registry, session: &Shared, model: &ModelRef, error: &RunError) {
    let reason = format!("it stopped working ({error})");
    tracing::warn!(slot = %session.slot, %model, "{reason}; switching it off until Omuzaki restarts");
    registry.disable_for_session(model, &reason);
}

/// What a replacement whose runtime panicked while it was reset is recorded as.
const RESET_PANICKED: &str = "its runtime failed while resetting it";

/// What the run side has and will have, as the loader last saw it. An answer is settled only if this hasn't moved.
#[derive(PartialEq)]
struct View {
    applied: u64,
    /// The model that answers, ignoring the withdrawal gate: a silent session holds nothing.
    answering: Option<ModelRef>,
    /// A replacement that waits: its serial and model (`None` for "nothing runs").
    pending: Option<(u64, Option<ModelRef>)>,
}

impl View {
    fn of(session: &Shared, mailbox: &Mailbox) -> Self {
        let applied = session.applied.load(Ordering::Acquire);
        let answering = if applied & SILENT == 0 { session.applied_model(&session.records()) } else { None };
        let pending = mailbox
            .ready
            .as_ref()
            .filter(|r| !r.void)
            .map(|r| (r.serial, r.replacement.as_ref().map(|a| a.model.clone())));
        Self { applied, answering, pending }
    }

    fn pending_model(&self) -> Option<&ModelRef> {
        self.pending.as_ref().and_then(|(_, m)| m.as_ref())
    }
}

/// What the loader decided.
enum Answer {
    /// This replacement (`None`: nothing runs), reset to this count (read just before it is reset).
    Publish(Option<Active>, u64),
    /// The model already held is the best that can run.
    Confirm(ModelRef),
    /// Nothing can run, and nothing is held: there is nothing to say.
    Nothing,
}

/// Settles what `session` should run and publishes it. Nothing it loads is published if the registry or the run side
/// moved on meanwhile: it chooses again, keeping the session it loaded in case it is still the choice.
///
/// A replacement whose runtime panics while it is reset counts as a load failure: it is recorded, dropped and skipped
/// for the rest of this pass, so the chain goes on to the next model. While a model that failed in `run` hasn't been
/// reported yet, nothing is chosen.
fn choose(registry: &Registry, session: &Shared, mut in_hand: Option<Active>, graveyard: &mut Vec<Box<dyn Session>>) {
    let mut failed: Vec<ModelRef> = Vec::new();
    loop {
        let view = View::of(session, &session.mailbox());
        // Silent with a usable model applied: that model failed in `run`, and its report hasn't reached the registry
        // (it is still on the run side, or in the mailbox since this pass collected it). Choosing now would load it
        // again, or publish a fallback the health meter can't explain yet. Nothing is lost by waiting: a report in the
        // mailbox has asked for another pass, and one still on the run side is handed over, with that ask, by the next
        // `run`, the only thing that applies an answer anyway.
        // The model is read first: the records' lock must not be held while the registry's is taken, the order a
        // commit takes them in (`Registry::changed`, then `Shared::withdraw`).
        let failed_unreported = view.applied & SILENT != 0 && {
            let applied = session.applied_model(&session.records());
            applied.is_some_and(|m| registry.is_usable(&m))
        };
        if failed_unreported {
            graveyard.extend(in_hand.take().map(|a| a.session));
            return;
        }
        let held: Vec<ModelRef> = [in_hand.as_ref().map(|a| &a.model), view.pending_model(), view.answering.as_ref()]
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let (generation, chosen) = registry.load_chain(&session.slot, session.explicit.as_ref(), &held, &failed);
        let mut answer = match chosen {
            Ok(Chosen::Keep(model)) if in_hand.as_ref().is_some_and(|a| a.model == model) => {
                Answer::Publish(in_hand.take(), 0)
            }
            Ok(Chosen::Keep(model)) => Answer::Confirm(model),
            Ok(Chosen::Loaded { session, model }) => Answer::Publish(Some(Active { session, model }), 0),
            Err(e) if view.answering.is_none() && view.pending.is_none() => {
                tracing::debug!(slot = %session.slot, "still no model can run: {e}");
                Answer::Nothing
            }
            Err(e) => {
                tracing::warn!(slot = %session.slot, "no model can run: {e}");
                Answer::Publish(None, 0)
            }
        };
        graveyard.extend(in_hand.take().map(|a| a.session));
        if let Answer::Publish(replacement, resets) = &mut answer {
            // Read before the reset: a `reset` after this moves the count on, and `run` won't apply this answer.
            *resets = session.resets.load(Ordering::Acquire);
            if let Some(active) = replacement.as_mut()
                && catch_unwind(AssertUnwindSafe(|| active.session.reset())).is_err()
            {
                tracing::warn!(slot = %session.slot, model = %active.model, "{RESET_PANICKED}");
                registry.record_load_failure(&active.model, RESET_PANICKED.to_owned());
                failed.push(active.model.clone());
                graveyard.extend(replacement.take().map(|a| a.session));
                continue;
            }
        }
        match settle(registry, session, &view, generation, answer) {
            Ok(superseded) => {
                graveyard.extend(superseded);
                return;
            }
            Err(back) => in_hand = back,
        }
    }
}

/// Puts `answer` in the mailbox if `view` and `generation` still hold. Returns what it displaced, to drop off the
/// lock; or, if they moved on, the replacement it didn't publish.
fn settle(
    registry: &Registry,
    session: &Shared,
    view: &View,
    generation: u64,
    answer: Answer,
) -> Result<Option<Box<dyn Session>>, Option<Active>> {
    let mut mailbox = session.mailbox();
    if registry.generation() != generation || View::of(session, &mailbox) != *view {
        return Err(match answer {
            Answer::Publish(replacement, _) => replacement,
            Answer::Confirm(_) | Answer::Nothing => None,
        });
    }
    #[cfg(feature = "test-support")]
    let published = matches!(answer, Answer::Publish(..));
    let displaced = match answer {
        Answer::Nothing => None,
        Answer::Confirm(model) if view.pending_model() == Some(&model) => {
            if let Some(ready) = mailbox.ready.as_mut() {
                ready.cleared = ready.cleared.max(generation);
            }
            None
        }
        Answer::Confirm(_) => {
            // The applied model is the choice: a replacement that waits isn't any more.
            session.ready.store(false, Ordering::Release);
            session.cleared.fetch_max(generation, Ordering::AcqRel);
            mailbox.ready.take()
        }
        Answer::Publish(replacement, resets) => {
            let serial = mailbox.next_serial;
            mailbox.next_serial += 1;
            record(session, serial, replacement.as_ref());
            let ready = Ready { replacement, serial, cleared: generation, resets, void: false };
            let displaced = mailbox.ready.replace(ready);
            session.ready_resets.store(resets, Ordering::Release);
            session.ready.store(true, Ordering::Release);
            displaced
        }
    };
    drop(mailbox);
    #[cfg(feature = "test-support")]
    if published {
        registry.count_publication();
    }
    Ok(displaced.and_then(|r| r.replacement).map(|a| a.session))
}

/// Records what answer `serial` will run, and forgets the answers that are neither applied nor this one. Called under
/// the mailbox lock, so the run side can't apply anything meanwhile.
fn record(session: &Shared, serial: u64, replacement: Option<&Active>) {
    let applied = session.applied.load(Ordering::Acquire) >> 1;
    let mut records = session.records();
    if let Some(info) = records.list.iter().find(|r| r.serial == applied).and_then(|r| r.info.clone()) {
        records.last = info;
    }
    records.list.retain(|r| r.serial == applied);
    records.list.push(Record { serial, info: replacement.map(|a| a.session.info()) });
}
