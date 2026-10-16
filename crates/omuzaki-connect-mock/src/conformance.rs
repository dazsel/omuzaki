//! The conformance suite (D4, D11): an independent set of checks every connector's own tests run against its protocol
//! fake, so "a connector" means the same thing everywhere. Like `omuzaki-store-mock::conformance`, each check is
//! named, and a connector that breaks one rule fails exactly that check.
//!
//! The suite drives the connector itself: it resolves the target once, builds every run context, wraps the
//! transport to see what the connector sends, and swaps in its own failing transport for the error mapping.
//!
//! ```ignore
//! let http = omuzaki_http_mock::FakeHttp::new().json(Method::Post, URL, 200, reply);
//! let seen = || http.calls().len() + http.unmatched().len();
//! let env = Env::new(&http, &seen, &inputs, vec![request]);
//! assert_eq!(conformance::check(&MyConnector::default(), &manifest, &env), vec![]);
//! ```

use std::collections::BTreeMap;
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};

use omuzaki_ai_api::Progress;
use omuzaki_connect_api::{
    AuthSpec, Capability, ChatMessage, ConnectError, Connector, ConnectorConfig, ConnectorManifest, DccRequest,
    Deliver, ImageRequest, Inputs, LlmRequest, MediaKind, Model3dRequest, MotionRequest, MotionSource, MusicRequest,
    Output, Pace, Request, Role, RunCx, RunEnv, RunOutput, Target, TextureRequest, Transport, VisionRequest,
    VoiceChoice, VoiceRequest, usd,
};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_http_api::{Cancel, CancelFlag, HttpError, HttpRequest, HttpResponse, HttpStream, HttpTransport};
use omuzaki_ops_api::Services;
use omuzaki_store_api::ContentHash;

use crate::meter::RecordingMeter;

/// The check names, in the order they run.
pub const CHECKS: [&str; 11] = [
    MANIFEST,
    ESTIMATE_IS_PURE,
    ESTIMATE_BOUNDS_USAGE,
    USAGE_IS_METERED,
    METER_REFUSAL_STOPS,
    CANCELLED_MAKES_NO_REQUEST,
    CAP_IS_CHECKED_FIRST,
    OUTPUTS_MATCH_MANIFEST,
    UNSUPPORTED_IS_REFUSED,
    SECRETS_STAY_OUT,
    TRANSPORT_ERRORS_MAP,
];

/// The manifest validates, its target resolves, and `env.requests` has one valid request per declared capability.
pub const MANIFEST: &str = "manifest";
/// `estimate` twice gives equal results and sends nothing.
pub const ESTIMATE_IS_PURE: &str = "estimate_is_pure";
/// A real run bills at most its estimate.
pub const ESTIMATE_BOUNDS_USAGE: &str = "estimate_bounds_usage";
/// The sum reported to the meter equals `Usage::usd_micros`.
pub const USAGE_IS_METERED: &str = "usage_is_metered";
/// A paid connector charges the meter for a request that costs something, and stops with `CapReached` once the
/// meter refuses, before its next billable request.
pub const METER_REFUSAL_STOPS: &str = "meter_refusal_stops";
/// An already cancelled run is `Err(Cancelled)` and sends nothing.
pub const CANCELLED_MAKES_NO_REQUEST: &str = "cancelled_makes_no_request";
/// A paid connector with a cap of 0 is `Err(CapReached)` and sends nothing.
pub const CAP_IS_CHECKED_FIRST: &str = "cap_is_checked_first";
/// A declared request succeeds with at least one output, and every output fits the capability's declaration, has a
/// model and a media type of its kind ([`media_type_fits`]), and the first carries the seed.
pub const OUTPUTS_MATCH_MANIFEST: &str = "outputs_match_manifest";
/// A request of an undeclared capability is `Unsupported`: no panic, no request.
pub const UNSUPPORTED_IS_REFUSED: &str = "unsupported_is_refused";
/// The probe key appears in no output, error or header, and the connector sets no auth header.
pub const SECRETS_STAY_OUT: &str = "secrets_stay_out";
/// Transport failures and error statuses map to the shared errors (`ConnectError::from_http`, `from_status`).
pub const TRANSPORT_ERRORS_MAP: &str = "transport_errors_map";

