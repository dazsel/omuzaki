//! Mock of the plugin contract (PL-01, ADR 0058, `docs/design/plugins.md` §1, §12).
//!
//! - [`MockPlugins`]: an in-memory [`Plugins`](omuzaki_plugin_api::Plugins) service that runs [`MockGuest`]s, with no
//!   wasm and no engine, on the runner's clock.
//! - [`MockGuest`]: a plugin written as Rust closures against the guest interface ([`GuestHost`]), or a script of op
//!   calls ([`MockGuest::scripted`]).
//! - [`MockSessions`]: an in-memory session registry, so a host opens and ends real sessions in tests.
//! - [`conformance`]: the cases every plugin host passes, parameterised over a [`Subject`](conformance::Subject).
//!   [`MockSubject`] is the mock's own.
#![deny(missing_docs)]

pub mod conformance;
mod fake;
mod guest;
mod sessions;

pub use fake::{CallRecord, MockOp, MockPlugins};
pub use guest::{GuestHost, GuestResult, MockGuest, ScriptedCall, Trap};
pub use sessions::MockSessions;

use std::time::Duration;

use omuzaki_ops_api::Grants;
use omuzaki_plugin_api::{FaultKind, LogLevel, Manifest, PluginError};

use conformance::{Behaviour, OpSubject, Subject, World};

/// How long [`Behaviour::Slow`] works in the mock.
const SLOW_INIT: Duration = Duration::from_millis(50);

/// The mock as a conformance [`Subject`]: each [`Behaviour`] as closures, for a person holding every scope.
pub struct MockSubject;

impl MockSubject {
    /// The guest that acts out `behaviour`.
    pub fn guest(behaviour: Behaviour) -> MockGuest {
        match behaviour {
            Behaviour::Ok => MockGuest::new(|_, _| Ok(Ok(()))),
            Behaviour::InitError(text) => MockGuest::new(move |_, _| Ok(Err(text.clone()))),
            Behaviour::Trap => MockGuest::new(|_, _| Err(Trap(FaultKind::Trap))),
            Behaviour::Loop => MockGuest::new(|_, _| Err(Trap(FaultKind::OutOfFuel))),
            Behaviour::Logs(n) => MockGuest::new(move |host, _| {
                for i in 0..n {
                    host.log(LogLevel::Info, &format!("line {i}"));
                }
                Ok(Ok(()))
            }),
            Behaviour::Slow => MockGuest::new(|host, _| {
                std::thread::sleep(SLOW_INIT);
                host.log(LogLevel::Info, "init done");
                Ok(Ok(()))
            }),
            Behaviour::Calls(script) => MockGuest::scripted(script),
        }
    }
}

impl Subject for MockSubject {
    type Service = MockPlugins;

    fn build(&self, world: &World, plugins: Vec<(Manifest, Behaviour)>) -> MockPlugins {
        let service = MockPlugins::new(world.clock.clone(), world.sessions.clone(), Grants::person());
        for (manifest, behaviour) in plugins {
            if let Err(error) = service.add(manifest, Self::guest(behaviour)) {
                panic!("the suite's plugins install: {error}");
            }
        }
        service
    }
}

impl OpSubject for MockSubject {
    fn add_op(&self, service: &MockPlugins, name: &str, op: MockOp) {
        service.add_op(name, op);
    }

    fn resolve(&self, service: &MockPlugins, approval: u64, approve: bool) -> Result<(), PluginError> {
        service.resolve(approval, approve, &conformance::person())
    }
}
