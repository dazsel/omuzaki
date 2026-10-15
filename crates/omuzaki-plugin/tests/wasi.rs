//! The WASI stub subset (design §4.1): a deadline-aware `poll` in a worker, a `poll` that never waits in a node
//! instance, and stdout into the plugin's log.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::{Duration, Instant};

use common::*;
use omuzaki_plugin::{InstanceKind, LoadedPlugin, load};
use omuzaki_plugin_api::{CallKind, FaultKind, LogLevel};

const POLL_IMPORTS: &str = r#"
  (import "wasi:io/poll@0.2.0" (instance $poll
    (export "pollable" (type $pt (sub resource)))
    (export "[method]pollable.block" (func (param "self" (borrow $pt))))))
  (alias export $poll "pollable" (type $pollable))
  (import "wasi:clocks/monotonic-clock@0.2.0" (instance $clock
    (alias outer 1 $pollable (type $p))
    (export "subscribe-duration" (func (param "when" u64) (result (own $p))))))
"#;

fn sleeper() -> Vec<u8> {
    let block = Lowered {
        alias: r#"(alias export $poll "[method]pollable.block" (func $block'))"#.into(),
        name: "block".into(),
        core_import: r#"(import "env" "block" (func $block (param i32)))"#.into(),
    };
    let subscribe = Lowered {
        alias: r#"(alias export $clock "subscribe-duration" (func $sub'))"#.into(),
        name: "sub".into(),
        core_import: r#"(import "env" "sub" (func $sub (param i64) (result i32)))"#.into(),
    };
    // Sleep for 60 s of the runner's clock, which never moves: only the deadline ends the wait.
    component(POLL_IMPORTS, &[block, subscribe], &init_doing("(call $block (call $sub (i64.const 60000000000)))"), 1)
}

fn started(kind: InstanceKind, bytes: &[u8], log: std::sync::Arc<omuzaki_plugin::log::PluginLog>) -> LoadedPlugin {
    let (engine, _) = engine();
    let compiled = load::compile(&engine, bytes).unwrap();
    let mut setup = setup(log, 1);
    setup.kind = kind;
    LoadedPlugin::start(&engine, &compiled, setup).unwrap_or_else(|e| panic!("{e:?}"))
}

#[test]
fn a_worker_waits_at_most_until_the_deadline_of_init() {
    let (_, clock) = engine();
    let mut plugin = started(InstanceKind::Worker, &sleeper(), log("sleeper", clock));
    let begun = Instant::now();
    let fault = plugin.init("{}").unwrap_err();
    let waited = begun.elapsed();
    assert_eq!((fault.kind, fault.detail.as_str()), (FaultKind::Timeout, "deadline in init"));
    let deadline = Duration::from_millis(CallKind::Init.budget().deadline_ms);
    assert!(waited >= deadline && waited < deadline.mul_f64(1.1), "{waited:?}");
}

#[test]
fn a_node_instance_never_waits() {
    let (_, clock) = engine();
    let mut plugin = started(InstanceKind::Node, &sleeper(), log("sleeper", clock));
    let begun = Instant::now();
    let fault = plugin.init("{}").unwrap_err();
    assert_eq!((fault.kind, fault.detail.as_str()), (FaultKind::Trap, "would block in a node activation"));
    assert!(begun.elapsed() < Duration::from_secs(1), "{:?}", begun.elapsed());
}

#[test]
fn stdout_lands_in_the_log_at_debug() {
    let imports = r#"
  (import "wasi:io/error@0.2.0" (instance $ioerr (export "error" (type (sub resource)))))
  (alias export $ioerr "error" (type $error))
  (import "wasi:io/streams@0.2.0" (instance $streams
    (export "output-stream" (type $os (sub resource)))
    (alias outer 1 $error (type $e))
    (type $se' (variant (case "last-operation-failed" (own $e)) (case "closed")))
    (export "stream-error" (type $se (eq $se')))
    (export "[method]output-stream.blocking-write-and-flush"
      (func (param "self" (borrow $os)) (param "contents" (list u8)) (result (result (error $se)))))))
  (alias export $streams "output-stream" (type $output))
  (import "wasi:cli/stdout@0.2.0" (instance $stdout
    (alias outer 1 $output (type $o))
    (export "get-stdout" (func (result (own $o))))))
"#;
    let stdout = Lowered {
        alias: r#"(alias export $stdout "get-stdout" (func $stdout'))"#.into(),
        name: "stdout".into(),
        core_import: r#"(import "env" "stdout" (func $stdout (result i32)))"#.into(),
    };
    let write = Lowered {
        alias: r#"(alias export $streams "[method]output-stream.blocking-write-and-flush" (func $write'))"#.into(),
        name: "write".into(),
        core_import: r#"(import "env" "write" (func $write (param i32 i32 i32 i32)))"#.into(),
    };
    let main = format!(
        r#"(data (i32.const 100) "hello\nworld\n") {}"#,
        init_doing("(call $write (call $stdout) (i32.const 100) (i32.const 12) (i32.const 32))")
    );
    let (_, clock) = engine();
    let log = log("printer", clock);
    let mut plugin = started(InstanceKind::Worker, &component(imports, &[stdout, write], &main, 1), log.clone());
    assert_eq!(plugin.init("{}"), Ok(()));
    let lines: Vec<(LogLevel, String)> = log.page(0, 10).into_iter().map(|l| (l.level, l.text)).collect();
    assert_eq!(lines, [(LogLevel::Debug, "hello".to_owned()), (LogLevel::Debug, "world".to_owned())]);
}