/// What the suite runs a connector with.
pub struct Env<'a> {
    /// The transport, loaded by the adapter's test with replies for `requests`.
    pub http: &'a dyn HttpTransport,
    /// How many requests `http` has received so far, by any route: how the suite sees a request sent outside the run
    /// context (`FakeHttp`'s `calls().len() + unmatched().len()`).
    pub requests_seen: &'a dyn Fn() -> usize,
    /// Holds the assets `requests` name.
    pub inputs: &'a dyn Inputs,
    /// One valid request per declared capability.
    pub requests: Vec<Request>,
    /// The settings the target is resolved from; enabled by default.
    pub config: ConnectorConfig,
    /// A key the test put in the transport's way (a reply that echoes it); it must come back nowhere.
    pub secret_probe: Option<&'a str>,
    /// False for a connector that reaches HTTP through another seam (the LLM providers): the error mapping check
    /// is skipped. It is skipped for `fake` and `local` transports too.
    pub transport_checks: bool,
}

impl<'a> Env<'a> {
    /// An environment with an enabled default config, no probe and the transport checks on.
    pub fn new(
        http: &'a dyn HttpTransport,
        requests_seen: &'a dyn Fn() -> usize,
        inputs: &'a dyn Inputs,
        requests: Vec<Request>,
    ) -> Self {
        Self {
            http,
            requests_seen,
            inputs,
            requests,
            config: ConnectorConfig { enabled: true, ..ConnectorConfig::default() },
            secret_probe: None,
            transport_checks: true,
        }
    }
}

/// One way a connector does not conform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The check, one of [`CHECKS`].
    pub check: &'static str,
    /// What went wrong.
    pub problem: String,
}

/// Runs every check; an empty list means the connector conforms. Reports every finding, not the first.
pub fn check(c: &dyn Connector, m: &ConnectorManifest, env: &Env<'_>) -> Vec<Finding> {
    let mut s = Suite { c, m, env, found: Vec::new() };
    let Some(target) = s.manifest() else {
        return s.found;
    };
    s.estimate_is_pure(&target);
    s.estimate_bounds_usage(&target);
    s.meter_refusal_stops(&target);
    s.cancelled_makes_no_request(&target);
    s.cap_is_checked_first(&target);
    s.outputs_match_manifest(&target);
    s.unsupported_is_refused(&target);
    s.secrets_stay_out(&target);
    s.transport_errors_map(&target);
    s.found
}

/// The bytes of [`sample_request`]'s vision image; a test adds them to its inputs.
pub const SAMPLE_IMAGE: &[u8] = b"conformance sample image";

/// A small valid request of `capability`, seeded where the kind has a seed. A vision request names
/// [`SAMPLE_IMAGE`]; a DCC request names the tool `dcc` and the action `run`.
pub fn sample_request(capability: Capability) -> Request {
    let seed = Some(7);
    match capability {
        Capability::Image => Request::Image(ImageRequest {
            prompt: "a test card".into(),
            width: 8,
            height: 8,
            count: 1,
            seed,
            ..ImageRequest::default()
        }),
        Capability::Texture => Request::Texture(TextureRequest {
            prompt: "a test pattern".into(),
            width: 8,
            height: 8,
            count: 1,
            seed,
            ..TextureRequest::default()
        }),
        Capability::Model3d => {
            Request::Model3d(Model3dRequest { prompt: Some("a cube".into()), seed, ..Model3dRequest::default() })
        }
        Capability::Voice => Request::Voice(VoiceRequest {
            text: "hello".into(),
            voice: VoiceChoice::preset("test"),
            language: None,
            seed,
            params: BTreeMap::new(),
        }),
        Capability::Music => Request::Music(MusicRequest {
            prompt: "a test tone".into(),
            seconds: 1,
            count: 1,
            seed,
            ..MusicRequest::default()
        }),
        Capability::Motion => Request::Motion(MotionRequest {
            source: MotionSource::prompt("a wave"),
            seconds: Some(1),
            seed,
            params: BTreeMap::new(),
        }),
        Capability::Llm => Request::Llm(LlmRequest {
            messages: vec![ChatMessage { role: Role::User, text: "Say hello.".into() }],
            max_tokens: 16,
            ..LlmRequest::default()
        }),
        Capability::Vision => Request::Vision(VisionRequest {
            instruction: "Describe it.".into(),
            images: vec![ContentHash::of(SAMPLE_IMAGE)],
            max_tokens: 16,
            ..VisionRequest::default()
        }),
        Capability::Dcc => {
            Request::Dcc(DccRequest { tool: "dcc".into(), action: "run".into(), ..DccRequest::default() })
        }
    }
}

