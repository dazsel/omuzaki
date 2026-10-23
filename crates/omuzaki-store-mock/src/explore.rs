//! Exhaustive interleaving exploration for store tests: a few actors (writers, a sweeper, a clock) run on their own
//! threads, but only one runs at a time, and the explorer decides which one goes next at every stop. Running again with
//! every different decision visits every interleaving of the actors' steps, so a race that needs a particular order is
//! found by construction instead of by one round of review at a time.
//!
//! An actor reaches a stop by calling [`Stepper::stop`] (its store's failpoints, or [`SteppedStore`]'s wrappers, do
//! this). Locks are modelled, not waited for: [`Stepper::acquire`] and [`Stepper::release`] tell the explorer when an
//! actor takes or drops the store lock, and the explorer only lets an actor go on once the lock is free for it. That
//! keeps every run deterministic: nothing ever blocks for real while others are parked.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

use omuzaki_store_api::{ContentHash, Provenance, Removal, Store, StoreError, StoreGc};

/// How long the explorer waits for a running actor to reach its next stop before it gives up and fails the run. An
/// actor that blocks for real (instead of telling the explorer about a lock) would otherwise hang the test.
const STALL_LIMIT: Duration = Duration::from_secs(30);

/// How a lock is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Any number of holders at once.
    Shared,
    /// A single holder, and no shared ones.
    Exclusive,
}

/// Who holds the modelled lock.
#[derive(Debug, Default)]
struct Locks {
    shared: usize,
    exclusive: bool,
}

impl Locks {
    fn allows(&self, mode: LockMode) -> bool {
        match mode {
            LockMode::Shared => !self.exclusive,
            LockMode::Exclusive => !self.exclusive && self.shared == 0,
        }
    }

    fn take(&mut self, mode: LockMode) {
        match mode {
            LockMode::Shared => self.shared += 1,
            LockMode::Exclusive => self.exclusive = true,
        }
    }

    fn release(&mut self, mode: LockMode) {
        match mode {
            LockMode::Shared => self.shared = self.shared.saturating_sub(1),
            LockMode::Exclusive => self.exclusive = false,
        }
    }
}

/// What an actor tells the explorer.
enum Event {
    Stopped(usize),
    Wants(usize, LockMode),
    Done(usize),
}

/// An actor's handle to the explorer. Cheap to clone.
#[derive(Clone)]
pub struct Stepper {
    id: usize,
    events: Sender<Event>,
    go: Arc<Mutex<Receiver<()>>>,
    locks: Arc<Mutex<Locks>>,
}

impl Stepper {
    /// Parks the actor until the explorer picks it to go on.
    pub fn stop(&self) {
        let _ = self.events.send(Event::Stopped(self.id));
        self.wait();
    }

    /// Announces that the actor is about to take the lock in `mode`, and parks it until the explorer both picks it and
    /// finds the lock free for it. The actor then holds the lock until it calls [`Stepper::release`].
    pub fn acquire(&self, mode: LockMode) {
        let _ = self.events.send(Event::Wants(self.id, mode));
        self.wait();
    }

    /// Announces that the actor dropped the lock it holds in `mode`. Does not park: the actor goes on running.
    pub fn release(&self, mode: LockMode) {
        self.locks.lock().unwrap_or_else(PoisonError::into_inner).release(mode);
    }

    fn wait(&self) {
        // A closed channel means the explorer is gone (a failed run): just run on.
        let _ = self.go.lock().unwrap_or_else(PoisonError::into_inner).recv();
    }
}

/// Tells the explorer an actor is done, even if it panicked.
struct DoneOnDrop(usize, Sender<Event>);

impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        let _ = self.1.send(Event::Done(self.0));
    }
}

/// One actor: runs on its own thread with its [`Stepper`].
pub type Actor = Box<dyn FnOnce(Stepper) + Send>;

/// One run of a scenario: fresh state, the actors to interleave, and the check to make after all of them are done.
pub struct Scenario {
    /// The actors, in a fixed order (the order is part of how decisions are numbered).
    pub actors: Vec<Actor>,
    /// Called with a description of the schedule once every actor is done; panics if an invariant is violated.
    pub check: Box<dyn FnOnce(&str)>,
}

/// Where an actor is, as the explorer sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// At a stop (or not started): can be picked.
    Parked,
    /// Waiting for the lock: can be picked once the lock is free for it.
    Wants(LockMode),
    Done,
}

