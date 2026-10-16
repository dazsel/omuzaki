//! Benches of design §5.2's per-call rows: `host_small` (`host.log` ≤ 2 µs and `host.monotonic-seconds` ≤ 0.5 µs a
//! call, p95) and `fuel_calibration` (a node activation's 10⁶ fuel is about 1 ms on `R`: one plugin second's CPU
//! share of 10⁸ fuel should take about 100 ms). Each reports a time per call or per share; on a breach the nightly
//! run reports it, and nothing stops.

// A bench's setup may panic: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, missing_docs)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_plugin::bridge::NoBridge;
use omuzaki_plugin::log::PluginLog;
use omuzaki_plugin::{HostConfig, InstanceKind, InstanceSetup, LoadedPlugin, PluginEngine, load};
use omuzaki_plugin_api::PluginId;
use omuzaki_plugin_api::limits::{CPU_SHARE_FUEL_PER_SECOND, HOST_CALL_BURST, LOG_BURST};
use wasmtime::component::Component;

/// Calls of `host.log` in one `init`: every one passes the log's burst, so each is a kept line.
const LOG_CALLS: u32 = LOG_BURST;
/// Calls of `host.monotonic-seconds` in one `init`, inside the host-call burst.
const CLOCK_CALLS: u32 = HOST_CALL_BURST - 1;

/// A component whose `init` runs `body` with `host.log` lowered as `$log` and `host.monotonic-seconds` as `$now`.
fn component(body: &str) -> Vec<u8> {
    let text = format!(
        r#"(component
  (import "omuzaki:plugin/host@0.1.0" (instance $host
    (type $level' (enum "trace" "debug" "info" "warn" "error"))
    (export "level" (type $level (eq $level')))
    (export "log" (func (param "level" $level) (param "message" string)))
    (export "monotonic-seconds" (func (result f64)))))
  (core module $mem
    (memory (export "memory") 1)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) (i32.const 4096)))
  (core instance $memi (instantiate $mem))
  (alias core export $memi "memory" (core memory $memory))
  (alias core export $memi "realloc" (core func $realloc))
  (alias export $host "log" (func $log'))
  (alias export $host "monotonic-seconds" (func $now'))
  (core func $log-core (canon lower (func $log') (memory $memory) (realloc $realloc)))
  (core func $now-core (canon lower (func $now')))
  (core module $main
    (import "env" "memory" (memory 1))
    (import "env" "log" (func $log (param i32 i32 i32)))
    (import "env" "now" (func $now (result f64)))
    (data (i32.const 100) "a bench line")
    (func (export "init") (param i32 i32) (result i32)
      (local $i i32)
      {body}
      (i32.store8 (i32.const 16) (i32.const 0))
      (i32.const 16)))
  (core instance $env (export "memory" (memory $memory)) (export "log" (func $log-core)) (export "now" (func $now-core)))
  (core instance $maini (instantiate $main (with "env" (instance $env))))
  (alias core export $maini "init" (core func $init-core))
  (func $init (param "config" string) (result (result (error string)))
    (canon lift (core func $init-core) (memory $memory) (realloc $realloc)))
  (instance $lifecycle (export "init" (func $init)))
  (export "omuzaki:plugin/lifecycle@0.1.0" (instance $lifecycle)))"#
    );
    wat::parse_str(&text).unwrap()
}

/// `call` repeated `n` times.
fn repeat(n: u32, call: &str) -> String {
    format!(
        "(block $done (loop $l (br_if $done (i32.ge_u (local.get $i) (i32.const {n}))) {call}
          (local.set $i (i32.add (local.get $i) (i32.const 1))) (br $l)))"
    )
}

/// A fresh instance of `compiled`: each has its own bucket, log and CPU share.
fn instance(engine: &PluginEngine, compiled: &Component, clock: &Arc<FakeClock>) -> LoadedPlugin {
    let log = Arc::new(PluginLog::new(PluginId::new("bench").unwrap(), clock.clone(), None));
    let setup = InstanceSetup {
        kind: InstanceKind::Worker,
        log,
        bridge: Arc::new(NoBridge),
        insecure_seed: [1; 32],
        secure_seed: None,
        data_requested: false,
        net_requested: false,
    };
    LoadedPlugin::start(engine, compiled, setup).unwrap()
}

/// The time of one `init` of a fresh instance, per `per` units of work in it.
fn timed_init(engine: &PluginEngine, compiled: &Component, clock: &Arc<FakeClock>, iters: u64, per: u32) -> Duration {
    let mut total = Duration::ZERO;
    let mut done = 0;
    while done < iters {
        let mut plugin = instance(engine, compiled, clock);
        let started = Instant::now();
        let _ran = plugin.init("{}");
        total += started.elapsed();
        done += u64::from(per);
    }
    // Scaled to exactly `iters` units of work.
    total.mul_f64(iters as f64 / done as f64)
}

fn benches(c: &mut Criterion) {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let engine = PluginEngine::new(HostConfig::default(), clock.clone()).unwrap();
    let logging = load::compile(
        &engine,
        &component(&repeat(LOG_CALLS, "(call $log (i32.const 2) (i32.const 100) (i32.const 12))")),
    )
    .unwrap();
    let clocking = load::compile(&engine, &component(&repeat(CLOCK_CALLS, "(drop (call $now))"))).unwrap();
    let spinning = load::compile(&engine, &component("(loop $l (br $l))")).unwrap();

    let mut group = c.benchmark_group("plugin");
    group.bench_function("host_small_log", |b| {
        b.iter_custom(|iters| timed_init(&engine, &logging, &clock, iters, LOG_CALLS));
    });
    group.bench_function("host_small_monotonic", |b| {
        b.iter_custom(|iters| timed_init(&engine, &clocking, &clock, iters, CLOCK_CALLS));
    });
    group.sample_size(10);
    // One iteration is one CPU share: the endless `init` stops at the share's fuel.
    group.bench_function("fuel_calibration_1e8", |b| {
        b.iter_custom(|iters| (0..iters).map(|_| timed_init(&engine, &spinning, &clock, 1, 1)).sum());
    });
    group.finish();
    println!("fuel_calibration_1e8: {CPU_SHARE_FUEL_PER_SECOND} fuel an iteration; budget about 100 ms on `R`");
}

criterion_group!(plugin, benches);
criterion_main!(plugin);