struct Suite<'s, 'e> {
    c: &'s dyn Connector,
    m: &'s ConnectorManifest,
    env: &'s Env<'e>,
    found: Vec<Finding>,
}

/// A scripted transport reply and the test its error must pass.
type Case = (&'static str, Result<HttpResponse, HttpError>, fn(&ConnectError) -> bool);

/// A run's result, or the panic it ended in.
type Ran = Result<Result<RunOutput, ConnectError>, String>;

/// How one run is set up.
struct Run<'r> {
    http: &'r dyn HttpTransport,
    progress: &'r dyn Progress,
    cancel: Arc<dyn Cancel>,
    meter: &'r RecordingMeter,
    cap: u64,
}

impl Suite<'_, '_> {
    fn finding(&mut self, check: &'static str, problem: impl Into<String>) {
        self.found.push(Finding { check, problem: problem.into() });
    }

    fn seen(&self) -> usize {
        (self.env.requests_seen)()
    }

    fn run(&self, target: &Target, req: &Request, r: &Run<'_>) -> Ran {
        let env = RunEnv { progress: r.progress, cancel: Arc::clone(&r.cancel), meter: r.meter };
        let clock = FakeClock::new(Seconds(0.0));
        let cx = RunCx::new(target, r.http, self.env.inputs, &env, &InstantPace, &clock, r.cap);
        catch_unwind(AssertUnwindSafe(|| self.c.run(self.m, req, &cx))).map_err(|panic| panic_text(&*panic))
    }

    /// A plain run: not cancelled, an unrefusing meter, the given cap, the env's transport.
    fn plain(&self, target: &Target, req: &Request, http: &dyn HttpTransport, cap: u64) -> (Ran, RecordingMeter) {
        let meter = RecordingMeter::new();
        let ran = self.run(
            target,
            req,
            &Run { http, progress: &Live, cancel: Arc::new(CancelFlag::new()), meter: &meter, cap },
        );
        (ran, meter)
    }

    fn manifest(&mut self) -> Option<Target> {
        if let Err(errors) = self.m.validate() {
            let all: Vec<String> = errors.iter().map(ToString::to_string).collect();
            self.finding(MANIFEST, format!("the manifest does not validate: {}", all.join("; ")));
        }
        for decl in &self.m.capabilities {
            if !self.env.requests.iter().any(|r| r.capability() == decl.capability) {
                self.finding(MANIFEST, format!("env.requests has no {} request", decl.capability));
            }
        }
        for (i, req) in self.env.requests.iter().enumerate() {
            if self.m.declares(req.capability()).is_none() {
                self.finding(MANIFEST, format!("env.requests.{i} asks for undeclared {}", req.capability()));
            }
            let problems = req.check();
            if !problems.is_empty() {
                let all: Vec<String> = problems.iter().map(ToString::to_string).collect();
                self.finding(MANIFEST, format!("env.requests.{i} is invalid: {}", all.join("; ")));
            }
        }
        match self.c.target(self.m, &self.env.config, &Services::none()) {
            Ok(target) => Some(target),
            Err(e) => {
                self.finding(MANIFEST, format!("the target does not resolve: {e}"));
                None
            }
        }
    }

