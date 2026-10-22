//! The rig every test of the host shares: the real `Host` over the mock connectors (fresh `FakeConnector`s, so their
//! counters are readable), a `FakeHttp`, `MemorySecrets`, `MemStore`, `MemoryConfig` and a fake clock, plus a driver
//! that runs an op the way the executor does (plan; for a job, work then finish; each patch applied) and a key scan.
#![allow(dead_code, reason = "each test binary uses part of the rig")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "test helpers: a panic is a clear failure")]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use omuzaki_ai_api::{ConsentLookup, Likeness, NoConsentRecords};
use omuzaki_connect::{ConnectorSet, Host, HostParts, scrub_str};
use omuzaki_connect_api::{
    Artifact, ConnectError, Connector, ConnectorConfig, ConnectorId, ConnectorManifest, Connectors, Output, Request,
    RunCx, RunOutput, Target, registered,
};
use omuzaki_connect_mock::{Counters, FakeConnector, FakePace, MemoryConfig, Script};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_http_api::{HttpBody, HttpRequest, Method};
use omuzaki_http_mock::FakeHttp;
use omuzaki_ops_api::{
    Actor, CancelFlag, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, OpKind, Services, ServicesBuilder,
};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore, SecretValue};
use omuzaki_secrets_mock::MemorySecrets;
use omuzaki_store_api::{Commercial, Consent, ContentHash, Provenance, Source, Store};
use omuzaki_store_mock::MemStore;
use serde_json::Value;

/// The key every test stores, and searches for everywhere afterwards.
pub const KEY: &str = "sk-test-0123456789abcdef";

/// The manifest of the design's worked example (section 4), verbatim: a local `ComfyUI`-like connector whose outputs
/// carry the licence the person declares for their model.
pub const COMFYUI: &str = r#"
id = "comfyui"
name = "ComfyUI"
version = "1.0.0"
transport = "http"
endpoint = "http://127.0.0.1:8188"

[[capabilities]]
capability = "image"
inputs = ["image", "text"]
outputs = ["image"]
max_count = 8
params = [
  { name = "steps", kind = { int = { min = 1, max = 150 } }, default = 20, summary = "Sampling steps." },
  { name = "cfg", kind = { float = { min = 0.0, max = 30.0 } }, default = 7.0, summary = "Prompt strength." },
]

[auth]
kind = "none"

[cost]
model = "free"

[privacy]
leaves_machine = false
destinations = ["127.0.0.1"]
sends = ["prompt", "images"]
retention = "Stays on this computer."
trains_on_inputs = "no"

[licence]
register = "ComfyUI"
outputs = "user_model"
spdx = "LicenseRef-User-Model"
commercial = "unknown"
"#;

/// A hosted connector that sends one HTTP request per call through the host's transport, with a key in a header: the
/// path the key takes, end to end.
pub const ECHO: &str = r#"
id = "test-echo"
name = "Echo service"
version = "2.1.0"
transport = "http"
endpoint = "https://echo.invalid"

[[capabilities]]
capability = "image"
inputs = ["image", "text"]
outputs = ["image"]
max_count = 4

[auth]
kind = "api_key"
secret = "connector/test-echo"
deliver = { via = "header", name = "authorization", scheme = "Bearer" }

[cost]
model = "per_call"
usd_micros = 10000
priced = "2026-10-21"

[privacy]
leaves_machine = true
destinations = ["echo.invalid"]
sends = ["prompt", "images"]
retention = "It keeps requests for 30 days."
trains_on_inputs = "no"

[licence]
register = "Echo service"
outputs = "vendor_terms"
spdx = "LicenseRef-Echo-Terms"
commercial = "ok"
"#;

/// A connector over the fake image generator that reports the model the request names, as a real local generator
/// does (so a declared model licence applies).
pub struct NamedModel(pub FakeConnector);

impl Connector for NamedModel {
    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        let model = match req {
            Request::Image(r) => r.model.clone(),
            _ => None,
        };
        let mut out = self.0.run(m, req, cx)?;
        for o in &mut out.outputs {
            if let (Output::Asset(a), Some(model)) = (o, &model) {
                a.model.clone_from(model);
            }
        }
        Ok(out)
    }

    fn ping(&self, m: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError> {
        self.0.ping(m, cx)
    }
}

/// The echo service's adapter: posts the prompt to `/generate`, bills its price, and returns the answer's body as an
/// image, plus the answer's text as a text answer (so a body that echoes the key would reach the caller).
#[derive(Default)]
pub struct EchoConnector {
    pub runs: AtomicU32,
}

