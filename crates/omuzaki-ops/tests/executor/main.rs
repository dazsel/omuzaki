//! Behaviour tests for the executor, through the public `Runner` interface only, against the in-memory document.
//! One test binary: the ops in `ops.rs` register themselves with the linker like real ones do.

// Test helpers may panic: a panic is a clear failure (clippy.toml exempts only `#[test]` functions).
// `set` and friends take `json!(..)` temporaries by value, which keeps call sites short.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

mod adversarial;
mod events;
mod explicit;
mod file_access;
mod jobs;
mod ops;
mod props;
mod run;
mod services;
mod transactions;
mod undo;

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocPath;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Actor, Event, Grants, Mode, OpError, OpRecord, Outcome, Request, Runner};
use serde_json::{Value, json};

/// An executor over a `MemDoc`, with the fake clock that stamps its log.
pub(crate) struct Rig {
    pub(crate) exec: Executor,
    pub(crate) clock: Arc<FakeClock>,
}

pub(crate) fn rig(doc: Value) -> Rig {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(doc)), clock.clone()).unwrap();
    Rig { exec, clock }
}

pub(crate) fn path(s: &str) -> DocPath {
    DocPath::parse(s).unwrap()
}

pub(crate) fn person() -> Actor {
    Actor::Person { name: "alice".into() }
}

pub(crate) fn agent() -> Actor {
    Actor::Agent { id: "claude-1".into(), model: None }
}

pub(crate) fn plugin() -> Actor {
    Actor::Plugin { id: "stamper".into() }
}

/// A request to apply `op` as a person who may do anything, outside any transaction.
pub(crate) fn request(op: &str, args: Value) -> Request {
    Request {
        op: op.into(),
        args,
        mode: Mode::Apply,
        actor: person(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    }
}

/// Like [`request`], but run by the agent with the default agent grants (read, edit, render; no publish).
pub(crate) fn agent_request(op: &str, args: Value) -> Request {
    Request { actor: agent(), grants: Grants::agent_default(), ..request(op, args) }
}

impl Rig {
    pub(crate) fn run(&self, op: &str, args: Value) -> Result<Outcome, OpError> {
        self.exec.run(request(op, args))
    }

    /// `test.set` as a person, outside any transaction.
    pub(crate) fn set(&self, at: &str, value: Value) -> Outcome {
        self.run("test.set", json!({"path": at, "value": value})).unwrap()
    }

    /// `test.set` as the agent, outside any transaction.
    pub(crate) fn agent_set(&self, at: &str, value: Value) -> Outcome {
        self.exec.run(agent_request("test.set", json!({"path": at, "value": value}))).unwrap()
    }

    pub(crate) fn at(&self, p: &str) -> Option<Value> {
        self.exec.get(&path(p), &Grants::person()).unwrap()
    }

    /// A new event stream, as a person.
    pub(crate) fn subscribe(&self) -> Receiver<Event> {
        self.exec.subscribe(&Grants::person()).unwrap()
    }

    /// The whole op log, as a person.
    pub(crate) fn log(&self) -> Vec<OpRecord> {
        self.exec.log(0, usize::MAX, &Grants::person()).unwrap()
    }

    pub(crate) fn agent_undo(&self) -> Result<Option<String>, OpError> {
        self.exec.undo(&agent(), &Grants::agent_default())
    }

    pub(crate) fn agent_redo(&self) -> Result<Option<String>, OpError> {
        self.exec.redo(&agent(), &Grants::agent_default())
    }

    pub(crate) fn undo(&self) -> Result<Option<String>, OpError> {
        self.exec.undo(&person(), &Grants::person())
    }

    pub(crate) fn redo(&self) -> Result<Option<String>, OpError> {
        self.exec.redo(&person(), &Grants::person())
    }
}

/// Everything waiting on `rx`.
pub(crate) fn drain(rx: &Receiver<Event>) -> Vec<Event> {
    rx.try_iter().collect()
}