    fn estimate_is_pure(&mut self, target: &Target) {
        for req in &self.env.requests {
            let before = self.seen();
            let (a, b) = (self.c.estimate(self.m, target, req), self.c.estimate(self.m, target, req));
            if self.seen() != before {
                self.finding(ESTIMATE_IS_PURE, format!("estimating {} sent a request", req.capability()));
            }
            if a != b {
                self.finding(ESTIMATE_IS_PURE, format!("two estimates of {} differ: {a:?}, {b:?}", req.capability()));
            }
        }
    }

    fn estimate_bounds_usage(&mut self, target: &Target) {
        for req in &self.env.requests {
            let Ok(estimate) = self.c.estimate(self.m, target, req) else { continue };
            let (ran, meter) = self.plain(target, req, self.env.http, estimate.usd_micros);
            let Ok(Ok(out)) = ran else { continue };
            if out.usage.usd_micros > estimate.usd_micros {
                let (used, bound) = (out.usage.usd_micros, estimate.usd_micros);
                self.finding(
                    ESTIMATE_BOUNDS_USAGE,
                    format!("{} used {used} micro-dollars, above its estimate {bound}", req.capability()),
                );
            }
            if meter.total() != out.usage.usd_micros {
                let (metered, used) = (meter.total(), out.usage.usd_micros);
                self.finding(
                    USAGE_IS_METERED,
                    format!("{} metered {metered} but reported usage {used}", req.capability()),
                );
            }
        }
    }

    fn meter_refusal_stops(&mut self, target: &Target) {
        if !self.m.paid() {
            return;
        }
        for req in &self.env.requests {
            let Ok(estimate) = self.c.estimate(self.m, target, req) else { continue };
            let meter = RecordingMeter::capped(0);
            let run = Run {
                http: self.env.http,
                progress: &Live,
                cancel: Arc::new(CancelFlag::new()),
                meter: &meter,
                cap: estimate.usd_micros,
            };
            let ran = self.run(target, req, &run);
            let charges = meter.charges().len();
            if charges == 0 && estimate.usd_micros > 0 {
                self.finding(
                    METER_REFUSAL_STOPS,
                    format!("{} costs {} but never charged the meter", req.capability(), usd(estimate.usd_micros)),
                );
            }
            if charges > 1 {
                self.finding(
                    METER_REFUSAL_STOPS,
                    format!("{} kept billing after the meter refused ({charges} charges)", req.capability()),
                );
            }
            if charges == 1 && !matches!(stopped_by(&ran), Some(ConnectError::CapReached { .. })) {
                self.finding(
                    METER_REFUSAL_STOPS,
                    format!("{} did not stop with CapReached when the meter refused", req.capability()),
                );
            }
        }
    }

    fn cancelled_makes_no_request(&mut self, target: &Target) {
        for req in &self.env.requests {
            let flag = CancelFlag::new();
            flag.cancel();
            let meter = RecordingMeter::new();
            let before = self.seen();
            let ran = self.run(
                target,
                req,
                &Run {
                    http: self.env.http,
                    progress: &Cancelled,
                    cancel: Arc::new(flag),
                    meter: &meter,
                    cap: u64::MAX,
                },
            );
            if self.seen() != before {
                self.finding(
                    CANCELLED_MAKES_NO_REQUEST,
                    format!("a cancelled {} run sent a request", req.capability()),
                );
            }
            if !matches!(ran, Ok(Err(ConnectError::Cancelled))) {
                self.finding(
                    CANCELLED_MAKES_NO_REQUEST,
                    format!("a cancelled {} run gave {}", req.capability(), describe(&ran)),
                );
            }
        }
    }