impl Connector for EchoConnector {
    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        self.runs.fetch_add(1, Ordering::Relaxed);
        let price = 10_000;
        if price > cx.cap_usd_micros {
            return Err(ConnectError::cap_reached(cx.cap_usd_micros, price));
        }
        let url = format!("{}/generate", cx.target.endpoint.clone().unwrap_or_default());
        let body = HttpBody::Json(serde_json::json!({ "prompt": req.prompt_text() }));
        let http_req =
            HttpRequest { method: Method::Post, url, headers: vec![], body, timeout_s: 30, max_response: 1 << 20 };
        let response = cx.http.send(&http_req, &Never).map_err(|e| ConnectError::from_http(&e, 30))?;
        if let Some(e) = ConnectError::from_status(&response) {
            return Err(e);
        }
        cx.meter.charge(price)?;
        let text = String::from_utf8_lossy(&response.body).into_owned();
        let header_text: Vec<String> = response.headers.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        let artifact = Artifact {
            bytes: png_bytes(&response.body),
            media_type: "image/png".into(),
            kind: omuzaki_connect_api::MediaKind::Image,
            name: None,
            model: format!("echo@{}", m.version),
            seed: req.seed(),
            rights: None,
        };
        Ok(RunOutput {
            outputs: vec![Output::Asset(artifact), Output::Text(format!("{text}\n{}", header_text.join("\n")))],
            usage: omuzaki_connect_api::Usage { usd_micros: price, ..Default::default() },
            stopped: None,
        })
    }

    fn ping(&self, _: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError> {
        let url = format!("{}/health", cx.target.endpoint.clone().unwrap_or_default());
        let req = HttpRequest {
            method: Method::Get,
            url,
            headers: vec![],
            body: HttpBody::Empty,
            timeout_s: 5,
            max_response: 1024,
        };
        let response = cx.http.send(&req, &Never).map_err(|e| ConnectError::from_http(&e, 5))?;
        ConnectError::from_status(&response).map_or(Ok(()), Err)
    }
}

/// A PNG signature followed by `body`: enough for the sniffing and the store.
pub fn png_bytes(body: &[u8]) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(body);
    v
}

struct Never;

impl omuzaki_http_api::Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Consent records held in memory.
#[derive(Default)]
pub struct Consents(pub Mutex<Vec<Consent>>);

impl ConsentLookup for Consents {
    fn find(&self, person: &str, likeness: Likeness) -> Option<Consent> {
        (likeness == Likeness::VoiceClone)
            .then(|| self.0.lock().unwrap().iter().find(|c| c.person == person).cloned())
            .flatten()
    }
}

/// Every connector of the rig: the eight mocks, the worked example's `comfyui` and the echo service.
pub struct Rig {
    pub host: Arc<Host>,
    pub services: Services,
    pub store: Arc<MemStore>,
    pub secrets: Arc<MemorySecrets>,
    pub config: Arc<MemoryConfig>,
    pub http: Arc<FakeHttp>,
    pub clock: Arc<FakeClock>,
    pub consents: Arc<Consents>,
    pub counters: BTreeMap<String, Arc<Counters>>,
    pub echo_runs: Arc<EchoConnector>,
}

/// The script of each mock, as its registration bills.
fn mock(id: &str, script_for: &dyn Fn(&str) -> Option<Script>) -> FakeConnector {
    let default = match id {
        "mock-image-paid" => Script::priced(40_000),
        "mock-3d" => Script::priced(200_000),
        "mock-voice" => Script::by_manifest(),
        _ => Script::default(),
    };
    let script = script_for(id).unwrap_or(default);
    match id {
        "mock-image" | "mock-image-paid" => FakeConnector::image(script),
        "mock-texture" => FakeConnector::texture(script),
        "mock-3d" => FakeConnector::model3d(script),
        "mock-voice" => FakeConnector::voice(script),
        "mock-music" => FakeConnector::music(script),
        "mock-motion" => FakeConnector::motion(script),
        "mock-dcc" => FakeConnector::dcc(script),
        other => panic!("no mock {other}"),
    }
}

/// Shares an `Arc`'d connector as the box the set holds.
struct Shared<C>(Arc<C>);

impl<C: Connector> Connector for Shared<C> {
    fn target(&self, m: &ConnectorManifest, c: &ConnectorConfig, s: &Services) -> Result<Target, ConnectError> {
        self.0.target(m, c, s)
    }
    fn estimate(
        &self,
        m: &ConnectorManifest,
        t: &Target,
        r: &Request,
    ) -> Result<omuzaki_connect_api::Estimate, ConnectError> {
        self.0.estimate(m, t, r)
    }
    fn run(&self, m: &ConnectorManifest, r: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        self.0.run(m, r, cx)
    }
    fn ping(&self, m: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError> {
        self.0.ping(m, cx)
    }
}

impl Rig {
    /// The rig with every mock billing as registered.
    pub fn new() -> Self {
        Self::with(&|_| None)
    }

