//! `run` on a slot session allocates nothing: not in the steady state (1000 runs, once the output buffers have
//! grown), and not across a selection change or a crash either, where the loader thread does the allocating. The
//! counting allocator is the one from `omuzaki-ai-mock`'s `hot_loop` test; it counts only on the measuring thread.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

mod common;

use std::time::{Duration, Instant};

use omuzaki_ai::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};
use omuzaki_ai_api::{ModelRef, ModelRegistry, NoProgress, Requester, RunError, Session, TensorBuf, TensorRef};
use omuzaki_ai_mock::MemFetcher;

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
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
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.load(Ordering::Relaxed) - before
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

#[test]
fn run_allocates_nothing_in_the_steady_state_or_across_a_change_or_a_crash() {
    let files = [
        ("1.0.0", model_file("alloc-1", None)),
        ("2.0.0", model_file("alloc-2", None)),
        ("3.0.0", model_file("alloc-3", Some(3))),
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
    let (v1, v2, v3) = (by_version("1.0.0"), by_version("2.0.0"), by_version("3.0.0"));
    for model in [&v3, &v2, &v1] {
        registry.install(model, &NoProgress).unwrap(); // the last one, v1, is the shipped default
    }
    let mut session = registry.open(&slot, None).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    let frame = [0.5f32; 480];
    allocated_until(&mut *session, &mut out, &frame, |_, r| r.is_ok()); // warm-up: the output grows once

    let steady = (0..1000).map(|_| measured_run(&mut *session, &mut out, &frame).1).sum::<usize>();
    assert_eq!(steady, 0, "steady-state runs allocated");

    registry.select(&slot, Some(&v2), Requester::Person).unwrap();
    let change = allocated_until(&mut *session, &mut out, &frame, |s, r| r.is_ok() && s.info().model == v2);
    assert_eq!(change, 0, "runs across a selection change allocated");

    registry.select(&slot, Some(&v3), Requester::Person).unwrap();
    let mut crashed = false;
    let crash = allocated_until(&mut *session, &mut out, &frame, |s, r| {
        crashed |= s.info().model == v3 && r.is_ok(); // v3 answers, then dies after its third run
        crashed && r.is_ok() && s.info().model == v1
    });
    assert_eq!(crash, 0, "runs across a crash and its fallback allocated");
}
