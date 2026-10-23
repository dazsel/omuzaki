//! The journal writer's budgets (journal.md 13, #255), measured on the real file system of the machine that runs it:
//!
//! - `append` (the executor's hand-off, under its lock) with the writer busy: at most 0.5 µs median, 2 µs p99;
//! - throughput of 1 KiB records with every batch flushed: at least 20,000 records a second;
//! - apply to durable at 60 commits a second (each waited on with `sync`): p50 at most 2 ms, p99 at most 10 ms on ext4.
//!
//! Run with `cargo bench -p omuzaki-doc --bench journal` in a folder on the disk to measure (the temporary folder is
//! made under the current one). It prints the numbers; it asserts nothing, because only a quiet machine gives them.

// A failed setup should abort the benchmark.
#![allow(clippy::unwrap_used, clippy::print_stdout)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_core::time::Seconds;
use omuzaki_doc::Project;
use omuzaki_doc::journal::{JournalOptions, ProjectJournal, SessionStart, SystemWall, reopen};
use omuzaki_doc::snapshot::Snapshotter;
use omuzaki_history_api::{HistoryEntry, JournalSink};
use omuzaki_ops_api::{Actor, TxId};
use omuzaki_store_mock::MemStore;

const WAIT: Duration = Duration::from_secs(30);

fn journal(dir: &Path) -> Arc<ProjectJournal> {
    static STORES: Mutex<BTreeMap<std::path::PathBuf, Arc<MemStore>>> = Mutex::new(BTreeMap::new());
    let store = STORES.lock().unwrap().entry(dir.to_path_buf()).or_default().clone();
    let snapshots = Arc::new(Snapshotter::new(dir, store));
    let reopened = reopen(dir, &snapshots).unwrap();
    let start =
        SessionStart::new("bench", "0.0.0", reopened.last_lsn, reopened.history, Arc::new(SystemWall), snapshots);
    ProjectJournal::start(reopened.project, start, JournalOptions::default()).unwrap()
}

/// A record of about `bytes` bytes that changes nothing in the document.
fn record(n: u64, bytes: usize) -> HistoryEntry {
    HistoryEntry::Begin {
        tx: TxId(n),
        actor: Actor::Person { name: "bench".into() },
        label: "x".repeat(bytes.saturating_sub(64)),
        at: Seconds(0.0),
    }
}

/// The `per_mille`th of 1,000 of `sorted`.
fn percentile(sorted: &[Duration], per_mille: usize) -> Duration {
    sorted[((sorted.len() - 1) * per_mille / 1000).min(sorted.len() - 1)]
}

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir_in(".").unwrap();
    Project::create(tmp.path(), "bench").unwrap();
    tmp
}

fn append_latency() {
    let tmp = project();
    let journal = journal(tmp.path());
    let mut times = Vec::with_capacity(200_000);
    for n in 0..200_000u64 {
        let entry = record(n, 128);
        let started = Instant::now();
        journal.append(entry);
        times.push(started.elapsed());
    }
    journal.sync(WAIT).unwrap();
    times.sort();
    println!(
        "append, writer busy: median {:?}, p99 {:?} (budget 0.5 µs, 2 µs)",
        percentile(&times, 500),
        percentile(&times, 990)
    );
}

fn throughput() {
    let tmp = project();
    let journal = journal(tmp.path());
    let count = 100_000u32;
    let started = Instant::now();
    for n in 0..u64::from(count) {
        journal.append(record(n, 1024));
    }
    journal.sync(WAIT).unwrap();
    let rate = f64::from(count) / started.elapsed().as_secs_f64();
    println!("1 KiB records, flushed: {rate:.0} a second (budget at least 20,000)");
}

fn apply_to_durable() {
    let tmp = project();
    let journal = journal(tmp.path());
    let period = Duration::from_secs_f64(1.0 / 60.0);
    let mut times = Vec::with_capacity(600);
    let mut next = Instant::now();
    for n in 0..600u64 {
        // Paced, not slept for ordering: the rate is what is being measured.
        while Instant::now() < next {
            std::thread::yield_now();
        }
        next += period;
        let started = Instant::now();
        journal.append(record(n, 512));
        journal.sync(WAIT).unwrap();
        times.push(started.elapsed());
    }
    times.sort();
    println!(
        "apply to durable at 60 commits a second: p50 {:?}, p99 {:?} (budget 2 ms, 10 ms on ext4)",
        percentile(&times, 500),
        percentile(&times, 990)
    );
}

fn main() {
    append_latency();
    throughput();
    apply_to_durable();
}
