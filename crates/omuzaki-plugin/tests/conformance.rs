//! The wasmtime host runs the conformance suite of `omuzaki-plugin-mock` with WAT components (design §12). The groups
//! that need the op bridge (the ceiling and per-use queueing) join when the bridge does.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::*;
use omuzaki_ops_api::Grants;
use omuzaki_plugin::{HostConfig, PluginEngine, WasmPlugins};
use omuzaki_plugin_api::manifest::Manifest;
use omuzaki_plugin_mock::conformance::{self, Behaviour, Subject, World};

/// The host as a conformance subject: each behaviour as a WAT component.
struct WasmSubject;

/// `init` returns `err(text)`.
fn init_error(text: &str) -> Vec<u8> {
    let main = format!(
        r#"(data (i32.const 100) "{text}")
        (func (export "init") (param i32 i32) (result i32)
          (i32.store8 (i32.const 16) (i32.const 1))
          (i32.store (i32.const 20) (i32.const 100))
          (i32.store (i32.const 24) (i32.const {len}))
          (i32.const 16))"#,
        len = text.len()
    );
    component("", &[], &main, 1)
}

/// `init` logs `line 0` to `line <n-1>` (n at most 10).
fn logs(n: u32) -> Vec<u8> {
    let data = (0..10).map(|i| format!("line {i}")).collect::<Vec<_>>().concat();
    let body = format!(
        "(local $i i32) (block $done (loop $l (br_if $done (i32.ge_u (local.get $i) (i32.const {n})))
          (call $log (i32.const 2) (i32.add (i32.const 100) (i32.mul (local.get $i) (i32.const 6))) (i32.const 6))
          (local.set $i (i32.add (local.get $i) (i32.const 1))) (br $l)))"
    );
    component(HOST_IMPORT, &[log_import()], &format!(r#"(data (i32.const 100) "{data}") {}"#, init_doing(&body)), 1)
}

/// `init` works for a while (well inside the CPU share), then logs `init done`.
fn slow() -> Vec<u8> {
    let body = "(local $i i32) (loop $l (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br_if $l (i32.lt_u (local.get $i) (i32.const 5000000))))
        (call $log (i32.const 2) (i32.const 100) (i32.const 9))";
    component(HOST_IMPORT, &[log_import()], &format!(r#"(data (i32.const 100) "init done") {}"#, init_doing(body)), 1)
}

fn bytes_of(behaviour: &Behaviour) -> Vec<u8> {
    match behaviour {
        Behaviour::Ok => ok_component(),
        Behaviour::InitError(text) => init_error(text),
        Behaviour::Trap => plain("unreachable"),
        Behaviour::Loop => plain("(loop $l (br $l))"),
        Behaviour::Logs(n) => logs(*n),
        Behaviour::Slow => slow(),
        Behaviour::Calls(_) => panic!("op calls need the op bridge, which this host does not have yet"),
    }
}

impl Subject for WasmSubject {
    type Service = WasmPlugins;

    fn build(&self, world: &World, plugins: Vec<(Manifest, Behaviour)>) -> WasmPlugins {
        let engine = Arc::new(PluginEngine::new(HostConfig::default(), world.clock.clone()).unwrap());
        let service = WasmPlugins::new(engine, world.sessions.clone(), Grants::person(), None);
        for (i, (manifest, behaviour)) in plugins.into_iter().enumerate() {
            let hash = [u8::try_from(i).unwrap(); 32];
            service.add(manifest, &bytes_of(&behaviour), hash).unwrap_or_else(|e| panic!("{e}"));
        }
        service
    }
}

#[test]
fn lifecycle() {
    conformance::lifecycle(&WasmSubject);
}

#[test]
fn faults() {
    conformance::faults(&WasmSubject);
}

#[test]
fn restart_and_disable() {
    conformance::restart_and_disable(&WasmSubject);
}

#[test]
fn reload_drains() {
    conformance::reload_drains(&WasmSubject);
}

#[test]
fn logs_page() {
    conformance::logs(&WasmSubject);
}
