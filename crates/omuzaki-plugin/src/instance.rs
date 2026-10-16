//! One instance of a plugin's component: its store, its limits, and the entry calls that run under fuel, an epoch
//! deadline and the CPU share (design §4.2 to §4.4).

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use omuzaki_core::time::Clock;
use omuzaki_plugin_api::PluginError;
use omuzaki_plugin_api::limits::{
    CallKind, HOST_SIDE_BYTES, IMPORT_FUEL, IMPORT_FUEL_PER_BYTE, INIT_CONFIG_BYTES_MAX, NODE_MEMORY_BYTES,
    OP_BYTES_MAX,
};
use omuzaki_plugin_api::state::FaultKind;
use wasmtime::component::{Component, ComponentExportIndex, Instance, Linker, TypedFunc};
use wasmtime::{Store, StoreContextMut};

use crate::bridge::{OpBridge, WitOpError};
use crate::engine::PluginEngine;
use crate::limits::{CpuShare, HostCallBucket, Limiter, Reservation};
use crate::log::PluginLog;
use crate::{imports, wasi};

/// Which kind of instance: a plugin's worker, or a node instance on the automation engine thread (§5.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InstanceKind {
    /// The worker instance: every entry call of the plugin, under its own session.
    Worker,
    /// A node instance (PL-03): 16 MiB, `ops`, `data` and `http` linked to refusals, a `poll` that never waits, and
    /// no OS clock or entropy. Until PL-03 brings `NodeCx`, its clocks read the runner's clock and its randomness is
    /// the seeded stream.
    Node,
}

/// Why a call stopped, as the plugin's fault: the kind and one plain sentence naming the entry
/// ("out of fuel in init").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    /// What went wrong.
    pub kind: FaultKind,
    /// The sentence for the session's end reason and the manager.
    pub detail: String,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

/// A trap a host import raises on purpose: a breached deadline, a flood, an exit, an oversized request.
#[derive(Debug)]
pub(crate) struct HostTrap(pub Fault);

impl fmt::Display for HostTrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.detail)
    }
}

impl std::error::Error for HostTrap {}

/// Builds the error a host import returns to trap the guest with `kind` and `detail`.
pub(crate) fn trap(kind: FaultKind, detail: impl Into<String>) -> wasmtime::Error {
    wasmtime::Error::new(HostTrap(Fault { kind, detail: detail.into() }))
}

/// The running entry call.
#[derive(Copy, Clone, Debug)]
pub(crate) struct CallState {
    pub kind: CallKind,
    pub deadline: Instant,
}

/// What a store holds besides the instance.
pub(crate) struct StoreData {
    pub limiter: Limiter,
    pub clock: Arc<dyn Clock>,
    pub log: Arc<PluginLog>,
    pub bridge: Arc<dyn OpBridge>,
    pub bucket: HostCallBucket,
    pub kind: InstanceKind,
    pub call: Option<CallState>,
    /// False once the session has ended (in `shutdown`, after a revocation): `ops`, `data` and `http` refuse.
    pub effectful: bool,
    /// Whether the manifest asked for `data` and `net`; unrequested ones are refusals.
    pub data_requested: bool,
    pub net_requested: bool,
    /// The `wasi:io/poll` pollables, by handle: `None` once dropped, else when it is ready on the runner's clock.
    pub pollables: Vec<Option<wasi::Wait>>,
    /// The seeded `insecure` stream (§4.1).
    pub insecure: ChaCha20,
    /// A seeded stream standing in for OS entropy: tests, and node instances, which never reach the OS.
    pub secure_seeded: Option<ChaCha20>,
}

impl StoreData {
    /// The entry running now, for fault sentences.
    pub fn entry(&self) -> &'static str {
        self.call.map_or("a call", |c| c.kind.entry())
    }
}

