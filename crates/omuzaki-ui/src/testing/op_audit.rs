//! The op audit (QG-11): every change a panel makes to the document arrives as an op the studio's runner ran, and is
//! in the person's undo history.
//!
//! [`Recorder`] stands between the shell and the real executor. It passes every call through, notes the log record of
//! each op the UI applied, and listens to the executor's events. After an interaction, [`Recorder::check`] compares the
//! document with how it was before:
//!
//! - an op applied that the UI's runner never ran came through a back door (a runner of its own, kept by a panel);
//! - a change with no op at all came through a back door into the document;
//! - the steps the interaction made are undone and redone through the person's history, and must take the document
//!   back to where it was and forward again exactly.
//!
//! The executor already checks every request against its op's schema before it applies it, so an op applied is a
//! schema-valid request.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, PoisonError};

use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request, Runner, TxId};

/// A [`Runner`] that passes every call to the executor it wraps and notes what the UI applied through it.
pub struct Recorder {
    inner: Arc<dyn Runner>,
    events: Mutex<Receiver<Event>>,
    /// The log positions of the ops applied through this runner since the last check.
    ran: Mutex<BTreeSet<u64>>,
    /// Undos, redos and rollbacks asked through this runner since the last check: their own events are expected.
    stepped: Mutex<usize>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder").finish_non_exhaustive()
    }
}

/// The person the studio acts as, whose history the audit undoes.
fn person() -> Actor {
    Actor::Person { name: "you".to_owned() }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Recorder {
    /// Wraps `inner`, listening to its events from now on.
    ///
    /// # Errors
    /// The executor refused the subscription.
    pub fn new(inner: Arc<dyn Runner>) -> Result<Arc<Self>, OpError> {
        let events = inner.subscribe(&Grants::person())?;
        Ok(Arc::new(Self {
            inner,
            events: Mutex::new(events),
            ran: Mutex::new(BTreeSet::new()),
            stepped: Mutex::new(0),
        }))
    }

    /// The whole document as the executor holds it.
    pub fn snapshot(&self) -> Value {
        self.inner.get(&DocPath::root(), &Grants::person()).ok().flatten().unwrap_or(Value::Null)
    }

    /// Forgets what happened so far, for a fresh start before the first interaction.
    pub fn settle(&self) {
        drop(self.drain());
        lock(&self.ran).clear();
        *lock(&self.stepped) = 0;
    }

    fn drain(&self) -> Vec<Event> {
        lock(&self.events).try_iter().collect()
    }

    /// What is wrong with the interaction called `what`, which started from the document `before`: each problem names
    /// it. Undoes and redoes its steps to check them, and leaves the document as the interaction left it.
    pub fn check(&self, before: &Value, what: &str) -> Vec<String> {
        let events = self.drain();
        let ran = std::mem::take(&mut *lock(&self.ran));
        let stepped = std::mem::take(&mut *lock(&self.stepped));
        let after = self.snapshot();
        let applied: Vec<&OpRecord> = events
            .iter()
            .filter_map(|event| match event {
                Event::OpApplied { record, .. } => Some(record),
                _ => None,
            })
            .collect();
        let mut problems: Vec<String> = applied
            .iter()
            .filter(|record| !ran.contains(&record.seq))
            .map(|record| format!("{what}: {} changed the document outside the studio's runner", record.op))
            .collect();
        if after == *before {
            return problems;
        }
        if applied.is_empty() && stepped == 0 {
            problems.push(format!("{what}: the document changed with no op"));
            return problems;
        }
        // An undo or redo the interaction asked for is a step back or forth already, not a new one to undo.
        if stepped > 0 || !problems.is_empty() {
            return problems;
        }
        let steps: BTreeSet<TxId> =
            applied.iter().filter(|r| r.actor == person() && !r.patch.changes.is_empty()).map(|r| r.tx).collect();
        let ops: Vec<&str> = applied.iter().map(|r| r.op.as_str()).collect();
        let grants = Grants::person();
        for _ in 0..steps.len() {
            if let Err(e) = self.inner.undo(&person(), &grants) {
                problems.push(format!("{what}: undoing {ops:?} failed: {}", e.message));
            }
        }
        if self.snapshot() != *before {
            problems
                .push(format!("{what}: undoing its {} step(s) of {ops:?} does not restore the document", steps.len()));
        }
        for _ in 0..steps.len() {
            if let Err(e) = self.inner.redo(&person(), &grants) {
                problems.push(format!("{what}: redoing {ops:?} failed: {}", e.message));
            }
        }
        if self.snapshot() != after {
            problems.push(format!("{what}: redoing {ops:?} does not bring its change back"));
        }
        drop(self.drain());
        problems
    }
}

impl Runner for Recorder {
    fn ops(&self) -> Vec<OpInfo> {
        self.inner.ops()
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        let outcome = self.inner.run(req)?;
        if let Some(record) = &outcome.applied {
            lock(&self.ran).insert(record.seq);
        }
        Ok(outcome)
    }

    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.inner.begin(label, actor)
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.commit(tx, actor)
    }

    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        *lock(&self.stepped) += 1;
        self.inner.rollback(tx, actor)
    }

    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        *lock(&self.stepped) += 1;
        self.inner.undo(actor, grants)
    }

    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        *lock(&self.stepped) += 1;
        self.inner.redo(actor, grants)
    }

    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        self.inner.subscribe(grants)
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.inner.get(path, grants)
    }

    fn log(&self, since: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.inner.log(since, limit, grants)
    }
}