    /// The rig with `script_for` overriding a mock's script.
    pub fn with(script_for: &dyn Fn(&str) -> Option<Script>) -> Self {
        omuzaki_connect_mock::link();
        let mut counters = BTreeMap::new();
        let mut entries: Vec<(ConnectorManifest, Box<dyn Connector>)> = Vec::new();
        for r in registered().filter(|r| r.id.starts_with("mock-")) {
            let fake = mock(r.id, script_for);
            counters.insert(r.id.to_owned(), fake.counters());
            entries.push((ConnectorManifest::from_toml(r.manifest).unwrap(), Box::new(fake)));
        }
        let comfy = FakeConnector::image(Script::default());
        counters.insert("comfyui".to_owned(), comfy.counters());
        entries.push((ConnectorManifest::from_toml(COMFYUI).unwrap(), Box::new(NamedModel(comfy))));
        let echo = Arc::new(EchoConnector::default());
        entries.push((ConnectorManifest::from_toml(ECHO).unwrap(), Box::new(Shared(Arc::clone(&echo)))));
        let set = ConnectorSet::from_entries(entries).unwrap();

        let store = Arc::new(MemStore::new());
        let secrets = Arc::new(MemorySecrets::new());
        let config = Arc::new(MemoryConfig::new());
        let clock = Arc::new(FakeClock::new(Seconds(1734.5)));
        let http = Arc::new(
            FakeHttp::new()
                .json(Method::Post, "https://echo.invalid/generate", 200, serde_json::json!({ "ok": true }))
                .json(Method::Get, "https://echo.invalid/health", 200, serde_json::json!({ "ok": true })),
        );
        let consents = Arc::new(Consents::default());
        let pace = Arc::new(FakePace::new(Arc::clone(&clock)));
        let host = Arc::new(Host::new(HostParts {
            set,
            config: Arc::clone(&config) as _,
            secrets: Arc::clone(&secrets) as _,
            store: Arc::clone(&store) as _,
            http: Arc::clone(&http) as _,
            pace,
            clock: Arc::clone(&clock) as _,
            consent: Arc::clone(&consents) as _,
            services: Services::default(),
        }));
        let services = ServicesBuilder::new()
            .insert::<dyn Connectors>(Arc::clone(&host) as _)
            .insert::<Host>(Arc::clone(&host))
            .insert::<dyn Store>(Arc::clone(&store) as _)
            .insert::<dyn SecretStore>(Arc::clone(&secrets) as _)
            .build();
        Self { host, services, store, secrets, config, http, clock, consents, counters, echo_runs: echo }
    }

    /// The rig with the consent lookup that has no records.
    pub fn no_consent() -> Arc<dyn ConsentLookup> {
        Arc::new(NoConsentRecords)
    }

    /// Enables `id` with `plan`.
    pub fn enable(&self, id: &str, plan: Option<&str>) {
        let config = ConnectorConfig { enabled: true, plan: plan.map(str::to_owned), ..ConnectorConfig::default() };
        self.config.set(ConnectorId::new(id).unwrap(), config);
    }

    /// Sets `id`'s settings.
    pub fn configure(&self, id: &str, config: ConnectorConfig) {
        self.config.set(ConnectorId::new(id).unwrap(), config);
    }

    /// Puts [`KEY`] in the keychain for `id`.
    pub fn set_key(&self, id: &str) {
        let label = SecretLabel::new(format!("connector/{id}")).unwrap();
        self.secrets.set(&label, SecretValue::new(KEY)).unwrap();
    }

    /// How many times `id`'s fake ran.
    pub fn runs(&self, id: &str) -> u32 {
        self.counters[id].runs.load(Ordering::Relaxed)
    }

