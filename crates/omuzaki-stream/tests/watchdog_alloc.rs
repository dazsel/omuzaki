//! The watchdog's hot paths allocate nothing (#1024): a counting allocator around `beat`, `read`, `observe` and
//! `tick` once the action buffer is warm, and the heartbeat counter under 8 threads.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_core::time::Seconds;
use omuzaki_stream::watchdog::{HeartbeatCell, Monitor, Observation, RestartPolicy, Scope, WorkerKind, WorkerSpec};

/// Counts allocations on the measuring thread while it measures.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

fn measuring() -> bool {
    MEASURING.try_with(Cell::get).unwrap_or(false)
}

// SAFETY: every call forwards to the system allocator unchanged; the counter is a relaxed atomic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if measuring() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract for `alloc` is passed on as is.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` with this `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if measuring() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller's contract for `realloc` is passed on as is.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn spec(name: &str) -> WorkerSpec {
    WorkerSpec {
        name: name.to_owned(),
        kind: WorkerKind::Renderer,
        start_grace: Seconds(5.0),
        hung_after: Seconds(0.5),
        slow_after: Seconds(2.0),
        restart: RestartPolicy {
            backoff_first: Seconds(0.25),
            backoff_max: Seconds(4.0),
            max_restarts: 1_000_000,
            within: Seconds(60.0),
            stable_after: Seconds(30.0),
        },
    }
}

#[test]
fn the_hot_paths_allocate_nothing_and_beats_are_never_lost() {
    // Both checks in one test, so no other test's allocations land inside the counted stretch.
    let cell = HeartbeatCell::new();
    let mut monitor = Monitor::new(Scope::Host, vec![spec("renderer"), spec("tracker")], Seconds(0.0));
    let mut out = Vec::with_capacity(16);
    let Some(w) = monitor.ids().next() else { panic!("no worker") };
    // Warm up: a whole crash and recovery, so every path has run once.
    monitor.tick(Seconds(0.0), &mut out);
    assert!(monitor.observe(Seconds(0.0), w, Observation::Spawned, &mut out).is_ok());
    assert!(monitor.observe(Seconds(0.1), w, Observation::Beat(1), &mut out).is_ok());

    MEASURING.with(|m| m.set(true));
    let mut t = 0.1;
    for round in 0..1_000_u32 {
        for _ in 0..30 {
            t += 0.033;
            cell.beat();
            assert!(monitor.observe(Seconds(t), w, Observation::Beat(cell.read()), &mut out).is_ok());
            monitor.tick(Seconds(t), &mut out);
        }
        if round % 10 == 0 {
            // A crash, a respawn and a recovery, all inside the counted stretch.
            assert!(monitor.observe(Seconds(t), w, Observation::Exited { code: Some(1) }, &mut out).is_ok());
            monitor.tick(Seconds(t + 5.0), &mut out);
            t += 5.0;
            assert!(monitor.observe(Seconds(t), w, Observation::Spawned, &mut out).is_ok());
            cell.beat();
            assert!(monitor.observe(Seconds(t + 0.01), w, Observation::Beat(1), &mut out).is_ok());
            assert!(monitor.observe(Seconds(t + 0.02), w, Observation::Beat(cell.read()), &mut out).is_ok());
        }
    }
    MEASURING.with(|m| m.set(false));
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    assert!(!monitor.log().is_empty(), "the crashes were logged");

    let shared = HeartbeatCell::new();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let cell = shared.clone();
            scope.spawn(move || (0..1_000_000).for_each(|_| cell.beat()));
        }
    });
    assert_eq!(shared.read(), 8_000_000);
}