/// Every import starts here: refuse once the entry's deadline has passed (an epoch is only seen back in wasm, so a
/// blocking import must check it itself), take a token from the host-call bucket, and charge [`IMPORT_FUEL`] plus
/// [`IMPORT_FUEL_PER_BYTE`] per byte passed either way, so the caps and the CPU share see host work. `Ok(false)` means
/// the bucket is empty and the caller decides: `call` and `plan` answer `limited`, every other import traps.
pub(crate) fn enter(cx: &mut StoreContextMut<'_, StoreData>, bytes: usize) -> wasmtime::Result<bool> {
    check_deadline(cx)?;
    let data = cx.data_mut();
    let now = data.clock.now();
    let allowed = data.bucket.take(now);
    let charge = IMPORT_FUEL.saturating_add(IMPORT_FUEL_PER_BYTE.saturating_mul(bytes as u64));
    let fuel = cx.get_fuel()?;
    if fuel < charge {
        let entry = cx.data().entry();
        cx.set_fuel(0)?;
        return Err(trap(FaultKind::OutOfFuel, format!("out of fuel in {entry}")));
    }
    cx.set_fuel(fuel - charge)?;
    Ok(allowed)
}

/// Traps when the entry call's deadline has passed.
fn check_deadline(cx: &StoreContextMut<'_, StoreData>) -> wasmtime::Result<()> {
    if let Some(call) = cx.data().call
        && Instant::now() >= call.deadline
    {
        return Err(trap(FaultKind::Timeout, format!("deadline in {}", call.kind.entry())));
    }
    Ok(())
}

/// Charges the bytes an import hands back to the guest, [`IMPORT_FUEL_PER_BYTE`] each, after checking the deadline
/// again. The call already took its host-call token and its fixed charge in [`enter`].
pub(crate) fn charge_returned(cx: &mut StoreContextMut<'_, StoreData>, bytes: usize) -> wasmtime::Result<()> {
    // The work behind the import (a bridge call, say) may have run past the entry's deadline: that is a fault before
    // its result reaches the guest (design §4.3).
    check_deadline(cx)?;
    let charge = IMPORT_FUEL_PER_BYTE.saturating_mul(bytes as u64);
    let fuel = cx.get_fuel()?;
    if fuel < charge {
        let entry = cx.data().entry();
        cx.set_fuel(0)?;
        return Err(trap(FaultKind::OutOfFuel, format!("out of fuel in {entry}")));
    }
    cx.set_fuel(fuel - charge)?;
    Ok(())
}

/// [`enter`] for an import that traps on a flood.
pub(crate) fn enter_or_trap(cx: &mut StoreContextMut<'_, StoreData>, bytes: usize) -> wasmtime::Result<()> {
    if enter(cx, bytes)? {
        Ok(())
    } else {
        let entry = cx.data().entry();
        Err(trap(FaultKind::HostCallFlood, format!("host-call flood in {entry}")))
    }
}

/// Fills `buf` from `stream`.
pub(crate) fn fill(stream: &mut ChaCha20, buf: &mut [u8]) {
    buf.fill(0);
    stream.apply_keystream(buf);
}

/// A `ChaCha20` stream keyed by `seed`.
pub(crate) fn stream(seed: &[u8; 32]) -> ChaCha20 {
    ChaCha20::new(seed.into(), &[0u8; 12].into())
}

/// What an instance is started with.
pub struct InstanceSetup {
    /// Worker or node.
    pub kind: InstanceKind,
    /// The plugin's log.
    pub log: Arc<PluginLog>,
    /// The op bridge; [`NoBridge`](crate::bridge::NoBridge) until the bridge task.
    pub bridge: Arc<dyn OpBridge>,
    /// The key of the `insecure` stream: derive it with [`insecure_seed`] from the content hash and the session.
    pub insecure_seed: [u8; 32],
    /// Test only: a seed that replaces OS entropy for `wasi:random/random`, so a test of the random interface is
    /// deterministic. `None` in the product.
    pub secure_seed: Option<[u8; 32]>,
    /// Whether the manifest asks for `data`.
    pub data_requested: bool,
    /// Whether the manifest asks for `net`.
    pub net_requested: bool,
}