    /// Stores `bytes` as an imported asset with `commercial` and `private_test`.
    pub fn import(&self, bytes: &[u8], commercial: Commercial, private_test: bool) -> ContentHash {
        let p = Provenance {
            source: Source::Imported { file: "fixture.png".into(), origin: None },
            author: None,
            licence: "CC0-1.0".into(),
            attribution: None,
            commercial,
            consent: None,
            private_test,
        };
        self.store.put(bytes, p).unwrap()
    }
}

pub fn person() -> Actor {
    Actor::Person { name: "person".into() }
}

pub fn agent(id: &str) -> Actor {
    Actor::Agent { id: id.into(), model: None }
}

/// A job's signal: progress dropped, a cancel after `cancel_after` checks, and money capped at the reservation like
/// the job manager's.
pub struct Signal {
    pub flag: CancelFlag,
    pub checks: AtomicU32,
    pub cancel_after: Option<u32>,
    pub reserved: u64,
    pub spent: AtomicU64,
    pub charges: Mutex<Vec<u64>>,
}

impl Signal {
    pub fn new(reserved: u64, cancel_after: Option<u32>) -> Self {
        Self {
            flag: CancelFlag::new(),
            checks: AtomicU32::new(0),
            cancel_after,
            reserved,
            spent: AtomicU64::new(0),
            charges: Mutex::default(),
        }
    }
}

impl JobSignal for Signal {
    fn progress(&self, _: &str, _: f32) {}

    fn is_cancelled(&self) -> bool {
        let n = self.checks.fetch_add(1, Ordering::Relaxed);
        self.cancel_after.is_some_and(|after| n >= after)
    }

    fn cancel_flag(&self) -> CancelFlag {
        self.flag.clone()
    }

    fn spend(&self, usd_micros: u64) -> Result<(), OpError> {
        let spent = self.spent.load(Ordering::Relaxed);
        if spent + usd_micros > self.reserved {
            return Err(OpError::denied("past the job's cap"));
        }
        self.spent.store(spent + usd_micros, Ordering::Relaxed);
        self.charges.lock().unwrap().push(usd_micros);
        Ok(())
    }
}

struct Snapshot(MemDoc);

impl JobDoc for Snapshot {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(&self.0);
    }
}

/// What a driven op left behind.
pub struct Driven {
    pub output: Value,
    pub plan: omuzaki_ops_api::Plan,
    pub finish: Option<omuzaki_ops_api::Plan>,
}

/// Runs `op` as the executor would: plan; for a job, work as job `run` with `signal` then finish against the
/// document as it is then; every patch applied to `doc`. A dry run stops after the plan.
pub fn drive<O: Op>(
    rig: &Rig,
    doc: &mut MemDoc,
    op: &O,
    actor: &Actor,
    run: u64,
    signal: &Signal,
    confirmed_by: Option<&Actor>,
) -> Result<Driven, OpError> {
    let plan = {
        let ctx = OpCtx::new(doc, actor, &*rig.clock).with_services(&rig.services);
        let ctx = match confirmed_by {
            Some(p) => ctx.with_confirmation(p),
            None => ctx,
        };
        op.plan(&ctx)?
    };
    doc.apply(&plan.patch).unwrap();
    if O::meta().kind != OpKind::Job {
        return Ok(Driven { output: plan.output.clone().unwrap_or(Value::Null), plan, finish: None });
    }
    let planned = plan.output.clone().unwrap_or(Value::Null);
    let snapshot = Snapshot(doc.clone());
    let job = JobCtx::new(JobId(run), actor, &snapshot, signal)
        .with_services(&rig.services)
        .with_plan_output(&planned)
        .with_reserved_usd_micros(signal.reserved);
    let result = op.work(&job)?;
    let finish = {
        let ctx = OpCtx::new(doc, actor, &*rig.clock).with_services(&rig.services);
        op.finish(&ctx, result)?
    };
    doc.apply(&finish.patch).unwrap();
    Ok(Driven { output: finish.output.clone().unwrap_or(Value::Null), plan, finish: Some(finish) })
}

/// Only the plan: a dry run.
pub fn dry<O: Op>(rig: &Rig, doc: &MemDoc, op: &O, actor: &Actor) -> Result<omuzaki_ops_api::Plan, OpError> {
    let ctx = OpCtx::new(doc, actor, &*rig.clock).with_services(&rig.services).with_dry_run(true);
    op.plan(&ctx)
}

/// Every text a test saw, for the key scan at its end.
#[derive(Default)]
pub struct Seen(pub Mutex<Vec<String>>);

impl Seen {
    pub fn add(&self, text: impl Into<String>) {
        self.0.lock().unwrap().push(text.into());
    }

    pub fn debug(&self, value: &impl std::fmt::Debug) {
        self.add(format!("{value:?}"));
    }

    pub fn json(&self, value: &impl serde::Serialize) {
        self.add(serde_json::to_string(value).unwrap());
    }

    /// Fails if the key, raw, percent-encoded or base64, is in anything seen.
    pub fn assert_no_key(&self) {
        let key = SecretValue::new(KEY);
        for text in self.0.lock().unwrap().iter() {
            assert_eq!(scrub_str(&key, text), *text, "the key leaked into: {text}");
        }
    }
}
