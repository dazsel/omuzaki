//! The sandbox (#1292, #1567; design §4): loading, fuel and deadlines per call kind, the CPU share and the import
//! charge, memory caps and the all-plugins budget, the WASI stubs, and the refusals of the base world's imports.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::{Duration, Instant};

use common::*;
use omuzaki_core::time::Seconds;
use omuzaki_plugin::{HostConfig, InstanceKind, StartError, load};
use omuzaki_plugin_api::PluginError;
use omuzaki_plugin_api::limits::{
    CPU_SHARE_FUEL_PER_SECOND, CallKind, HOST_CALL_BURST, IMPORT_FUEL, LOG_BURST, MAX_TABLE_ELEMENTS, MIB, OP_BYTES_MAX,
};
use omuzaki_plugin_api::state::{FaultKind, LogLevel};

fn texts(log: &omuzaki_plugin::log::PluginLog) -> Vec<String> {
    log.page(0, usize::MAX).into_iter().map(|l| l.text).collect()
}

/// `init` logs the `len` bytes at `ptr` at `info`.
fn logging(body: &str, data: &str) -> Vec<u8> {
    component(HOST_IMPORT, &[log_import()], &format!("{data}\n{}", init_doing(body)), 1)
}

#[test]
fn an_init_that_returns_ok_is_ok_and_its_fuel_is_the_same_every_run() {
    let (engine, clock) = engine();
    let bytes = logging("(call $log (i32.const 2) (i32.const 100) (i32.const 5))", r#"(data (i32.const 100) "hello")"#);
    let run = || {
        let log = log("ok", clock.clone());
        let mut plugin = start(&engine, &bytes, log.clone()).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(plugin.init("{}"), Ok(()));
        assert_eq!(texts(&log), ["hello"]);
        plugin.fuel_used()
    };
    let first = run();
    assert!(first > IMPORT_FUEL, "the log call's charge is counted: {first}");
    assert_eq!(run(), first, "fuel is deterministic");
}

#[test]
fn an_endless_loop_runs_out_of_fuel_at_the_share_well_inside_the_deadline() {
    let (engine, clock) = engine();
    let mut plugin =
        start(&engine, &plain("(loop $l (br $l))"), log("loop", clock)).unwrap_or_else(|e| panic!("{e:?}"));
    let started = Instant::now();
    let fault = plugin.init("{}").unwrap_err();
    let deadline = Duration::from_millis(CallKind::Init.budget().deadline_ms);
    assert!(started.elapsed() < deadline.mul_f64(1.1), "{:?}", started.elapsed());
    assert_eq!((fault.kind, fault.detail.as_str()), (FaultKind::OutOfFuel, "CPU share used up in init"));
}

#[test]
fn a_long_call_is_interrupted_at_the_cpu_share_and_the_share_refills_next_second() {
    let (engine, clock) = engine();
    // About 5 × 10⁸ fuel of work: five times the share, half of init's cap.
    let work = "(local $i i32) (loop $l (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br_if $l (i32.lt_u (local.get $i) (i32.const 120000000))))";
    let bytes = component(
        "",
        &[],
        &format!(
            r#"(func (export "init") (param i32 i32) (result i32) {work}
        (i32.store8 (i32.const 16) (i32.const 0)) (i32.const 16))"#
        ),
        1,
    );
    let mut plugin = start(&engine, &bytes, log("long", clock.clone())).unwrap_or_else(|e| panic!("{e:?}"));
    // Instantiating used a little of this second's share already.
    let left = plugin.share_left();
    let fault = plugin.init("{}").unwrap_err();
    assert_eq!(fault.detail, "CPU share used up in init");
    assert_eq!(plugin.fuel_used(), left, "interrupted exactly at the share, not only refused next time");
    assert_eq!(plugin.share_left(), 0);
    clock.advance(Seconds(1.0));
    assert_eq!(plugin.share_left(), CPU_SHARE_FUEL_PER_SECOND);
}

