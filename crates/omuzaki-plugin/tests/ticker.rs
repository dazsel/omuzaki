//! Dropping the engine joins its epoch ticker: no thread outlives it. Its own test binary, so no other test's engine
//! is alive while it counts threads.

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_plugin::{HostConfig, PluginEngine};

/// How many of this process's threads are epoch tickers (Linux's thread list).
fn ticker_threads() -> usize {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else { return 0 };
    tasks
        .filter_map(Result::ok)
        .filter_map(|task| std::fs::read_to_string(task.path().join("comm")).ok())
        .filter(|name| name.trim() == "plugin-epoch")
        .count()
}

#[test]
fn dropping_the_engine_joins_the_ticker() {
    if !std::path::Path::new("/proc/self/task").exists() {
        return; // The thread list is Linux's; the join itself is the same code on every platform.
    }
    assert_eq!(ticker_threads(), 0);
    let engine = PluginEngine::new(HostConfig::default(), Arc::new(FakeClock::new(Seconds(0.0)))).unwrap();
    // A thread names itself once it runs, a moment after `spawn` returns.
    let named = std::time::Instant::now();
    while ticker_threads() == 0 && named.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(ticker_threads(), 1);
    drop(engine);
    // A joined thread can stay listed for a moment: the join returns when the kernel clears its id, a little before
    // the task leaves `/proc` (#2661). A thread left running stays listed for the whole wait.
    let gone = std::time::Instant::now();
    while ticker_threads() != 0 && gone.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(ticker_threads(), 0, "the ticker thread is joined, not left running");
}