    fn cap_is_checked_first(&mut self, target: &Target) {
        if !self.m.paid() {
            return;
        }
        for req in &self.env.requests {
            if self.c.estimate(self.m, target, req).is_ok_and(|e| e.usd_micros == 0) {
                continue;
            }
            let before = self.seen();
            let (ran, _) = self.plain(target, req, self.env.http, 0);
            if self.seen() != before {
                self.finding(
                    CAP_IS_CHECKED_FIRST,
                    format!("a {} run with a cap of 0 sent a request", req.capability()),
                );
            }
            if !matches!(ran, Ok(Err(ConnectError::CapReached { .. }))) {
                self.finding(
                    CAP_IS_CHECKED_FIRST,
                    format!("a {} run with a cap of 0 gave {}", req.capability(), describe(&ran)),
                );
            }
        }
    }

    fn outputs_match_manifest(&mut self, target: &Target) {
        for req in &self.env.requests {
            let Some(decl) = self.m.declares(req.capability()) else { continue };
            let cap = self.c.estimate(self.m, target, req).map_or(u64::MAX, |e| e.usd_micros);
            let (ran, _) = self.plain(target, req, self.env.http, cap);
            let out = match ran {
                Ok(Ok(out)) => out,
                Ok(Err(error)) => {
                    self.finding(OUTPUTS_MATCH_MANIFEST, format!("a {} run failed: {error}", req.capability()));
                    continue;
                }
                Err(panic) => {
                    self.finding(OUTPUTS_MATCH_MANIFEST, format!("a {} run panicked: {panic}", req.capability()));
                    continue;
                }
            };
            let what = req.capability();
            if out.outputs.is_empty() {
                self.finding(OUTPUTS_MATCH_MANIFEST, format!("{what} succeeded with no outputs"));
            }
            if out.outputs.len() > req.count() as usize {
                self.finding(
                    OUTPUTS_MATCH_MANIFEST,
                    format!("{what} gave {} outputs for a count of {}", out.outputs.len(), req.count()),
                );
            }
            let mut first_seed = None;
            for (i, output) in out.outputs.iter().enumerate() {
                let kind = match output {
                    Output::Asset(a) => {
                        if a.model.is_empty() || a.media_type.is_empty() {
                            self.finding(
                                OUTPUTS_MATCH_MANIFEST,
                                format!("{what} output {i} has no model or media type"),
                            );
                        } else if !media_type_fits(a.kind, &a.media_type) {
                            self.finding(
                                OUTPUTS_MATCH_MANIFEST,
                                format!("{what} output {i} is {} but says {}", a.kind, a.media_type),
                            );
                        }
                        first_seed.get_or_insert(a.seed);
                        a.kind
                    }
                    Output::Text(_) => MediaKind::Text,
                    Output::Json(_) => MediaKind::Json,
                };
                if !decl.outputs.contains(&kind) {
                    self.finding(
                        OUTPUTS_MATCH_MANIFEST,
                        format!("{what} output {i} is {kind}, which the manifest does not declare"),
                    );
                }
            }
            if let (Some(seed), Some(got)) = (req.seed(), first_seed)
                && got != Some(seed)
            {
                self.finding(
                    OUTPUTS_MATCH_MANIFEST,
                    format!("{what} asked for seed {seed} but its first output says {got:?}"),
                );
            }
        }
    }

    fn unsupported_is_refused(&mut self, target: &Target) {
        let Some(capability) = Capability::ALL.into_iter().find(|c| self.m.declares(*c).is_none()) else { return };
        let before = self.seen();
        let (ran, _) = self.plain(target, &sample_request(capability), self.env.http, u64::MAX);
        if self.seen() != before {
            self.finding(UNSUPPORTED_IS_REFUSED, format!("an undeclared {capability} request was sent"));
        }
        if !matches!(ran, Ok(Err(ConnectError::Unsupported { .. }))) {
            self.finding(UNSUPPORTED_IS_REFUSED, format!("an undeclared {capability} request gave {}", describe(&ran)));
        }
    }

