//! WAT components for the host's tests: a builder that wraps a guest's core module in the component skeleton every
//! plugin has (one memory with a bump `realloc`, the imports it lowers, `lifecycle` lifted), plus the engine and
//! setup helpers.
//!
//! Integration-test helpers may panic: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::sync::Arc;

use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_plugin::bridge::NoBridge;
use omuzaki_plugin::log::PluginLog;
use omuzaki_plugin::{HostConfig, InstanceKind, InstanceSetup, LoadedPlugin, PluginEngine, StartError};
use omuzaki_plugin_api::PluginId;
use omuzaki_plugin_api::manifest::Manifest;
use wasmtime::component::Component;

/// `omuzaki:plugin/host` as a component import type.
pub const HOST_IMPORT: &str = r#"(import "omuzaki:plugin/host@0.1.0" (instance $host
    (type $level' (enum "trace" "debug" "info" "warn" "error"))
    (export "level" (type $level (eq $level')))
    (export "log" (func (param "level" $level) (param "message" string)))
    (export "monotonic-seconds" (func (result f64)))))"#;

/// `omuzaki:plugin/ops`, `data` and `http` as component import types: the whole base world.
pub const WORLD_IMPORTS: &str = r#"
  (import "omuzaki:plugin/ops@0.1.0" (instance $ops
    (type $code' (enum "invalid" "not-found" "denied" "conflict" "unsupported" "unavailable" "internal" "waiting" "limited" "cancelled"))
    (export "error-code" (type $code (eq $code')))
    (type $err' (record (field "code" $code) (field "message" string) (field "detail" (option string)) (field "hint" (option string))))
    (export "op-error" (type $err (eq $err')))
    (export "call" (func (param "op" string) (param "args" string) (result (result string (error $err)))))
    (export "plan" (func (param "op" string) (param "args" string) (result (result string (error $err)))))
    (export "describe" (func (param "op" string) (result (option string))))
    (export "list" (func (result (list string))))))
  (import "omuzaki:plugin/data@0.1.0" (instance $data
    (export "read" (func (param "name" string) (result (option (list u8)))))
    (export "write" (func (param "name" string) (param "bytes" (list u8)) (result (result (error string)))))
    (export "remove" (func (param "name" string) (result bool)))
    (export "list" (func (result (list string))))))
  (import "omuzaki:plugin/http@0.1.0" (instance $http
    (type $req' (record (field "method" string) (field "url" string) (field "headers" (list (tuple string string))) (field "body" (list u8))))
    (export "request" (type $req (eq $req')))
    (type $resp' (record (field "status" u16) (field "headers" (list (tuple string string))) (field "body" (list u8))))
    (export "response" (type $resp (eq $resp')))
    (export "fetch" (func (param "req" $req) (result (result $resp (error string)))))))
"#;

/// One import the guest's core module sees: its component-level alias, how it is lowered, and its core type.
pub struct Lowered {
    /// `(alias export $host "log" (func $log'))`. A definition that starts `(core func` is the core func itself
    /// (`(core func $drop-core (canon resource.drop $pollable))`) and is not lowered again.
    pub alias: String,
    /// The core func name in the `env` instance, e.g. `log`.
    pub name: String,
    /// The core import line, e.g. `(import "env" "log" (func $log (param i32 i32 i32)))`.
    pub core_import: String,
}

/// `host.log`, lowered as `$log (level, ptr, len)`.
pub fn log_import() -> Lowered {
    Lowered {
        alias: r#"(alias export $host "log" (func $log'))"#.into(),
        name: "log".into(),
        core_import: r#"(import "env" "log" (func $log (param i32 i32 i32)))"#.into(),
    }
}

/// A WASI function `func` of `interface@0.2.0` with component type `ty`, lowered as `$<core>` of core type `core_ty`.
pub fn wasi_import(interface: &str, func: &str, ty: &str, core: &str, core_ty: &str) -> (String, Lowered) {
    let import = format!(r#"(import "{interface}@0.2.0" (instance ${core}-i (export "{func}" (func {ty}))))"#);
    let lowered = Lowered {
        alias: format!(r#"(alias export ${core}-i "{func}" (func ${core}'))"#),
        name: core.into(),
        core_import: format!(r#"(import "env" "{core}" (func ${core} {core_ty}))"#),
    };
    (import, lowered)
}

/// A component around `main`, a core module body that imports `env.memory` and the `lowered` funcs and exports
/// `init (param i32 i32) (result i32)`. `imports` are component-level import declarations. `memory_pages` is the
/// memory's initial size in 64 KiB pages.
pub fn component(imports: &str, lowered: &[Lowered], main: &str, memory_pages: u32) -> Vec<u8> {
    let text = component_text(imports, lowered, main, memory_pages);
    match wat::parse_str(&text) {
        Ok(bytes) => bytes,
        Err(error) => panic!("{error}\n{text}"),
    }
}

/// The WAT text [`component`] parses.
pub fn component_text(imports: &str, lowered: &[Lowered], main: &str, memory_pages: u32) -> String {
    let aliases: String = lowered.iter().map(|l| l.alias.clone()).collect::<Vec<_>>().join("\n  ");
    let lowers: String = lowered
        .iter()
        .filter(|l| !l.alias.starts_with("(core func"))
        .map(|l| {
            format!("(core func ${n}-core (canon lower (func ${n}') (memory $memory) (realloc $realloc)))", n = l.name)
        })
        .collect::<Vec<_>>()
        .join("\n  ");
    let env_exports: String =
        lowered.iter().map(|l| format!(r#"(export "{n}" (func ${n}-core))"#, n = l.name)).collect::<Vec<_>>().join(" ");
    let core_imports: String = lowered.iter().map(|l| l.core_import.clone()).collect::<Vec<_>>().join("\n    ");
    format!(
        r#"(component
  {imports}
  (core module $mem
    (memory (export "memory") {memory_pages})
    (global $heap (mut i32) (i32.const 2048))
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      (local $p i32)
      (local.set $p (i32.and (i32.add (global.get $heap) (i32.sub (local.get 2) (i32.const 1)))
                             (i32.sub (i32.const 0) (local.get 2))))
      (global.set $heap (i32.add (local.get $p) (local.get 3)))
      (local.get $p)))
  (core instance $memi (instantiate $mem))
  (alias core export $memi "memory" (core memory $memory))
  (alias core export $memi "realloc" (core func $realloc))
  {aliases}
  {lowers}
  (core module $main
    (import "env" "memory" (memory 1))
    {core_imports}
    {main})
  (core instance $env (export "memory" (memory $memory)) {env_exports})
  (core instance $maini (instantiate $main (with "env" (instance $env))))
  (alias core export $maini "init" (core func $init-core))
  (func $init (param "config" string) (result (result (error string)))
    (canon lift (core func $init-core) (memory $memory) (realloc $realloc)))
  (instance $lifecycle (export "init" (func $init)))
  (export "omuzaki:plugin/lifecycle@0.1.0" (instance $lifecycle))
)"#
    )
}

/// `init` returning `ok`: the result area at address 16 holds tag 0.
pub const INIT_OK: &str = r#"(func (export "init") (param i32 i32) (result i32)
      (i32.store8 (i32.const 16) (i32.const 0))
      (i32.const 16))"#;

/// A core module body: `init` runs `body`, then returns `ok`.
pub fn init_doing(body: &str) -> String {
    format!(
        r#"(func (export "init") (param i32 i32) (result i32)
      {body}
      (i32.store8 (i32.const 16) (i32.const 0))
      (i32.const 16))"#
    )
}

/// The smallest plugin: `init` returns `ok`.
pub fn ok_component() -> Vec<u8> {
    component("", &[], INIT_OK, 1)
}

/// A plugin whose `init` runs `body` and imports only memory.
pub fn plain(body: &str) -> Vec<u8> {
    component("", &[], &init_doing(body), 1)
}

/// A manifest for `id` with `api = "^0.1"`.
pub fn manifest(id: &str) -> Manifest {
    let text = format!(
        "id = \"{id}\"\nversion = \"0.1.0\"\napi = \"^0.1\"\nlicence = \"MIT\"\nname = \"{id}\"\ncomponent = \"plugin.wasm\"\n"
    );
    Manifest::parse(&text).unwrap()
}

/// An engine on a fake clock at 100 s.
pub fn engine_with(config: HostConfig) -> (PluginEngine, Arc<FakeClock>) {
    let clock = Arc::new(FakeClock::new(Seconds(100.0)));
    let engine = PluginEngine::new(config, clock.clone()).unwrap();
    (engine, clock)
}

/// [`engine_with`] the default config.
pub fn engine() -> (PluginEngine, Arc<FakeClock>) {
    engine_with(HostConfig::default())
}

/// A log for `id` on `clock`.
pub fn log(id: &str, clock: Arc<dyn Clock>) -> Arc<PluginLog> {
    Arc::new(PluginLog::new(PluginId::new(id).unwrap(), clock, None))
}

/// A worker setup with `log`, the insecure seed `seed` and no requests.
pub fn setup(log: Arc<PluginLog>, seed: u8) -> InstanceSetup {
    InstanceSetup {
        kind: InstanceKind::Worker,
        log,
        bridge: Arc::new(NoBridge),
        insecure_seed: [seed; 32],
        secure_seed: None,
        data_requested: false,
        net_requested: false,
    }
}

/// Compiles and starts `bytes` as a worker, panicking on a refusal.
pub fn start(engine: &PluginEngine, bytes: &[u8], log: Arc<PluginLog>) -> Result<LoadedPlugin, StartError> {
    let component: Component = omuzaki_plugin::load::compile(engine, bytes).unwrap();
    LoadedPlugin::start(engine, &component, setup(log, 1))
}
