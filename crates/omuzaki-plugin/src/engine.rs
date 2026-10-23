//! The engine every plugin shares: wasmtime with fuel and epoch interruption, its epoch ticker, the runner's clock and
//! the all-plugins memory budget (design §4.3, §5.2).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_core::time::Clock;
use omuzaki_plugin_api::PluginError;
use omuzaki_plugin_api::limits::{ALL_PLUGINS_EDITOR_BYTES, MAX_WASM_STACK_BYTES, WORKER_MEMORY_BYTES};

use crate::limits::MemoryBudget;

/// How often the epoch ticks, in milliseconds, so an epoch deadline in milliseconds is a count of ticks.
pub const EPOCH_TICK_MS: u64 = 1;

/// [`EPOCH_TICK_MS`] as a duration.
pub const EPOCH_TICK: Duration = Duration::from_millis(EPOCH_TICK_MS);

/// The host's knobs. The defaults are the design's numbers; tests lower the memory caps to reach them quickly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostConfig {
    /// Linear memory of a worker instance.
    pub memory_bytes: u64,
    /// Linear memory and host side of every running plugin together.
    pub all_plugins_bytes: u64,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self { memory_bytes: WORKER_MEMORY_BYTES, all_plugins_bytes: ALL_PLUGINS_EDITOR_BYTES }
    }
}

/// The ticker thread: increments the engine's epoch every [`EPOCH_TICK`] until dropped, then is joined.
struct Ticker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            // The thread only sleeps and ticks; a panic there has nothing to report beyond itself.
            let _joined = thread.join();
        }
    }
}

/// The shared engine. Dropping it stops the ticker; no thread outlives it.
pub struct PluginEngine {
    engine: wasmtime::Engine,
    config: HostConfig,
    clock: Arc<dyn Clock>,
    memory: MemoryBudget,
    _ticker: Ticker,
}

impl PluginEngine {
    /// An engine on the runner's `clock` (a `FakeClock` in tests).
    ///
    /// # Errors
    /// [`PluginError::Instantiate`] when wasmtime refuses the configuration or the ticker can't start.
    pub fn new(config: HostConfig, clock: Arc<dyn Clock>) -> Result<Self, PluginError> {
        let mut wasm = wasmtime::Config::new();
        wasm.wasm_component_model(true)
            .consume_fuel(true)
            .epoch_interruption(true)
            .max_wasm_stack(MAX_WASM_STACK_BYTES);
        let engine = wasmtime::Engine::new(&wasm).map_err(|e| PluginError::Instantiate(e.to_string()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let engine = engine.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("plugin-epoch".into())
                .spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        std::thread::sleep(EPOCH_TICK);
                        engine.increment_epoch();
                    }
                })
                .map_err(|e| PluginError::Instantiate(format!("the epoch ticker did not start: {e}")))?
        };
        let memory = MemoryBudget::new(config.all_plugins_bytes);
        Ok(Self { engine, config, clock, memory, _ticker: Ticker { stop, thread: Some(thread) } })
    }

    /// The wasmtime engine.
    pub fn wasmtime(&self) -> &wasmtime::Engine {
        &self.engine
    }

    /// The knobs it was built with.
    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    /// The runner's clock.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// The memory every running plugin shares.
    pub fn memory(&self) -> &MemoryBudget {
        &self.memory
    }
}
