//! The [`Connector`] trait every adapter implements, and what it sees while it runs ([`RunCx`]): a transport, its
//! inputs, progress, cancellation and a [`Meter`]; no store, no keychain and no document (least privilege, D6).
//!
//! Everything here is cold: a connector runs on a job worker, blocks, allocates and may take minutes. Nothing here is
//! called on a real-time path.

use std::sync::Arc;

use omuzaki_ai_api::Progress;
use omuzaki_core::time::Clock;
use omuzaki_http_api::{Cancel, HttpTransport};
use omuzaki_ops_api::{Service, Services};
use omuzaki_store_api::ContentHash;

use crate::config::{ConnectorConfig, Target};
use crate::estimate::{Estimate, estimate_from_manifest};
use crate::manifest::ConnectorManifest;
use crate::output::{ConnectError, RunOutput};
use crate::request::Request;

/// How a connector waits between polls: real time in the app, a fake clock in tests (no test sleeps).
pub trait Pace: Send + Sync {
    /// Waits `seconds`, returning early once `progress` is cancelled.
    fn wait(&self, seconds: f64, progress: &dyn Progress);
}

/// The bytes of an asset the request names. The host reads them from the store, after its checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputAsset {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// Their media type.
    pub media_type: String,
}

/// How a connector reads its inputs: only the assets in [`Request::input_assets`]; any other hash is
/// [`ConnectError::Refused`]. A connector has no other way to the store, so an adapter bug cannot read or leak a
/// project's other assets.
pub trait Inputs: Send + Sync {
    /// The asset `hash`.
    ///
    /// # Errors
    /// [`ConnectError::Refused`] for a hash the request does not name; [`ConnectError::Failed`] if it cannot be read.
    fn get(&self, hash: &ContentHash) -> Result<InputAsset, ConnectError>;
}

/// Where a billable request is reported the moment it completes, before anything is stored (D4.6): the job wires it to
/// `JobCtx::spend`, so a store failure or a cancel afterwards cannot lose a charge. The sum a run reports equals its
/// [`Usage::usd_micros`](crate::Usage::usd_micros).
pub trait Meter: Send + Sync {
    /// Reports `usd_micros` just billed.
    ///
    /// # Errors
    /// [`ConnectError::CapReached`] once the total would pass the job's cap; the connector then stops before its next
    /// billable request, keeping what it has.
    fn charge(&self, usd_micros: u64) -> Result<(), ConnectError>;
}

/// What a run gets from its job, besides the call (D4).
pub struct RunEnv<'a> {
    /// Progress and the borrowed cancel check.
    pub progress: &'a dyn Progress,
    /// The owned cancel handle, for work that outlives a borrow ([`HttpTransport::open`]'s streams).
    pub cancel: Arc<dyn Cancel>,
    /// Where charges go.
    pub meter: &'a dyn Meter,
}

/// The empty set [`RunCx::new`] points at, so a context costs nothing to build.
static NO_SERVICES: Services = Services::none();

/// What a connector sees while it runs. It has no store, no secrets and no document: the host owns those.
pub struct RunCx<'a> {
    /// The target the run resolved once ([`Connector::target`]).
    pub target: &'a Target,
    /// The HTTP transport. The host's attaches the key and allows only the target's hosts; a connector never sets or
    /// reads an auth header.
    pub http: &'a dyn HttpTransport,
    /// The request's input assets.
    pub inputs: &'a dyn Inputs,
    /// Progress and the cancel check: checked between requests and polls.
    pub progress: &'a dyn Progress,
    /// The owned cancel handle, for [`HttpTransport::open`].
    pub cancel: Arc<dyn Cancel>,
    /// Where each billable request is reported as it completes.
    pub meter: &'a dyn Meter,
    /// How to wait between polls.
    pub pace: &'a dyn Pace,
    /// The clock.
    pub clock: &'a dyn Clock,
    /// The most this call may bill, micro-dollars. A connector checks before each billable request and stops with
    /// [`ConnectError::CapReached`].
    pub cap_usd_micros: u64,
    services: &'a Services,
}

impl<'a> RunCx<'a> {
    /// A context with no services.
    pub fn new(
        target: &'a Target,
        http: &'a dyn HttpTransport,
        inputs: &'a dyn Inputs,
        env: &RunEnv<'a>,
        pace: &'a dyn Pace,
        clock: &'a dyn Clock,
        cap_usd_micros: u64,
    ) -> Self {
        Self {
            target,
            http,
            inputs,
            progress: env.progress,
            cancel: Arc::clone(&env.cancel),
            meter: env.meter,
            pace,
            clock,
            cap_usd_micros,
            services: &NO_SERVICES,
        }
    }

    /// The same context with the app's services (a registry session for the local TTS bridge).
    #[must_use]
    pub fn with_services(mut self, services: &'a Services) -> Self {
        self.services = services;
        self
    }

    /// The service `S`, if the app provided one.
    pub fn service<S: ?Sized + Service>(&self) -> Option<&'a S> {
        self.services.get::<S>()
    }

    /// The person's settings, from the target.
    pub fn config(&self) -> &'a ConnectorConfig {
        &self.target.config
    }
}

/// One connector: an adapter for one service or program. Implementations self-register with
/// [`register_connector!`](crate::register_connector). Every method is cold.
pub trait Connector: Send + Sync {
    /// Where and how a call goes, resolved once per run from `config`. The default suits HTTP, MCP, local and fake
    /// connectors ([`Target::from_manifest`]); the LLM wrapper overrides it to read its provider through `services`,
    /// the only place a connector reads settings.
    ///
    /// # Errors
    /// [`ConnectError::Unavailable`] naming the setting that is missing or wrong.
    fn target(
        &self,
        m: &ConnectorManifest,
        config: &ConnectorConfig,
        services: &Services,
    ) -> Result<Target, ConnectError> {
        let _ = services;
        Target::from_manifest(m, config)
    }

    /// Money and time for `req` on `t`. Pure and offline: no I/O, deterministic. The default reads the manifest's
    /// cost model ([`estimate_from_manifest`]).
    ///
    /// # Errors
    /// [`ConnectError::Unsupported`] when the request cannot be priced.
    fn estimate(&self, m: &ConnectorManifest, t: &Target, req: &Request) -> Result<Estimate, ConnectError> {
        let _ = t;
        estimate_from_manifest(m, req)
    }

    /// Does the work. Blocking, on a job worker. Checks `cx.progress.is_cancelled()` between requests, checks the
    /// cap before each billable request, reports each through `cx.meter` as it completes, keeps complete outputs on
    /// cancel or error (`stopped: Some(..)`), and never sets or reads an auth header.
    ///
    /// # Errors
    /// A [`ConnectError`] when nothing came back; [`ConnectError::Unsupported`] for a capability it does not declare.
    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError>;

    /// A free reachability check (`connect.test`): no billing, no outputs.
    ///
    /// # Errors
    /// What stops the service from being reached.
    fn ping(&self, m: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError>;
}
