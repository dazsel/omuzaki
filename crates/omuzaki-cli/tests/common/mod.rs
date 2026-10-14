//! Helpers shared by the integration tests: snapshot comparison, a fake [`Runner`] and a temporary project.
//!
//! Snapshots are plain files in `tests/snapshots/`. Run with `UPDATE_SNAPSHOTS=1` to rewrite them after an intended
//! change, then read the diff: the snapshot *is* the CLI's documented output.
// Test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request, Runner, TxId};

/// Compares `actual` with `tests/snapshots/<name>`.
pub fn snapshot(name: &str, actual: &str) {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "snapshots", name].iter().collect();
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, actual).expect("write snapshot");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("missing snapshot {name} ({e}); run with UPDATE_SNAPSHOTS=1\n--- actual ---\n{actual}")
    });
    assert_eq!(actual, expected, "snapshot {name} differs; run with UPDATE_SNAPSHOTS=1 if the change is intended");
}

/// A fresh project folder named `demo`, in a temporary directory that goes away with the returned guard.
pub fn new_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    omuzaki_doc::Project::create(dir.path(), "demo").expect("create the project");
    dir
}

/// The text of the project's `project.toml`.
pub fn project_toml(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("project.toml")).expect("read project.toml")
}

/// A runner that answers with fixed results and remembers what it was asked.
pub struct FakeRunner {
    /// What `run` returns.
    pub outcome: Result<Outcome, OpError>,
    /// What `undo` and `redo` return; tests change it between commands.
    pub history: Mutex<Result<Option<String>, OpError>>,
    /// What `ops` returns.
    pub ops: Vec<OpInfo>,
    /// Every request `run` received.
    pub requests: Mutex<Vec<Request>>,
}

impl FakeRunner {
    /// A runner whose `run` returns `outcome`. Shared, because `execute` takes the runner as an `Arc` and the test
    /// reads the requests afterwards.
    pub fn answering(outcome: Outcome) -> Arc<Self> {
        Arc::new(Self {
            outcome: Ok(outcome),
            history: Mutex::new(Ok(None)),
            ops: Vec::new(),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// A runner that refuses everything with `error`.
    pub fn refusing(error: OpError) -> Arc<Self> {
        Arc::new(Self {
            outcome: Err(error.clone()),
            history: Mutex::new(Err(error)),
            ops: Vec::new(),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// Sets what `undo` and `redo` return from now on.
    pub fn set_history(&self, history: Result<Option<String>, OpError>) {
        *self.history.lock().unwrap() = history;
    }

    /// The only request received.
    pub fn only_request(&self) -> Request {
        let seen = self.requests.lock().unwrap();
        assert_eq!(seen.len(), 1, "expected exactly one request, got {seen:?}");
        seen[0].clone()
    }
}

impl Runner for FakeRunner {
    fn ops(&self) -> Vec<OpInfo> {
        self.ops.clone()
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        self.requests.lock().unwrap().push(req);
        self.outcome.clone()
    }

    fn begin(&self, _label: &str, _actor: &Actor) -> TxId {
        TxId(1)
    }

    fn commit(&self, _tx: TxId, _actor: &Actor) -> Result<(), OpError> {
        Ok(())
    }

    fn rollback(&self, _tx: TxId, _actor: &Actor) -> Result<(), OpError> {
        Ok(())
    }

    fn undo(&self, _actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        self.history.lock().unwrap().clone()
    }

    fn redo(&self, _actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        self.history.lock().unwrap().clone()
    }

    fn subscribe(&self, _grants: &Grants) -> Result<Receiver<Event>, OpError> {
        Ok(mpsc::channel().1)
    }

    fn get(&self, _path: &DocPath, _grants: &Grants) -> Result<Option<Value>, OpError> {
        Ok(None)
    }

    fn log(&self, _after_seq: u64, _limit: usize, _grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        Ok(Vec::new())
    }
}
