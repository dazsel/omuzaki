//! `run` on a slot session allocates nothing: not in the steady state (1000 runs, once the output buffers have
//! grown), and not across a selection change or a crash either, where the loader thread does the allocating; also
//! while commits and the loader contend for the session's mailbox. The
//! counting allocator is the one from `omuzaki-ai-mock`'s `hot_loop` test; it counts only on the measuring thread, per
//! thread.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

mod common;

use std::time::{Duration, Instant};

use omuzaki_ai::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};
use omuzaki_ai_api::{ModelRef, ModelRegistry, NoProgress, Requester, RunError, Session, TensorBuf, TensorRef};
use omuzaki_ai_mock::MemFetcher;

struct Counting;

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    // Per thread, so tests that measure at the same time never count each other's allocations.
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn measure(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.with(Cell::get) - before
}

const MODELS: &str = "echo-fake";

fn url(version: &str) -> String {
    format!("https://models.invalid/{MODELS}/{version}/echo.fake.toml")
}

/// An identity model for the `fake` runtime; `tag` makes its digest unique and `crash_after` makes it die.
fn model_file(tag: &str, crash_after: Option<u64>) -> String {
    let crash = crash_after.map(|n| format!("crash_after = {n}\n")).unwrap_or_default();
    format!(
        "# {tag}\nscale = 1.0\nbias = 0.0\n{crash}\n[[inputs]]\nname = \"x\"\ndtype = \"f32\"\nshape = [\"any\"]\n\n\
         [[outputs]]\nname = \"y\"\ndtype = \"f32\"\nshape = [\"any\"]\n"
    )
}

fn manifest(version: &str, bytes: &str) -> (String, String) {
    let pin = common::pin(&[&url(version)], bytes.as_bytes());
    let text = format!(
        "id = \"{MODELS}\"\nversion = \"{version}\"\nname = \"Echo (fake)\"\nslot = \"test_echo@1\"\nruntime = \"fake\"\n\
         min_app_version = \"1.0.0\"\nproviders = [\"cpu\"]\n\n[[files]]\nname = \"echo.fake.toml\"\n\
         sha256 = \"{}\"\nsize = {}\nurls = [\"{}\"]\n\n[card]\nregister = \"omuzaki-ai tests\"\n\n\
         [card.licence]\nspdx = \"MIT OR Apache-2.0\"\n",
        pin.sha256,
        pin.size,
        url(version)
    );
    (MODELS.to_owned(), text)
}

/// One `run`; returns its result and what it allocated.
fn measured_run(session: &mut dyn Session, out: &mut [TensorBuf], frame: &[f32]) -> (Result<(), RunError>, usize) {
    let mut result = Ok(());
    let allocated = measure(|| result = session.run(&[TensorRef::f32(frame, &[frame.len()])], out));
    (result, allocated)
}

/// Runs, one `run` at a time, until `done`; returns everything those runs allocated.
fn allocated_until(
    session: &mut dyn Session,
    out: &mut [TensorBuf],
    frame: &[f32],
    mut done: impl FnMut(&dyn Session, &Result<(), RunError>) -> bool,
) -> usize {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut total = 0;
    loop {
        let (result, allocated) = measured_run(session, out, frame);
        total += allocated;
        if done(session, &result) {
            return total;
        }
        assert!(Instant::now() < deadline, "the loader never finished");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A registry with three echo models: 1.0.0 is the shipped default, 2.0.0 works and 3.0.0 crashes after three runs.
/// Each test tags its files apart, so the two tests' models never share a digest.
fn rig(tag: &str) -> (tempfile::TempDir, Registry, [ModelRef; 3]) {
    let files = [
        ("1.0.0", model_file(&format!("{tag}-1"), None)),
        ("2.0.0", model_file(&format!("{tag}-2"), None)),
        ("3.0.0", model_file(&format!("{tag}-3"), Some(3))),
    ];
    let mut net = MemFetcher::new();
    for (version, bytes) in &files {
        net = net.serving(url(version), bytes.as_str());
    }
    let dir = tempfile::tempdir().unwrap();
    let files_source = std::sync::Arc::new(VerifiedCache::new(dir.path().join(CACHE_DIR), net));
    let models: Vec<_> = files.iter().map(|(v, b)| manifest(v, b)).collect();
    let registry =
        Registry::with_models(dir.path(), files_source, "1.0.0".parse().unwrap(), RegistrySettings::default(), models)
            .unwrap();
    let slot = "test_echo@1".parse().unwrap();
    let by_version = |v: &str| -> ModelRef {
        let v = v.parse().unwrap();
        registry.models(Some(&slot)).into_iter().find(|e| e.model.version == v).unwrap().model
    };
    let models = [by_version("1.0.0"), by_version("2.0.0"), by_version("3.0.0")];
    for model in models.iter().rev() {
        registry.install(model, &NoProgress).unwrap(); // the last one, v1, is the shipped default
    }
    (dir, registry, models)
}

/// 1000 steady runs, a selection change and a crash with its fallback, every run measured.
fn steady_change_and_crash(registry: &Registry, [v1, v2, v3]: &[ModelRef; 3]) {
    let slot = "test_echo@1".parse().unwrap();
    let mut session = registry.open(&slot, None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    let frame = [0.5f32; 480];
    allocated_until(&mut *session, &mut out, &frame, |_, r| r.is_ok()); // warm-up: the output grows once

    let steady = (0..1000).map(|_| measured_run(&mut *session, &mut out, &frame).1).sum::<usize>();
    assert_eq!(steady, 0, "steady-state runs allocated");

    registry.select(&slot, Some(v2), Requester::Person).unwrap();
    let change = allocated_until(&mut *session, &mut out, &frame, |s, r| r.is_ok() && s.info().model == *v2);
    assert_eq!(change, 0, "runs across a selection change allocated");

    registry.select(&slot, Some(v3), Requester::Person).unwrap();
    let mut crashed = false;
    let crash = allocated_until(&mut *session, &mut out, &frame, |s, r| {
        crashed |= s.info().model == *v3 && r.is_ok(); // v3 answers, then dies after its third run
        crashed && r.is_ok() && s.info().model == *v1
    });
    assert_eq!(crash, 0, "runs across a crash and its fallback allocated");
}

#[test]
fn run_allocates_nothing_in_the_steady_state_or_across_a_change_or_a_crash() {
    let (_dir, registry, models) = rig("alloc");
    steady_change_and_crash(&registry, &models);
}

/// Stops the other thread even when an assertion fails, so the scope can end.
struct Stop<'a>(&'a AtomicBool);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Commits on another slot, as fast as they go, lock every session's mailbox (to check its model) and wake the loader
/// (which locks it again), so `run` often finds the mailbox taken: its retired sessions then wait in its own fixed
/// queue, and a replacement waits until two places are free.
#[test]
fn run_allocates_nothing_while_commits_and_the_loader_contend_for_the_mailbox() {
    let (_dir, registry, models) = rig("contend");
    let other: omuzaki_ai_api::SlotRef = "test_stream@1".parse().unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let churn = scope.spawn(|| {
            let mut commits = 0usize;
            while !stop.load(Ordering::Relaxed) {
                registry.select(&other, None, Requester::Person).unwrap();
                commits += 1;
            }
            commits
        });
        let stopping = Stop(&stop);
        steady_change_and_crash(&registry, &models);
        drop(stopping);
        assert!(churn.join().unwrap() > 0, "the other thread committed while the session ran");
    });
}
