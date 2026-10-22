//! Crash recovery, end to end (VC-08; `docs/design/journal.md` section 12, item 3): a seeded session of edits,
//! transactions, undos and redos driven through `omuzaki mcp`, killed with SIGKILL at a seeded point, then relaunched.
//! The recovered document must be the oracle's: every acknowledged change kept, a transaction left open rolled back.
//! Then `omuzaki undo` in a fresh process must undo the newest kept step.
//!
//! Three kinds of crash, chosen by the seed: a kill right after an acknowledgement; a kill with a request in flight
//! (either outcome is right, as long as the rest agrees with it); and a kill followed by a torn, garbled tail on the
//! newest journal segment, as a power cut can leave. A kill keeps the page cache, so it can't see a missing flush:
//! the simulated power-loss harness (#366) covers that.
//!
//! Each pull request runs three seeds; `OMUZAKI_CRASH_SEEDS=20` runs twenty (the nightly job), and
//! `OMUZAKI_CRASH_SEED=<n>` runs one, to reproduce a failure. `OMUZAKI_CRASH_RECOVER=disk` relaunches with
//! `--recover disk` instead, which skips recovery: the test then fails, which shows it can see a loss.

// `CARGO_BIN_EXE_omuzaki-fixture` exists only when the fixture is built (`test-fixtures` feature, see Cargo.toml).
#![cfg(feature = "test-fixtures")]
// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const FIXTURE: &str = env!("CARGO_BIN_EXE_omuzaki-fixture");

/// Longest any answer or child process may take. Generous: a loaded CI machine is slow, a hang is infinite.
const LIMIT: Duration = Duration::from_secs(60);

/// Steps in a recorded session; the kill comes somewhere inside it.
const SESSION: usize = 1_000;

const FIELDS: [&str; 4] = ["a", "b", "c", "d"];

/// The demo fields of a document: field name to text.
type Fields = BTreeMap<String, String>;

