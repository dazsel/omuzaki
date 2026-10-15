//! A loaded, idle session must not spin: it burns under 1 ms of CPU per second (`cpu.total` counts every cycle, spec §10).
//!
//! This is the only test in this file on purpose: it measures the *process's* CPU time, so any other test thread running
//! beside it would be counted. It reads per-thread on-CPU nanoseconds from `/proc/self/task/*/schedstat` (Linux), which
//! resolves microseconds where `/proc/self/stat` ticks are 10 ms. Needs `OMUZAKI_ORT_CACHE`; skips with a message otherwise.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a panic is a test failure

mod common;

use common::{cache, fixture, local, with_source};
use omuzaki_ai_api::{DType, Dim, Provider, Runtime as _, SessionOptions, Signature, TensorBuf, TensorRef, TensorSpec};
use omuzaki_ai_ort::OrtRuntime;

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
    let Some(natives) = cache("an_idle_loaded_session_does_not_spin") else { return };
    if process_cpu_ns().is_none() {
        eprintln!("skipping an_idle_loaded_session_does_not_spin: /proc/self/task/*/schedstat is not readable here");
        return;
    }
    let x = TensorSpec::new("x", DType::F32, [Dim::Fixed(1), Dim::Fixed(4)]);
    let slot = Signature { inputs: vec![x.clone()], outputs: vec![TensorSpec { name: "y".into(), ..x }] };
    let path = fixture("identity.onnx");
    let files = [local("model.onnx", &path)];
    // Four intra-op threads: a thread pool is where spin-waiting would show.
    let options = SessionOptions::new(vec![Provider::Cpu], 4);
    let mut session =
        with_source(&files, &[], &natives, |s| OrtRuntime.load(s, &slot, &std::collections::BTreeMap::new(), &options))
            .unwrap();
    let mut outputs = TensorBuf::for_outputs(session.signature());
    let input = [1.0f32, 2.0, 3.0, 4.0];
    for _ in 0..50 {
        session.run(&[TensorRef::f32(&input, &[1, 4])], &mut outputs).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(200)); // let the run's own wake-ups settle

    let before = process_cpu_ns().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let used_ns = process_cpu_ns().unwrap() - before;
    let per_second_us = used_ns as f64 / 2.0 / 1000.0;
    eprintln!("idle loaded session (4 intra-op threads): {per_second_us:.1} us of CPU per second");
    assert!(
        per_second_us < 1000.0,
        "an idle session used {per_second_us:.0} us of CPU per second; it must stay under 1000"
    );
}