    fn secrets_stay_out(&mut self, target: &Target) {
        let auth_header = match &self.m.auth {
            AuthSpec::ApiKey { deliver: Deliver::Header { name, .. }, .. } => Some(name.to_ascii_lowercase()),
            _ => None,
        };
        let recorder = Recorder { inner: self.env.http, headers: Mutex::default() };
        let mut leaks = Vec::new();
        for req in &self.env.requests {
            let (ran, _) = self.plain(target, req, &recorder, u64::MAX);
            if let Some(probe) = self.env.secret_probe {
                leaks.extend(
                    leaked(probe, &ran).map(|place| format!("{}: the probe key is in {place}", req.capability())),
                );
            }
        }
        for (name, value) in recorder.headers.into_inner().unwrap_or_else(PoisonError::into_inner) {
            let lower = name.to_ascii_lowercase();
            if lower == "authorization" || auth_header.as_ref() == Some(&lower) {
                leaks.push(format!("the connector set the auth header {name} itself"));
            }
            if self.env.secret_probe.is_some_and(|p| name.contains(p) || value.contains(p)) {
                leaks.push(format!("the probe key is in the header {name}"));
            }
        }
        for leak in leaks {
            self.finding(SECRETS_STAY_OUT, leak);
        }
    }

    fn transport_errors_map(&mut self, target: &Target) {
        if !self.env.transport_checks || !matches!(self.m.transport, Transport::Http | Transport::Mcp) {
            return;
        }
        let Some(req) = self.env.requests.first() else { return };
        let status =
            |status: u16, headers: Vec<(String, String)>| Ok(HttpResponse { status, headers, body: b"{}".to_vec() });
        let cases: [Case; 5] = [
            ("a timeout", Err(HttpError::Timeout), |e| matches!(e, ConnectError::Timeout { .. })),
            ("a too-large answer", Err(HttpError::TooLarge { limit: 1 }), |e| {
                matches!(e, ConnectError::BadResponse { .. })
            }),
            ("status 401", status(401, vec![]), |e| matches!(e, ConnectError::Auth { .. })),
            ("status 429", status(429, vec![("retry-after".into(), "7".into())]), |e| {
                matches!(e, ConnectError::RateLimited { retry_after_s: Some(7), .. })
            }),
            ("status 500", status(500, vec![]), |e| matches!(e, ConnectError::Failed { .. })),
        ];
        for (what, reply, expected) in cases {
            let (ran, _) = self.plain(target, req, &Fixed(reply), u64::MAX);
            let ok = stopped_by(&ran).is_some_and(expected);
            if !ok {
                self.finding(TRANSPORT_ERRORS_MAP, format!("{what} gave {}", describe(&ran)));
            }
        }
    }
}

/// The error a run ended with: its `Err`, or the `stopped` of a partial result.
fn stopped_by(ran: &Ran) -> Option<&ConnectError> {
    match ran {
        Ok(Err(e)) => Some(e),
        Ok(Ok(out)) => out.stopped.as_ref(),
        Err(_) => None,
    }
}

fn describe(ran: &Ran) -> String {
    match ran {
        Ok(Ok(out)) => format!("{} outputs, stopped {:?}", out.outputs.len(), out.stopped),
        Ok(Err(e)) => format!("the error {e:?}"),
        Err(panic) => format!("a panic: {panic}"),
    }
}

/// Where `probe` shows up in a run's result.
fn leaked<'r>(probe: &'r str, ran: &'r Ran) -> impl Iterator<Item = String> + 'r {
    let mut places = Vec::new();
    if let Some(e) = stopped_by(ran)
        && (e.to_string().contains(probe) || format!("{e:?}").contains(probe))
    {
        places.push("an error".to_owned());
    }
    if let Ok(Ok(out)) = ran {
        for (i, output) in out.outputs.iter().enumerate() {
            let hit = match output {
                Output::Asset(a) => {
                    a.bytes.windows(probe.len().max(1)).any(|w| w == probe.as_bytes())
                        || a.model.contains(probe)
                        || a.media_type.contains(probe)
                        || a.name.as_deref().is_some_and(|n| n.contains(probe))
                }
                Output::Text(t) => t.contains(probe),
                Output::Json(v) => v.to_string().contains(probe),
            };
            if hit {
                places.push(format!("output {i}"));
            }
        }
    }
    places.into_iter()
}

fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic".to_owned())
}

/// Progress that is never cancelled.
struct Live;

impl Progress for Live {
    fn stage(&self, _: &str, _: f32) {}
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Progress that is already cancelled.
struct Cancelled;

impl Progress for Cancelled {
    fn stage(&self, _: &str, _: f32) {}
    fn is_cancelled(&self) -> bool {
        true
    }
}

/// Waits take no time: the suite's runs are instant.
struct InstantPace;

impl Pace for InstantPace {
    fn wait(&self, _: f64, _: &dyn Progress) {}
}

/// Passes requests on to the env's transport and records their headers.
struct Recorder<'a> {
    inner: &'a dyn HttpTransport,
    headers: Mutex<Vec<(String, String)>>,
}

impl Recorder<'_> {
    fn record(&self, req: &HttpRequest) {
        self.headers.lock().unwrap_or_else(PoisonError::into_inner).extend(req.headers.iter().cloned());
    }
}

impl HttpTransport for Recorder<'_> {
    fn send(&self, req: &HttpRequest, cancel: &dyn Cancel) -> Result<HttpResponse, HttpError> {
        self.record(req);
        self.inner.send(req, cancel)
    }

    fn open(&self, req: &HttpRequest, cancel: Arc<dyn Cancel>) -> Result<HttpStream, HttpError> {
        self.record(req);
        self.inner.open(req, cancel)
    }
}

/// Answers every request with one reply.
struct Fixed(Result<HttpResponse, HttpError>);

impl HttpTransport for Fixed {
    fn send(&self, _: &HttpRequest, _: &dyn Cancel) -> Result<HttpResponse, HttpError> {
        self.0.clone()
    }

    fn open(&self, _: &HttpRequest, _: Arc<dyn Cancel>) -> Result<HttpStream, HttpError> {
        self.0.clone().map(|r| HttpStream { status: r.status, headers: r.headers, body: Box::new(Cursor::new(r.body)) })
    }
}

/// Whether `media_type` is a well-formed `type/subtype`, each parameter after it a `name=value` (the value a token or
/// a quoted string without `;`), that can be an asset of `kind`, by its top-level type: `image/*`, `audio/*`,
/// `model/*`, `video/*`, `text/*`, `application/json`; motion files are `text/*` (BVH), `model/*` or
/// `application/*` (FBX). `image`, `image/`, `image/png/x`, `image/png;` and `image/png; charset` fit nothing.
pub fn media_type_fits(kind: MediaKind, media_type: &str) -> bool {
    let token = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b));
    let quoted = |v: &str| v.len() >= 2 && v.starts_with('"') && v.ends_with('"') && !v[1..v.len() - 1].contains('"');
    let mut parts = media_type.split(';');
    let essence = parts.next().unwrap_or_default().trim();
    let Some((top, sub)) = essence.split_once('/') else { return false };
    if !token(top) || !token(sub) {
        return false;
    }
    let parameter_ok =
        |p: &str| p.trim().split_once('=').is_some_and(|(name, value)| token(name) && (token(value) || quoted(value)));
    if !parts.all(parameter_ok) {
        return false;
    }
    let top = top.to_ascii_lowercase();
    match kind {
        MediaKind::Image => top == "image",
        MediaKind::Audio => top == "audio",
        MediaKind::Mesh => top == "model",
        MediaKind::Video => top == "video",
        MediaKind::Text => top == "text",
        MediaKind::Json => essence.eq_ignore_ascii_case("application/json"),
        MediaKind::Motion => matches!(top.as_str(), "text" | "model" | "application"),
    }
}