/// A small deterministic generator (`SplitMix64`), so a seed names the same session on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n`.
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// One step an agent takes.
#[derive(Clone, Debug)]
enum Step {
    Set { field: &'static str, text: String },
    Preview { field: &'static str, text: String },
    Begin { label: String },
    Commit,
    Rollback,
    Undo,
    Redo,
}

impl Step {
    fn call(&self) -> (&'static str, Value) {
        match self {
            Self::Set { field, text } => ("demo_set_field", json!({ "field": field, "text": text })),
            Self::Preview { field, text } => {
                ("demo_set_field", json!({ "field": field, "text": text, "dry_run": true }))
            }
            Self::Begin { label } => ("tx_begin", json!({ "label": label })),
            Self::Commit => ("tx_commit", json!({})),
            Self::Rollback => ("tx_rollback", json!({})),
            Self::Undo => ("history_undo", json!({})),
            Self::Redo => ("history_redo", json!({})),
        }
    }
}

/// What the project should hold: the fields now, the undo and redo stacks (the fields before and after each step), and
/// the fields when the open transaction began.
#[derive(Clone, Debug, Default, PartialEq)]
struct Model {
    fields: Fields,
    undo: Vec<Fields>,
    redo: Vec<Fields>,
    open: Option<Fields>,
}

impl Model {
    fn apply(&mut self, step: &Step) {
        match step {
            Step::Set { field, text } => {
                if self.open.is_none() {
                    self.undo.push(self.fields.clone());
                    self.redo.clear();
                }
                self.fields.insert((*field).to_owned(), text.clone());
            }
            Step::Preview { .. } => {}
            Step::Begin { .. } => self.open = Some(self.fields.clone()),
            Step::Commit => {
                let before = self.open.take().unwrap();
                // A transaction that changed nothing leaves no step.
                if before != self.fields {
                    self.undo.push(before);
                    self.redo.clear();
                }
            }
            Step::Rollback => self.fields = self.open.take().unwrap(),
            Step::Undo => {
                let before = self.undo.pop().unwrap();
                self.redo.push(std::mem::replace(&mut self.fields, before));
            }
            Step::Redo => {
                let after = self.redo.pop().unwrap();
                self.undo.push(std::mem::replace(&mut self.fields, after));
            }
        }
    }

    /// The model as recovery leaves it: an open transaction rolled back.
    fn recovered(&self) -> Self {
        let mut model = self.clone();
        if let Some(before) = model.open.take() {
            model.fields = before;
        }
        model
    }

    /// The fields after a person's undo of the newest step, and whether there was one to undo.
    fn after_undo(&self) -> (Fields, bool) {
        self.undo.last().map_or_else(|| (self.fields.clone(), false), |before| (before.clone(), true))
    }
}

/// A seeded session of [`SESSION`] steps that are all valid in order: transactions of one to five edits, committed
/// or (now and then) rolled back, previews, undos and redos outside transactions. Every text is new, so no edit is a
/// no-op.
fn session(seed: u64) -> Vec<Step> {
    let mut rng = Rng(seed);
    let mut model = Model::default();
    let mut steps = Vec::with_capacity(SESSION);
    let mut left_in_tx = 0;
    while steps.len() < SESSION {
        let n = steps.len();
        let field = FIELDS[rng.below(FIELDS.len())];
        let text = format!("s{seed}-{n}");
        let step = if model.open.is_some() {
            if left_in_tx > 0 {
                left_in_tx -= 1;
                Step::Set { field, text }
            } else if rng.chance(15) {
                Step::Rollback
            } else {
                Step::Commit
            }
        } else {
            match rng.below(100) {
                45..55 => Step::Preview { field, text },
                55..70 => {
                    left_in_tx = 1 + rng.below(5);
                    Step::Begin { label: format!("group {n}") }
                }
                70..85 if !model.undo.is_empty() => Step::Undo,
                85..100 if !model.redo.is_empty() => Step::Redo,
                // Below 45, and an undo or redo with nothing to take.
                _ => Step::Set { field, text },
            }
        };
        model.apply(&step);
        steps.push(step);
    }
    steps
}

/// A running `omuzaki mcp`, spoken to in newline-delimited JSON-RPC. Killed when dropped.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn start(project: &Path) -> Self {
        let mut child = Command::new(FIXTURE)
            .arg("--project")
            .arg(project)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn omuzaki mcp");
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut mcp = Self { child, stdin, lines, next_id: 1 };
        let client = json!({"name": "crash-recovery", "version": "0"});
        mcp.request("initialize", &json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": client}));
        mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        mcp
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").expect("write to the server");
        self.stdin.flush().expect("flush");
    }

    /// Sends a request without waiting for its answer; returns its id.
    fn post(&mut self, method: &str, params: &Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// Sends a request and returns its result, skipping notifications.
    fn request(&mut self, method: &str, params: &Value) -> Value {
        let id = self.post(method, params);
        loop {
            let line = self.lines.recv_timeout(LIMIT).expect("the server answers in time");
            let message: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("stdout line {line:?}: {e}"));
            if message["id"] == json!(id) {
                assert!(message.get("error").is_none(), "{method} failed: {message}");
                return message["result"].clone();
            }
        }
    }

    /// Takes `step`; it must succeed. Returns once the server has acknowledged it, which it does once the journal
    /// holds it. A session's edit rate is limited (600 a minute after a burst of 100); a refusal for that is waited
    /// out and the step taken again, as the error's hint tells an agent to.
    fn take(&mut self, step: &Step) {
        let (tool, arguments) = step.call();
        loop {
            let result = self.request("tools/call", &json!({"name": tool, "arguments": arguments}));
            let refusal = &result["structuredContent"];
            if result["isError"] == json!(true) && refusal["code"] == "limited" {
                let wait = refusal["detail"]["seconds"].as_f64().unwrap_or(0.1);
                thread::sleep(Duration::from_secs_f64(wait.clamp(0.001, 10.0)));
                continue;
            }
            assert_ne!(result["isError"], json!(true), "{step:?} failed: {result}");
            return;
        }
    }

    /// SIGKILL (`TerminateProcess` on Windows), and reaped.
    fn kill(mut self) {
        self.child.kill().expect("kill omuzaki mcp");
        self.child.wait().expect("reap omuzaki mcp");
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A command that ran to the end.
struct Done {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn fixture(project: &Path, args: &[&str]) -> Done {
    let mut child = Command::new(FIXTURE)
        .arg("--project")
        .arg(project)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn omuzaki");
    let began = Instant::now();
    loop {
        if child.try_wait().expect("poll the child").is_some() {
            break;
        }
        assert!(began.elapsed() < LIMIT, "omuzaki {args:?} overstayed {LIMIT:?}");
        thread::park_timeout(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("collect the output");
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("utf-8 output");
    Done { code: output.status.code(), stdout: text(output.stdout), stderr: text(output.stderr) }
}

/// The demo fields of the project, read by a fresh process (which recovers the project if it needs to).
fn fields(project: &Path, recover: &str) -> (Fields, String) {
    let read = fixture(project, &["--recover", recover, "--json", "run", "demo.get_project"]);
    assert_eq!(read.code, Some(0), "reading the project failed: {}", read.stderr);
    let outcome: Value = serde_json::from_str(&read.stdout).expect("--json output");
    let table = outcome["plan"]["output"]["project"].as_object().cloned().unwrap_or_default();
    let fields = table
        .iter()
        .filter_map(|(key, value)| Some((key.strip_prefix("field_")?.to_owned(), value.as_str()?.to_owned())))
        .collect();
    (fields, read.stderr)
}

/// The newest segment of the live journal.
fn newest_segment(project: &Path) -> PathBuf {
    let dir = project.join(".omuzaki").join("journal");
    let mut segments: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the journal folder")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "tjl"))
        .collect();
    segments.sort();
    segments.pop().expect("a journal segment")
}

/// What a power cut can leave after the last flushed record: part of a frame that was being written, then garbage.
fn tear_the_tail(project: &Path, rng: &mut Rng) {
    let segment = newest_segment(project);
    let mut tail = vec![0u8; 16 + rng.below(4096)];
    for byte in &mut tail {
        *byte = rng.next().to_le_bytes()[0];
    }
    let mut file = std::fs::OpenOptions::new().append(true).open(&segment).expect("open the segment");
    file.write_all(&tail).expect("garble the tail");
}

/// How a seed's session ends.
#[derive(Clone, Copy, Debug)]
enum Crash {
    AfterAck,
    InFlight,
    TornTail,
}

/// Runs one seed: the session up to its kill, the relaunch and the comparison, then the undo.
fn run_seed(seed: u64) {
    let recover = std::env::var("OMUZAKI_CRASH_RECOVER").unwrap_or_else(|_| "restore".to_owned());
    let steps = session(seed);
    let mut rng = Rng(seed ^ 0x5EED);
    let crash = [Crash::AfterAck, Crash::InFlight, Crash::TornTail][usize::try_from(seed % 3).unwrap()];
    let acked = 1 + rng.below(SESSION - 1);
    let project = common::new_project();

    let mut mcp = Mcp::start(project.path());
    let mut model = Model::default();
    for step in &steps[..acked] {
        mcp.take(step);
        model.apply(step);
    }
    // The candidates for what the journal holds: what was acknowledged, and for a request in flight, that as well.
    let mut candidates = vec![model.recovered()];
    match crash {
        Crash::AfterAck => mcp.kill(),
        Crash::TornTail => {
            mcp.kill();
            tear_the_tail(project.path(), &mut rng);
        }
        Crash::InFlight => {
            let next = &steps[acked];
            let (tool, arguments) = next.call();
            mcp.post("tools/call", &json!({"name": tool, "arguments": arguments}));
            // Give the request a seeded head start, from none to a few milliseconds, so the kill lands at different
            // points of its way through the server.
            let head_start = Duration::from_micros(rng.below(4_000) as u64);
            let began = Instant::now();
            while began.elapsed() < head_start {
                std::hint::spin_loop();
            }
            mcp.kill();
            let mut after = model.clone();
            after.apply(next);
            candidates.push(after.recovered());
        }
    }

    let context = format!("seed {seed}, {crash:?} after step {acked} of {SESSION} (`OMUZAKI_CRASH_SEED={seed}`)");
    let (recovered, notice) = fields(project.path(), &recover);
    let matching: Vec<&Model> = candidates.iter().filter(|m| m.fields == recovered).collect();
    assert!(
        !matching.is_empty(),
        "{context}: the recovered fields {recovered:?} are none of what the journal can hold: {:?}\nnotice: {notice}",
        candidates.iter().map(|m| &m.fields).collect::<Vec<_>>()
    );

    // A person's undo in a new process takes the newest kept step, and the files follow.
    let undone = fixture(project.path(), &["undo"]);
    assert_eq!(undone.code, Some(0), "{context}: undo failed: {}", undone.stderr);
    let (after_undo, _) = fields(project.path(), &recover);
    assert!(
        matching.iter().any(|m| {
            let (fields, any) = m.after_undo();
            fields == after_undo && any == undone.stdout.starts_with("undid ")
        }),
        "{context}: after `omuzaki undo` ({}) the fields are {after_undo:?}; expected one of {:?}",
        undone.stdout.trim(),
        matching.iter().map(|m| m.after_undo()).collect::<Vec<_>>()
    );
}

/// The seeds to run: `OMUZAKI_CRASH_SEED` alone, or the first `OMUZAKI_CRASH_SEEDS` (3 by default).
fn seeds() -> Vec<u64> {
    if let Some(seed) = std::env::var("OMUZAKI_CRASH_SEED").ok().and_then(|s| s.parse().ok()) {
        return vec![seed];
    }
    let count = std::env::var("OMUZAKI_CRASH_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(3u64);
    (1..=count).collect()
}

#[test]
fn a_killed_session_reopens_at_its_last_acknowledged_change_and_undo_takes_the_newest_step() {
    // Seeds run side by side: each has its own project and server, and most of a session's time is spent waiting out
    // the edit rate limit, not working.
    let seeds = seeds();
    let parallel = std::env::var("OMUZAKI_CRASH_PARALLEL").ok().and_then(|s| s.parse().ok()).unwrap_or(10usize);
    for batch in seeds.chunks(parallel.max(1)) {
        thread::scope(|scope| {
            for &seed in batch {
                scope.spawn(move || {
                    let began = Instant::now();
                    run_seed(seed);
                    println!("crash recovery: seed {seed} passed in {:.1}s", began.elapsed().as_secs_f64());
                });
            }
        });
    }
}

#[test]
fn sessions_are_valid_and_seeded() {
    let steps = session(7);
    assert_eq!(steps.len(), SESSION);
    assert_eq!(format!("{steps:?}"), format!("{:?}", session(7)), "a seed names one session");
    assert_ne!(format!("{steps:?}"), format!("{:?}", session(8)));
    let kinds = |pick: fn(&Step) -> bool| steps.iter().filter(|s| pick(s)).count();
    assert!(kinds(|s| matches!(s, Step::Begin { .. })) > 50, "transactions are common");
    assert!(kinds(|s| matches!(s, Step::Undo)) > 20 && kinds(|s| matches!(s, Step::Redo)) > 5);
    assert!(kinds(|s| matches!(s, Step::Rollback)) > 0);
}