/// The key of a session's `insecure` stream: BLAKE3 over a label, the package's content hash and the session id, so
/// it is deterministic (ADR 0009) and differs between sessions of one package.
pub fn insecure_seed(content_hash: &[u8; 32], session: u64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"omuzaki-plugin-insecure-v1\0");
    hasher.update(content_hash);
    hasher.update(&session.to_le_bytes());
    *hasher.finalize().as_bytes()
}

/// Why an instance did not start.
#[derive(Debug)]
pub enum StartError {
    /// Refused before running any guest code: a limit, a link error.
    Refused(PluginError),
    /// The component's start code faulted.
    Faulted(Fault),
}

/// `lifecycle.approval-resolved`.
type ApprovalResolved = TypedFunc<(u64, Result<String, WitOpError>), ()>;

/// The `lifecycle` exports.
struct Lifecycle {
    init: TypedFunc<(String,), (Result<(), String>,)>,
    approval_resolved: Option<ApprovalResolved>,
    shutdown: Option<TypedFunc<(), ()>>,
}

/// A started instance: one store, its exports, and the plugin's CPU share.
pub struct LoadedPlugin {
    store: Store<StoreData>,
    lifecycle: Lifecycle,
    share: CpuShare,
    fuel_used: u64,
}

fn linker(engine: &PluginEngine, kind: InstanceKind) -> Result<Linker<StoreData>, PluginError> {
    let mut linker = Linker::new(engine.wasmtime());
    imports::add(&mut linker, kind).map_err(|e| PluginError::Instantiate(format!("{e:#}")))?;
    wasi::add(&mut linker, kind).map_err(|e| PluginError::Instantiate(format!("{e:#}")))?;
    Ok(linker)
}

fn lookup<P, R>(
    store: &mut Store<StoreData>,
    instance: &Instance,
    iface: &ComponentExportIndex,
    name: &str,
) -> Option<TypedFunc<P, R>>
where
    P: wasmtime::component::ComponentNamedList + wasmtime::component::Lower,
    R: wasmtime::component::ComponentNamedList + wasmtime::component::Lift,
{
    let index = instance.get_export_index(&mut *store, Some(iface), name)?;
    instance.get_typed_func::<P, R>(&mut *store, &index).ok()
}