#[test]
fn host_call_charges_reach_the_cpu_share() {
    let (engine, clock) = engine();
    let body = "(local $i i32) (loop $l (call $log (i32.const 2) (i32.const 100) (i32.const 2))
                (local.set $i (i32.add (local.get $i) (i32.const 1))) (br_if $l (i32.lt_u (local.get $i) (i32.const 50))))";
    let bytes = logging(body, r#"(data (i32.const 100) "hi")"#);
    let mut plugin = start(&engine, &bytes, log("charged", clock)).unwrap_or_else(|e| panic!("{e:?}"));
    let before = plugin.share_left();
    plugin.init("{}").unwrap();
    let used = plugin.fuel_used();
    assert!(used >= 50 * (IMPORT_FUEL + 2), "50 log calls charge at least {}: {used}", 50 * (IMPORT_FUEL + 2));
    assert_eq!(plugin.share_left(), before - used, "the charge comes out of the share");
}

#[test]
fn growing_memory_in_a_loop_is_out_of_memory_before_70_mib() {
    let (engine, clock) = engine();
    let body = "(loop $l (if (i32.eq (memory.grow (i32.const 1)) (i32.const -1)) (then unreachable)) (br $l))";
    let mut plugin = start(&engine, &plain(body), log("grow", clock)).unwrap_or_else(|e| panic!("{e:?}"));
    let fault = plugin.init("{}").unwrap_err();
    assert_eq!((fault.kind, fault.detail.as_str()), (FaultKind::OutOfMemory, "out of memory in init"));
    let linear = plugin.reserved_bytes() - omuzaki_plugin_api::limits::HOST_SIDE_BYTES;
    assert!(linear <= 64 * MIB, "stopped at the 64 MiB cap, under 70 MiB: {linear}");
}

#[test]
fn unreachable_and_deep_recursion_are_traps() {
    let (engine, clock) = engine();
    let mut trap =
        start(&engine, &plain("unreachable"), log("trap", clock.clone())).unwrap_or_else(|e| panic!("{e:?}"));
    let fault = trap.init("{}").unwrap_err();
    assert_eq!(fault.kind, FaultKind::Trap);
    assert!(fault.detail.starts_with("trap in init"), "{fault}");
    let recursion = component("", &[], &format!("(func $r (call $r)) {}", init_doing("(call $r)")), 1);
    let mut deep = start(&engine, &recursion, log("deep", clock)).unwrap_or_else(|e| panic!("{e:?}"));
    let fault = deep.init("{}").unwrap_err();
    assert_eq!((fault.kind, fault.detail.as_str()), (FaultKind::Trap, "stack overflow in init"));
}

#[test]
fn after_every_fault_the_next_plugin_loads_and_runs_on_the_same_engine() {
    let (engine, clock) = engine();
    for body in ["unreachable", "(loop $l (br $l))", "(call $r)"] {
        let main = format!("(func $r (call $r)) {}", init_doing(body));
        let mut bad = start(&engine, &component("", &[], &main, 1), log("bad", clock.clone())).unwrap();
        assert!(bad.init("{}").is_err(), "{body}");
        clock.advance(Seconds(1.0));
        let mut good = start(&engine, &ok_component(), log("good", clock.clone())).unwrap();
        assert_eq!(good.init("{}"), Ok(()), "after {body}");
    }
}

#[test]
fn an_init_error_faults_with_its_text_and_invalid_utf8_is_a_bad_return() {
    let (engine, clock) = engine();
    let err = |text: &str| {
        let main = format!(
            r#"(data (i32.const 100) "{text}")
            (func (export "init") (param i32 i32) (result i32)
              (i32.store8 (i32.const 16) (i32.const 1))
              (i32.store (i32.const 20) (i32.const 100))
              (i32.store (i32.const 24) (i32.const {len}))
              (i32.const 16))"#,
            len = text.len() - text.matches('\\').count() * 2,
        );
        let mut plugin = start(&engine, &component("", &[], &main, 1), log("err", clock.clone())).unwrap();
        plugin.init("{}").unwrap_err()
    };
    let refused = err("no thanks");
    assert_eq!((refused.kind, refused.detail.as_str()), (FaultKind::InitError, "init failed: no thanks"));
    let bad = err("\\ff\\fe");
    assert_eq!(bad.kind, FaultKind::BadReturn, "{bad}");
}

#[test]
fn a_trap_at_start_is_a_fault_before_init() {
    let (engine, clock) = engine();
    let main = format!("(func $boom unreachable) (start $boom) {INIT_OK}");
    match start(&engine, &component("", &[], &main, 1), log("start", clock)) {
        Err(StartError::Faulted(fault)) => assert_eq!(fault.kind, FaultKind::Trap, "{fault}"),
        other => panic!("{:?}", other.map(|_| ())),
    }
}

