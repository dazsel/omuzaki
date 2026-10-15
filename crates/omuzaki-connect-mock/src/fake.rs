//! [`FakeConnector`]: one type for every capability. A [`Generator`] makes the bytes; the [`Script`] decides when and
//! how the run goes wrong. A fake never touches a transport.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use omuzaki_connect_api::{
    Capability, ConnectError, Connector, ConnectorManifest, Estimate, Output, Request, RunCx, RunOutput, Target, Usage,
    estimate_from_manifest,
};
use serde_json::json;

use crate::script::Script;
use crate::{gen_audio, gen_image, gen_mesh, gen_motion, gen_texture};

/// Makes the output of a fake: deterministic in (request, index), with no clock and no randomness beyond the seed.
pub trait Generator: Send + Sync {
    /// The capability it makes.
    fn capability(&self) -> Capability;

    /// Output `index` for `req`.
    ///
    /// # Errors
    /// [`ConnectError::Unsupported`] for a request of another capability.
    fn make(&self, req: &Request, index: u32) -> Result<Output, ConnectError>;
}

/// What a fake was asked to do, for tests to assert on.
#[derive(Debug, Default)]
pub struct Counters {
    /// Calls of `run`.
    pub runs: AtomicU32,
    /// Calls of `estimate`.
    pub estimates: AtomicU32,
    /// Calls of `ping`.
    pub pings: AtomicU32,
    /// Requests sent through a transport: always 0, a fake never touches one.
    pub http_calls: AtomicU32,
}

/// A connector that makes deterministic outputs in code, following its [`Script`].
pub struct FakeConnector {
    script: Script,
    generator: Box<dyn Generator>,
    counters: Arc<Counters>,
}

impl FakeConnector {
    /// A fake over `generator`.
    pub fn new(generator: Box<dyn Generator>, script: Script) -> Self {
        Self { script, generator, counters: Arc::default() }
    }

    /// PNG images.
    pub fn image(script: Script) -> Self {
        Self::new(Box::new(gen_image::ImageGen), script)
    }

    /// PNG textures, tileable on request.
    pub fn texture(script: Script) -> Self {
        Self::new(Box::new(gen_texture::TextureGen), script)
    }

    /// One-triangle GLB models.
    pub fn model3d(script: Script) -> Self {
        Self::new(Box::new(gen_mesh::MeshGen), script)
    }

    /// Sine-wave speech as WAV.
    pub fn voice(script: Script) -> Self {
        Self::new(Box::new(gen_audio::VoiceGen), script)
    }

    /// Two-note music or a noise burst as WAV.
    pub fn music(script: Script) -> Self {
        Self::new(Box::new(gen_audio::MusicGen), script)
    }

    /// BVH motion.
    pub fn motion(script: Script) -> Self {
        Self::new(Box::new(gen_motion::MotionGen), script)
    }

    /// DCC calls answered with a JSON echo of the request.
    pub fn dcc(script: Script) -> Self {
        Self::new(Box::new(DccGen), script)
    }

    /// The shared counters, still readable after the fake is boxed or registered.
    pub fn counters(&self) -> Arc<Counters> {
        Arc::clone(&self.counters)
    }
}

/// How a run ends early: with no output it is an error; with some, they are kept and the error says why it stopped.
fn stop(outputs: Vec<Output>, usage: Usage, error: ConnectError) -> Result<RunOutput, ConnectError> {
    if outputs.is_empty() { Err(error) } else { Ok(RunOutput { outputs, usage, stopped: Some(error) }) }
}

impl Connector for FakeConnector {
    fn estimate(&self, m: &ConnectorManifest, _: &Target, req: &Request) -> Result<Estimate, ConnectError> {
        self.counters.estimates.fetch_add(1, Ordering::Relaxed);
        estimate_from_manifest(m, req)
    }

    /// For each output in order: stop if cancelled; wait the script's latency; stop with `CapReached` if its price
    /// would pass the cap; stop with the scripted failure at its index; make it; report its price to the meter
    /// (stopping, the output kept, if the meter refuses).
    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        self.counters.runs.fetch_add(1, Ordering::Relaxed);
        let capability = req.capability();
        if m.declares(capability).is_none() || capability != self.generator.capability() {
            return Err(ConnectError::unsupported(format!("{} does not make {capability}", m.id)));
        }
        let price = self.script.usd_micros_per_output;
        let mut outputs = Vec::new();
        let mut usage = Usage::default();
        for index in 0..req.count() {
            if cx.progress.is_cancelled() {
                return stop(outputs, usage, ConnectError::Cancelled);
            }
            if self.script.latency_s > 0.0 {
                cx.pace.wait(self.script.latency_s, cx.progress);
            }
            let needed = usage.usd_micros.saturating_add(price);
            if needed > cx.cap_usd_micros {
                return stop(outputs, usage, ConnectError::cap_reached(cx.cap_usd_micros, needed));
            }
            if let Some((at, error)) = &self.script.fail
                && *at == index
            {
                return stop(outputs, usage, error.clone());
            }
            let output = match self.generator.make(req, index) {
                Ok(output) => output,
                Err(error) => return stop(outputs, usage, error),
            };
            outputs.push(output);
            if price > 0 {
                usage.usd_micros = needed;
                if let Err(error) = cx.meter.charge(price) {
                    return stop(outputs, usage, error);
                }
            }
        }
        Ok(RunOutput { outputs, usage, stopped: None })
    }

    fn ping(&self, _: &ConnectorManifest, _: &RunCx<'_>) -> Result<(), ConnectError> {
        self.counters.pings.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// The seed of output `index`: the request's seed (0 when it has none) plus the index.
pub(crate) fn seed_of(req: &Request, index: u32) -> u64 {
    req.seed().unwrap_or(0).wrapping_add(u64::from(index))
}

/// The error a generator gives for a request of another capability.
pub(crate) fn wrong_kind(want: Capability, req: &Request) -> ConnectError {
    ConnectError::unsupported(format!("this fake makes {want}, not {}", req.capability()))
}

/// DCC calls: `{"tool", "action", "echo": args}`, no bytes.
struct DccGen;

impl Generator for DccGen {
    fn capability(&self) -> Capability {
        Capability::Dcc
    }

    fn make(&self, req: &Request, _: u32) -> Result<Output, ConnectError> {
        let Request::Dcc(r) = req else {
            return Err(wrong_kind(Capability::Dcc, req));
        };
        Ok(Output::Json(json!({ "tool": r.tool, "action": r.action, "echo": r.args })))
    }
}