impl LoadedPlugin {
    /// Instantiates `component`: reserves its host side in the all-plugins budget, links the imports for `setup.kind`
    /// and runs its start code under `init`'s budget.
    ///
    /// # Errors
    /// [`StartError::Refused`] when the reservation does not fit, the imports don't link or `lifecycle.init` is
    /// missing; [`StartError::Faulted`] when the start code traps.
    pub fn start(engine: &PluginEngine, component: &Component, setup: InstanceSetup) -> Result<Self, StartError> {
        let budget = engine.memory();
        let reservation = Reservation::take(budget, HOST_SIDE_BYTES).ok_or_else(|| {
            StartError::Refused(PluginError::limit(format!(
                "every plugin's memory together ({} of {} bytes in use)",
                budget.used(),
                budget.cap()
            )))
        })?;
        let memory_bytes = match setup.kind {
            InstanceKind::Worker => engine.config().memory_bytes,
            InstanceKind::Node => NODE_MEMORY_BYTES.min(engine.config().memory_bytes),
        };
        let clock = engine.clock().clone();
        let now = clock.now();
        let secure_seeded = match setup.kind {
            InstanceKind::Node => Some(stream(&setup.insecure_seed)),
            InstanceKind::Worker => setup.secure_seed.as_ref().map(stream),
        };
        let data = StoreData {
            limiter: Limiter::new(memory_bytes, reservation),
            clock,
            log: setup.log,
            bridge: setup.bridge,
            bucket: HostCallBucket::new(now),
            kind: setup.kind,
            call: None,
            effectful: true,
            data_requested: setup.data_requested,
            net_requested: setup.net_requested,
            pollables: Vec::new(),
            insecure: stream(&setup.insecure_seed),
            secure_seeded,
        };
        let mut store = Store::new(engine.wasmtime(), data);
        store.limiter(|data| &mut data.limiter);
        let linker = linker(engine, setup.kind).map_err(StartError::Refused)?;
        // Linking first, so an import whose type disagrees is a refusal; only the start code can fault.
        let pre = linker
            .instantiate_pre(component)
            .map_err(|e| StartError::Refused(PluginError::Instantiate(format!("{e:#}"))))?;
        let mut share = CpuShare::default();
        let mut fuel_used = 0;
        let instance =
            run(&mut store, &mut share, &mut fuel_used, CallKind::Init, |store| pre.instantiate(&mut *store)).map_err(
                |fault| {
                    // A grow the all-plugins budget refused while instantiating is the budget's refusal, not a fault.
                    if store.data().limiter.refused_grow && fault.kind == FaultKind::OutOfMemory {
                        StartError::Refused(PluginError::limit("every plugin's memory together"))
                    } else {
                        StartError::Faulted(fault)
                    }
                },
            )?;
        let missing = || StartError::Refused(PluginError::Manifest("the component lacks lifecycle.init".into()));
        let iface = instance
            .get_export_index(
                &mut store,
                None,
                &format!("{}/lifecycle@{}", crate::load::PACKAGE, version_of(component)),
            )
            .ok_or_else(missing)?;
        let init = lookup(&mut store, &instance, &iface, "init").ok_or_else(missing)?;
        let approval_resolved = lookup(&mut store, &instance, &iface, "approval-resolved");
        let shutdown = lookup(&mut store, &instance, &iface, "shutdown");
        Ok(Self { store, lifecycle: Lifecycle { init, approval_resolved, shutdown }, share, fuel_used })
    }

    /// Calls `init` with the plugin's settings as JSON.
    ///
    /// # Errors
    /// The [`Fault`]: a trap, a breached budget, an `init` error ([`FaultKind::InitError`]) or a bad return value.
    pub fn init(&mut self, config: &str) -> Result<(), Fault> {
        if config.len() > INIT_CONFIG_BYTES_MAX {
            return Err(Fault {
                kind: FaultKind::BadReturn,
                detail: format!("the init config is {} bytes; the cap is {INIT_CONFIG_BYTES_MAX}", config.len()),
            });
        }
        let init = self.lifecycle.init;
        let config = config.to_owned();
        let (outcome,) = self.call(CallKind::Init, move |store| init.call(store, (config,)))?;
        outcome.map_err(|text| {
            if text.len() > OP_BYTES_MAX {
                Fault {
                    kind: FaultKind::BadReturn,
                    detail: format!("bad return value from init: {} bytes", text.len()),
                }
            } else {
                Fault { kind: FaultKind::InitError, detail: format!("init failed: {text}") }
            }
        })
    }

    /// Calls `approval-resolved`; a component without it ignores the outcome.
    ///
    /// # Errors
    /// The [`Fault`].
    pub fn approval_resolved(&mut self, approval: u64, outcome: Result<String, WitOpError>) -> Result<(), Fault> {
        let Some(func) = self.lifecycle.approval_resolved else { return Ok(()) };
        self.call(CallKind::ApprovalResolved, move |store| func.call(store, (approval, outcome)))
    }

    /// Calls `shutdown` with every effectful import denied. A breach only drops the instance, so it is returned for
    /// the log, never as a fault.
    ///
    /// # Errors
    /// What stopped `shutdown`.
    pub fn shutdown(&mut self) -> Result<(), Fault> {
        self.store.data_mut().effectful = false;
        let Some(func) = self.lifecycle.shutdown else { return Ok(()) };
        self.call(CallKind::Shutdown, move |store| func.call(store, ()))
    }

    /// The fuel the last call used, host-call charges included.
    pub fn fuel_used(&self) -> u64 {
        self.fuel_used
    }