#[test]
fn a_core_module_a_truncated_component_and_an_oversized_one_are_refused() {
    let (engine, _) = engine();
    let module = wat::parse_str("(module)").unwrap();
    assert!(matches!(load::compile(&engine, &module), Err(PluginError::Instantiate(m)) if m.contains("core module")));
    let bytes = ok_component();
    assert!(matches!(load::compile(&engine, &bytes[..bytes.len() / 2]), Err(PluginError::Instantiate(_))));
    let huge = vec![0u8; usize::try_from(omuzaki_plugin_api::limits::COMPONENT_MAX_BYTES + 1).unwrap()];
    assert!(matches!(load::compile(&engine, &huge), Err(PluginError::Package(_))));
}

#[test]
fn imports_outside_the_stub_subset_are_refused_and_the_subset_is_not() {
    let (engine, _) = engine();
    let manifest = manifest("imports");
    let sockets = component(r#"(import "wasi:sockets/tcp@0.2.0" (instance))"#, &[], INIT_OK, 1);
    let compiled = load::compile(&engine, &sockets).unwrap();
    assert_eq!(load::check(&engine, &compiled, &manifest), Err(PluginError::imports(["wasi:sockets/tcp"])));
    let other = component(r#"(import "acme:secret/door" (instance))"#, &[], INIT_OK, 1);
    let compiled = load::compile(&engine, &other).unwrap();
    assert_eq!(load::check(&engine, &compiled, &manifest), Err(PluginError::imports(["acme:secret/door"])));
    let (environment, _) =
        wasi_import("wasi:cli/environment", "initial-cwd", "(result (option string))", "cwd", "(param i32)");
    let compiled = load::compile(&engine, &component(&environment, &[], INIT_OK, 1)).unwrap();
    assert_eq!(load::check(&engine, &compiled, &manifest).map(|b| b.version), Ok("0.1.0".to_owned()));
}

#[test]
fn the_wit_version_must_be_served_and_agree_with_the_manifest() {
    let (engine, _) = engine();
    let compiled = load::compile(&engine, &ok_component()).unwrap();
    let mut wrong = manifest("versions");
    wrong.api = "^0.2".into();
    assert!(matches!(load::check(&engine, &compiled, &wrong), Err(PluginError::Manifest(m)) if m.contains("^0.2")));
    let text = component_text("", &[], INIT_OK, 1).replace("lifecycle@0.1.0", "lifecycle@0.3.0");
    let newer = load::compile(&engine, &wat::parse_str(&text).unwrap()).unwrap();
    assert_eq!(
        load::check(&engine, &newer, &manifest("versions")),
        Err(PluginError::incompatible("0.3", vec!["0.1".into()]))
    );
}

#[test]
fn an_init_only_component_of_the_full_world_links_with_data_and_http_refused() {
    let (engine, clock) = engine();
    let list = Lowered {
        alias: r#"(alias export $data "list" (func $dlist'))"#.into(),
        name: "dlist".into(),
        core_import: r#"(import "env" "dlist" (func $dlist (param i32)))"#.into(),
    };
    let imports = format!("{HOST_IMPORT}{WORLD_IMPORTS}");
    let bytes = component(&imports, &[list], &init_doing("(call $dlist (i32.const 32))"), 1);
    let compiled = load::compile(&engine, &bytes).unwrap();
    assert!(load::check(&engine, &compiled, &manifest("world")).is_ok());
    for (kind, why) in
        [(InstanceKind::Worker, "the plugin did not ask for data"), (InstanceKind::Node, "not in a node activation")]
    {
        let log = log("world", clock.clone());
        let mut setup = setup(log.clone(), 1);
        setup.kind = kind;
        let mut plugin =
            omuzaki_plugin::LoadedPlugin::start(&engine, &compiled, setup).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(plugin.init("{}"), Ok(()), "{kind:?}");
        let lines = log.page(0, 10);
        assert_eq!(lines.len(), 1, "one warn line per refused call: {lines:?}");
        assert_eq!((lines[0].level, lines[0].text.clone()), (LogLevel::Warn, format!("data.list refused: {why}")));
    }
}

#[test]
fn a_log_flood_keeps_the_burst_and_the_host_call_bucket_traps() {
    let (engine, clock) = engine();
    let body = "(local $i i32) (loop $l (call $log (i32.const 2) (i32.const 100) (i32.const 2))
                (local.set $i (i32.add (local.get $i) (i32.const 1))) (br_if $l (i32.lt_u (local.get $i) (i32.const 1000000))))";
    let log = log("flood", clock);
    let mut plugin = start(&engine, &logging(body, r#"(data (i32.const 100) "hi")"#), log.clone()).unwrap();
    let fault = plugin.init("{}").unwrap_err();
    assert_eq!((fault.kind, fault.detail.as_str()), (FaultKind::HostCallFlood, "host-call flood in init"));
    log.flush();
    let lines = texts(&log);
    assert_eq!(lines.len(), LOG_BURST as usize + 1);
    let dropped = HOST_CALL_BURST - LOG_BURST;
    assert_eq!(lines.last().map(String::as_str), Some(format!("{dropped} lines dropped").as_str()));
}

/// `init` reads 16 bytes of `random` and 16 of `insecure` and logs both in hex.
fn random_reader() -> Vec<u8> {
    let (random, rand) = wasi_import(
        "wasi:random/random",
        "get-random-bytes",
        r#"(param "len" u64) (result (list u8))"#,
        "rand",
        "(param i64 i32)",
    );
    let (insecure, weak) = wasi_import(
        "wasi:random/insecure",
        "get-insecure-random-bytes",
        r#"(param "len" u64) (result (list u8))"#,
        "weak",
        "(param i64 i32)",
    );
    let main = format!(
        r#"(data (i32.const 200) "0123456789abcdef")
        (func $hex (param $src i32) (param $dst i32)
          (local $i i32) (local $b i32)
          (block $done (loop $l
            (br_if $done (i32.ge_u (local.get $i) (i32.const 16)))
            (local.set $b (i32.load8_u (i32.add (local.get $src) (local.get $i))))
            (i32.store8 (i32.add (local.get $dst) (i32.shl (local.get $i) (i32.const 1)))
              (i32.load8_u (i32.add (i32.const 200) (i32.shr_u (local.get $b) (i32.const 4)))))
            (i32.store8 (i32.add (i32.add (local.get $dst) (i32.shl (local.get $i) (i32.const 1))) (i32.const 1))
              (i32.load8_u (i32.add (i32.const 200) (i32.and (local.get $b) (i32.const 15)))))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br $l))))
        {}"#,
        init_doing(
            "(call $rand (i64.const 16) (i32.const 32)) (call $hex (i32.load (i32.const 32)) (i32.const 300))
             (call $log (i32.const 2) (i32.const 300) (i32.const 32))
             (call $weak (i64.const 16) (i32.const 40)) (call $hex (i32.load (i32.const 40)) (i32.const 400))
             (call $log (i32.const 2) (i32.const 400) (i32.const 32))"
        )
    );
    component(&format!("{HOST_IMPORT}{random}{insecure}"), &[log_import(), rand, weak], &main, 1)
}

#[test]
fn random_is_os_entropy_and_insecure_is_the_seeded_stream() {
    let (engine, clock) = engine();
    let compiled = load::compile(&engine, &random_reader()).unwrap();
    let read = |seed: u8, kind: InstanceKind| {
        let log = log("random", clock.clone());
        let mut setup = setup(log.clone(), seed);
        setup.kind = kind;
        let mut plugin =
            omuzaki_plugin::LoadedPlugin::start(&engine, &compiled, setup).unwrap_or_else(|e| panic!("{e:?}"));
        plugin.init("{}").unwrap();
        texts(&log)
    };
    let (a, b) = (read(7, InstanceKind::Worker), read(7, InstanceKind::Worker));
    assert_ne!(a[0], b[0], "random differs between two sessions of one package");
    assert_eq!(a[1], b[1], "insecure is the same stream for the same seed");
    assert_ne!(read(8, InstanceKind::Worker)[1], a[1], "another session's stream differs");
    let (n1, n2) = (read(7, InstanceKind::Node), read(7, InstanceKind::Node));
    assert_eq!(n1, n2, "a node instance never reaches OS entropy");
}

#[test]
fn a_random_request_over_the_cap_traps_before_the_host_allocates() {
    let (engine, clock) = engine();
    let (random, rand) = wasi_import(
        "wasi:random/random",
        "get-random-bytes",
        r#"(param "len" u64) (result (list u8))"#,
        "rand",
        "(param i64 i32)",
    );
    let bytes = component(&random, &[rand], &init_doing("(call $rand (i64.const 4294967296) (i32.const 32))"), 1);
    let mut plugin = start(&engine, &bytes, log("huge", clock)).unwrap();
    let fault = plugin.init("{}").unwrap_err();
    assert_eq!((fault.kind, fault.detail.as_str()), (FaultKind::Trap, "random request too large in init"));
}

#[test]
fn exit_is_a_fault() {
    let (engine, clock) = engine();
    let (exit, lowered) = wasi_import("wasi:cli/exit", "exit", r#"(param "status" (result))"#, "exit", "(param i32)");
    let bytes = component(&exit, &[lowered], &init_doing("(call $exit (i32.const 0))"), 1);
    let fault = start(&engine, &bytes, log("exit", clock)).unwrap().init("{}").unwrap_err();
    assert_eq!((fault.kind, fault.detail.as_str()), (FaultKind::Exited, "exited in init"));
}

#[test]
fn idle_plugins_fill_the_all_plugins_budget_and_the_next_start_is_refused() {
    let (engine, clock) = engine_with(HostConfig::default());
    // 56 MiB of initial memory plus the 8 MiB host side: four fit the editor's 256 MiB, the fifth does not.
    let bytes = component("", &[], INIT_OK, 896);
    let compiled = load::compile(&engine, &bytes).unwrap();
    let start_one = || omuzaki_plugin::LoadedPlugin::start(&engine, &compiled, setup(log("idle", clock.clone()), 1));
    let running: Vec<_> = (0..4).map(|_| start_one().unwrap_or_else(|e| panic!("{e:?}"))).collect();
    assert_eq!(engine.memory().used(), 4 * 64 * MIB);
    assert!(matches!(start_one(), Err(StartError::Refused(PluginError::Limit { .. }))));
    drop(running);
    assert_eq!(engine.memory().used(), 0, "a stopped plugin gives its memory back");
    assert!(start_one().is_ok());
}

#[test]
fn approval_resolved_without_the_export_is_ignored_and_shutdown_denies_effects() {
    let (engine, clock) = engine();
    let mut plugin = start(&engine, &ok_component(), log("quiet", clock)).unwrap();
    plugin.init("{}").unwrap();
    assert_eq!(plugin.approval_resolved(1, Ok("{}".into())), Ok(()), "a component without the export ignores it");
    assert_eq!(plugin.shutdown(), Ok(()), "a component without shutdown stops cleanly");
}

#[test]
fn growing_a_table_past_its_cap_is_out_of_memory() {
    let (engine, clock) = engine();
    let grow = format!(
        "(if (i32.eq (table.grow $t (ref.null func) (i32.const {MAX_TABLE_ELEMENTS})) (i32.const -1)) (then unreachable))"
    );
    let main = format!("(table $t 1 funcref)\n    {}", init_doing(&grow));
    let mut plugin =
        start(&engine, &component("", &[], &main, 1), log("table", clock)).unwrap_or_else(|e| panic!("{e:?}"));
    let fault = plugin.init("{}").unwrap_err();
    assert_eq!((fault.kind, fault.detail.as_str()), (FaultKind::OutOfMemory, "out of memory in init"));
}

#[test]
fn an_op_argument_over_its_cap_faults_before_the_host_copies_it() {
    let (engine, clock) = engine();
    let call = Lowered {
        alias: r#"(alias export $ops "call" (func $call'))"#.into(),
        name: "call".into(),
        core_import: r#"(import "env" "call" (func $call (param i32 i32 i32 i32 i32)))"#.into(),
    };
    let imports = format!("{HOST_IMPORT}{WORLD_IMPORTS}");
    let len = 2 * OP_BYTES_MAX;
    let body = format!("(call $call (i32.const 100) (i32.const 4) (i32.const 65536) (i32.const {len}) (i32.const 32))");
    let pages = u32::try_from(len / 65536 + 2).unwrap();
    let bytes = component(&imports, &[call], &init_doing(&body), pages);
    let mut plugin = start(&engine, &bytes, log("big", clock)).unwrap_or_else(|e| panic!("{e:?}"));
    let fault = plugin.init("{}").unwrap_err();
    let detail = format!("bad return value in init: ops.call arguments is {len} bytes, over {OP_BYTES_MAX}");
    assert_eq!((fault.kind, fault.detail), (FaultKind::BadReturn, detail));
}
