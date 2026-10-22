//! A loaded, idle session must not spin: it burns under 1 ms of CPU per second (`cpu.total` counts every cycle, spec
//! §10). At 4 threads the XNNPACK delegate has a worker pool; the shim sets `XNN_FLAG_DONT_SPIN_WORKERS`, and this test
//! shows its effect whether or not the flag reaches pthreadpool (design §12).
//!
//! This is the only test in this file on purpose: it measures the *process's* CPU time, so any other test thread
//! running beside it would be counted. It reads per-thread on-CPU nanoseconds from `/proc/self/task/*/schedstat`
//! (Linux). Skips with a message without `OMUZAKI_LITERT_CACHE`.
#![allow(clippy::doc_markdown, clippy::unwrap_used, clippy::cast_precision_loss)] // panics are test failures

mod common;

use common::model::{Value, add_graph, tensor_of};
use common::{add_slot, library, load_synthetic};
use omuzaki_ai_api::{Dim, Session as _, TensorBuf, TensorRef};

/// Total on-CPU time of every thread of this process so far, in nanoseconds; `None` where the kernel doesn't say.
fn process_cpu_ns() -> Option<u64> {
    let mut total = 0u64;
    for task in std::fs::read_dir("/proc/self/task").ok()? {
        let schedstat = std::fs::read_to_string(task.ok()?.path().join("schedstat")).ok()?;
        total += schedstat.split_whitespace().next()?.parse::<u64>().ok()?;
    }
    Some(total)
}

#[test]
fn an_idle_loaded_session_does_not_spin() {
    const WIDTH: i32 = 1 << 16;
    if !library("an_idle_loaded_session_does_not_spin") {
        return;
    }
    if process_cpu_ns().is_none() {
        eprintln!("skipping an_idle_loaded_session_does_not_spin: /proc/self/task/*/schedstat is not readable here");
        return;
    }
    // A wide ADD, so XNNPACK's four workers all get work and then go idle.
    let mut model = add_graph();
    for index in 0..3 {
        tensor_of(&mut model, index).set(0, Value::Ints(vec![1, WIDTH]));
    }
    let width = usize::try_from(WIDTH).unwrap();
    let mut session = load_synthetic(&model, &add_slot(&[Dim::Fixed(1), Dim::Fixed(width)]), 4);
    let mut out = TensorBuf::for_outputs(session.signature());
    let input = vec![1.0f32; width];
    for _ in 0..50 {
        session.run(&[TensorRef::f32(&input, &[1, width]), TensorRef::f32(&input, &[1, width])], &mut out).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(200)); // let the run's own wake-ups settle

    let before = process_cpu_ns().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let used_ns = process_cpu_ns().unwrap() - before;
    let per_second_us = used_ns as f64 / 2.0 / 1000.0;
    eprintln!("idle loaded session (4 threads, XNNPACK pool): {per_second_us:.1} us of CPU per second");
    assert!(
        per_second_us < 1000.0,
        "an idle session used {per_second_us:.0} us of CPU per second; it must stay under 1000"
    );
}