    /// Fuel left of the plugin's CPU share this second of the runner's clock.
    pub fn share_left(&mut self) -> u64 {
        let now = self.store.data().clock.now();
        self.share.left(now)
    }

    /// Bytes this instance holds in the all-plugins budget: its host side and its linear memory.
    pub fn reserved_bytes(&self) -> u64 {
        self.store.data().limiter.reserved()
    }

    /// Turns `ops`, `data` and `http` into refusals from the next import on: the session has ended.
    pub fn deny_effects(&mut self) {
        self.store.data_mut().effectful = false;
    }

    fn call<R>(
        &mut self,
        kind: CallKind,
        f: impl FnOnce(&mut Store<StoreData>) -> wasmtime::Result<R>,
    ) -> Result<R, Fault> {
        run(&mut self.store, &mut self.share, &mut self.fuel_used, kind, f)
    }
}

/// The version of `omuzaki:plugin` a component exports `lifecycle` at.
fn version_of(component: &Component) -> String {
    let engine = component.engine();
    let prefix = format!("{}/lifecycle@", crate::load::PACKAGE);
    component
        .component_type()
        .exports(engine)
        .find_map(|(name, _)| name.strip_prefix(prefix.as_str()).map(str::to_owned))
        .unwrap_or_default()
}

/// Runs `f` as one entry call of `kind`: fuel is the smaller of the kind's cap and the CPU share left, the epoch
/// deadline is the kind's, and whatever stops it becomes a [`Fault`] naming the entry.
fn run<R>(
    store: &mut Store<StoreData>,
    share: &mut CpuShare,
    fuel_used: &mut u64,
    kind: CallKind,
    f: impl FnOnce(&mut Store<StoreData>) -> wasmtime::Result<R>,
) -> Result<R, Fault> {
    let budget = kind.budget();
    let clock = store.data().clock.clone();
    let fuel = budget.fuel_within(share.left(clock.now()));
    let started = Instant::now();
    let deadline = started + std::time::Duration::from_millis(budget.deadline_ms);
    {
        let data = store.data_mut();
        data.call = Some(CallState { kind, deadline });
        data.limiter.refused_grow = false;
    }
    let setup = store.set_fuel(fuel).map(|()| {
        store.set_epoch_deadline(budget.deadline_ms.div_ceil(crate::engine::EPOCH_TICK_MS));
        store.epoch_deadline_trap();
    });
    let result = setup.and_then(|()| f(store));
    let left = store.get_fuel().unwrap_or(0);
    *fuel_used = fuel.saturating_sub(left);
    share.spend(clock.now(), *fuel_used);
    let refused_grow = store.data().limiter.refused_grow;
    store.data_mut().call = None;
    result.map_err(|error| classify(&error, kind, fuel < budget.fuel, refused_grow))
}

/// The fault an error from a call means.
fn classify(error: &wasmtime::Error, kind: CallKind, share_capped: bool, refused_grow: bool) -> Fault {
    let entry = kind.entry();
    if let Some(HostTrap(fault)) = error.downcast_ref::<HostTrap>() {
        return fault.clone();
    }
    let (fault, detail) = match error.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel) if share_capped => {
            (FaultKind::OutOfFuel, format!("CPU share used up in {entry}"))
        }
        Some(wasmtime::Trap::OutOfFuel) => (FaultKind::OutOfFuel, format!("out of fuel in {entry}")),
        Some(wasmtime::Trap::Interrupt) => (FaultKind::Timeout, format!("deadline in {entry}")),
        Some(wasmtime::Trap::StackOverflow) => (FaultKind::Trap, format!("stack overflow in {entry}")),
        _ if refused_grow => (FaultKind::OutOfMemory, format!("out of memory in {entry}")),
        Some(other) => (FaultKind::Trap, format!("trap in {entry}: {other}")),
        None => (FaultKind::BadReturn, format!("bad return value from {entry}: {error}")),
    };
    Fault { kind: fault, detail }
}