/// Runs the actors once, taking `forced` as the first decisions and the first choice after that, and calling
/// `after_step` after each actor step. Returns every decision made as (index chosen, number of choices).
fn run_once(actors: Vec<Actor>, forced: &[usize], after_step: &dyn Fn()) -> Vec<(usize, usize)> {
    let (events, inbox) = channel();
    let locks = Arc::new(Mutex::new(Locks::default()));
    let mut gates = Vec::new();
    let mut threads = Vec::new();
    for (id, actor) in actors.into_iter().enumerate() {
        let (go, wait) = channel();
        gates.push(go);
        let stepper = Stepper { id, events: events.clone(), go: Arc::new(Mutex::new(wait)), locks: Arc::clone(&locks) };
        let done = DoneOnDrop(id, events.clone());
        threads.push(thread::spawn(move || {
            let _done = done;
            stepper.wait();
            actor(stepper);
        }));
    }
    drop(events);

    let mut states = vec![State::Parked; gates.len()];
    let mut decisions = Vec::new();
    loop {
        let enabled: Vec<usize> = states
            .iter()
            .enumerate()
            .filter(|(_, state)| match state {
                State::Parked => true,
                State::Wants(mode) => locks.lock().unwrap_or_else(PoisonError::into_inner).allows(*mode),
                State::Done => false,
            })
            .map(|(id, _)| id)
            .collect();
        if enabled.is_empty() {
            assert!(states.iter().all(|state| *state == State::Done), "deadlock: {states:?}");
            break;
        }
        let pick = forced.get(decisions.len()).copied().unwrap_or(0).min(enabled.len() - 1);
        decisions.push((pick, enabled.len()));
        let id = enabled[pick];
        if let State::Wants(mode) = states[id] {
            locks.lock().unwrap_or_else(PoisonError::into_inner).take(mode);
        }
        let _ = gates[id].send(());
        // Let the actor run to its next stop, lock request or end. It is the only one running.
        loop {
            match inbox.recv_timeout(STALL_LIMIT) {
                Ok(Event::Stopped(who)) if who == id => {
                    states[id] = State::Parked;
                    break;
                }
                Ok(Event::Wants(who, mode)) if who == id => {
                    let mut held = locks.lock().unwrap_or_else(PoisonError::into_inner);
                    if held.allows(mode) {
                        // Free: take it for the actor at once, without a decision.
                        held.take(mode);
                        drop(held);
                        let _ = gates[id].send(());
                    } else {
                        states[id] = State::Wants(mode);
                        break;
                    }
                }
                Ok(Event::Done(who)) if who == id => {
                    states[id] = State::Done;
                    break;
                }
                Ok(_) => unreachable!("only the running actor can speak"),
                Err(RecvTimeoutError::Timeout) => panic!("actor {id} blocked for real instead of stopping"),
                Err(RecvTimeoutError::Disconnected) => panic!("actor {id} vanished"),
            }
        }
        after_step();
    }
    drop(gates);
    for thread in threads {
        if let Err(panic) = thread.join() {
            std::panic::resume_unwind(panic);
        }
    }
    decisions
}

/// The decisions for the next schedule in depth-first order, or `None` once every schedule has been run.
fn next_schedule(decisions: &[(usize, usize)]) -> Option<Vec<usize>> {
    let depth = decisions.iter().rposition(|(pick, choices)| pick + 1 < *choices)?;
    let mut next: Vec<usize> = decisions[..depth].iter().map(|(pick, _)| *pick).collect();
    next.push(decisions[depth].0 + 1);
    Some(next)
}

/// Runs the scenario under every interleaving of its actors' steps and returns how many schedules that was. Each run
/// builds a fresh scenario with `make`. See [`explore_timed`] to let time pass as the steps go.
///
/// # Panics
/// If a check fails (the message names the schedule), if an actor panics, or if there are more than `max_schedules`
/// schedules: the bound keeps a scenario from quietly growing too slow to run.
pub fn explore(max_schedules: usize, make: &dyn Fn() -> Scenario) -> usize {
    explore_timed(max_schedules, make, &|| {})
}

/// Like [`explore`], and calls `after_step` after every actor step (a step is a decision of the explorer), so a test
/// can advance a clock with each step: the longer a schedule, the later it ends, and an actor kept waiting while
/// others take many steps sees time pass.
///
/// # Panics
/// As [`explore`].
pub fn explore_timed(max_schedules: usize, make: &dyn Fn() -> Scenario, after_step: &dyn Fn()) -> usize {
    let mut forced = Vec::new();
    let mut explored = 0;
    loop {
        let scenario = make();
        let decisions = run_once(scenario.actors, &forced, after_step);
        let schedule = format!("{:?}", decisions.iter().map(|(pick, _)| *pick).collect::<Vec<_>>());
        (scenario.check)(&schedule);
        explored += 1;
        assert!(explored <= max_schedules, "more than {max_schedules} schedules: shrink the scenario");
        match next_schedule(&decisions) {
            Some(next) => forced = next,
            None => return explored,
        }
    }
}

/// A store whose operations stop at the explorer first, so the interleavings of whole operations can be explored for
/// a store (like the in-memory one) that has no failpoints of its own. One wrapper per actor, over a shared store.
pub struct SteppedStore<S> {
    inner: Arc<S>,
    stepper: Stepper,
}

impl<S> SteppedStore<S> {
    /// Wraps `inner` for the actor that owns `stepper`.
    pub fn new(inner: Arc<S>, stepper: Stepper) -> Self {
        Self { inner, stepper }
    }
}

impl<S: Store> Store for SteppedStore<S> {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        self.stepper.stop();
        self.inner.put(bytes, provenance)
    }

    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        self.inner.get(hash)
    }

    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        self.inner.provenance(hash)
    }

    fn contains(&self, hash: &ContentHash) -> bool {
        self.inner.contains(hash)
    }

    fn size(&self, hash: &ContentHash) -> Result<u64, StoreError> {
        self.inner.size(hash)
    }
}

impl<S: StoreGc> StoreGc for SteppedStore<S> {
    fn hashes(&self) -> Result<Vec<ContentHash>, StoreError> {
        self.inner.hashes()
    }

    fn stored_at(&self, hash: &ContentHash) -> Result<SystemTime, StoreError> {
        self.stepper.stop();
        self.inner.stored_at(hash)
    }

    fn remove_if_older(&self, hash: &ContentHash, cutoff: SystemTime) -> Result<Removal, StoreError> {
        self.stepper.stop();
        self.inner.remove_if_older(hash, cutoff)
    }

    fn purge_retired(&self) -> Result<usize, StoreError> {
        self.inner.purge_retired()
    }
}
