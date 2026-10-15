//! `MockGuest`: a plugin written as Rust closures against the guest interface of the WIT world, with no wasm.

use omuzaki_ops_api::OpError;
use omuzaki_plugin_api::{FaultKind, LogLevel};
use serde::{Deserialize, Serialize};

/// What a guest can ask of its host: the base world's `ops` and `host` imports.
pub trait GuestHost {
    /// `ops.call`: runs an op as the plugin's session; `args` and the result are JSON.
    ///
    /// # Errors
    /// The op's error, or the refusal of the ceiling, queueing or a revoked session.
    fn call(&mut self, op: &str, args: &str) -> Result<String, OpError>;

    /// `host.log`.
    fn log(&mut self, level: LogLevel, message: &str);

    /// `host.monotonic-seconds`: the runner's clock.
    fn monotonic_seconds(&mut self) -> f64;
}

/// How a guest call ends when it does not return: a trap the host turns into a fault.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Trap(pub FaultKind);

/// What a guest export returns: the export's own value, or a trap.
pub type GuestResult<T> = Result<T, Trap>;

type InitFn = dyn Fn(&mut dyn GuestHost, &str) -> GuestResult<Result<(), String>> + Send + Sync;
type ResolvedFn = dyn Fn(&mut dyn GuestHost, u64, Result<String, OpError>) -> GuestResult<()> + Send + Sync;
type ShutdownFn = dyn Fn(&mut dyn GuestHost) -> GuestResult<()> + Send + Sync;

/// One op a scripted guest calls: what the plugin asks of the runner when it runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptedCall {
    /// The op's name.
    pub op: String,
    /// Its arguments.
    pub args: serde_json::Value,
}

/// A plugin as closures: `init` is required, `approval-resolved` and `shutdown` default to doing nothing, as the Rust
/// guest SDK's defaults do (design §1.1).
pub struct MockGuest {
    init: Box<InitFn>,
    approval_resolved: Box<ResolvedFn>,
    shutdown: Box<ShutdownFn>,
}

impl MockGuest {
    /// A guest whose `init` is `init`.
    pub fn new(
        init: impl Fn(&mut dyn GuestHost, &str) -> GuestResult<Result<(), String>> + Send + Sync + 'static,
    ) -> Self {
        Self { init: Box::new(init), approval_resolved: Box::new(|_, _, _| Ok(())), shutdown: Box::new(|_| Ok(())) }
    }

    /// A guest whose `init` calls each op of `script` in order, logs each outcome at `info` (`<op>: ok` or
    /// `<op>: <code>`) and returns `ok`; a resolved approval is logged the same way (`approval <n>: ok`).
    pub fn scripted(script: Vec<ScriptedCall>) -> Self {
        Self::new(move |host, _| {
            for call in &script {
                let line = match host.call(&call.op, &call.args.to_string()) {
                    Ok(_) => format!("{}: ok", call.op),
                    Err(error) => format!("{}: {}", call.op, error.code),
                };
                host.log(LogLevel::Info, &line);
            }
            Ok(Ok(()))
        })
        .on_approval_resolved(|host, approval, outcome| {
            let line = match outcome {
                Ok(_) => format!("approval {approval}: ok"),
                Err(error) => format!("approval {approval}: {}", error.code),
            };
            host.log(LogLevel::Info, &line);
            Ok(())
        })
    }

    /// The same guest with `approval-resolved` set.
    #[must_use]
    pub fn on_approval_resolved(
        mut self,
        f: impl Fn(&mut dyn GuestHost, u64, Result<String, OpError>) -> GuestResult<()> + Send + Sync + 'static,
    ) -> Self {
        self.approval_resolved = Box::new(f);
        self
    }

    /// The same guest with `shutdown` set.
    #[must_use]
    pub fn on_shutdown(mut self, f: impl Fn(&mut dyn GuestHost) -> GuestResult<()> + Send + Sync + 'static) -> Self {
        self.shutdown = Box::new(f);
        self
    }

    /// Calls `init`.
    ///
    /// # Errors
    /// The guest's trap.
    pub fn init(&self, host: &mut dyn GuestHost, config: &str) -> GuestResult<Result<(), String>> {
        (self.init)(host, config)
    }

    /// Calls `approval-resolved`.
    ///
    /// # Errors
    /// The guest's trap.
    pub fn approval_resolved(
        &self,
        host: &mut dyn GuestHost,
        approval: u64,
        outcome: Result<String, OpError>,
    ) -> GuestResult<()> {
        (self.approval_resolved)(host, approval, outcome)
    }

    /// Calls `shutdown`.
    ///
    /// # Errors
    /// The guest's trap.
    pub fn shutdown(&self, host: &mut dyn GuestHost) -> GuestResult<()> {
        (self.shutdown)(host)
    }
}
