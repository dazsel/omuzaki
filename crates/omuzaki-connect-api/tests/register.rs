//! Registration and the connector seams (#994, design D3, D4, D13): `register_connector!` is found once and recorded
//! for the link gate, `make` builds fresh connectors, the default `target` and `estimate` read the manifest, and the
//! run context hands out services and its parts.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_ai_api::{NoProgress, Progress};
use omuzaki_connect_api::{
    ConnectError, Connector, ConnectorConfig, ConnectorManifest, InputAsset, Inputs, Meter, Pace, Request, RunCx,
    RunEnv, RunOutput, Target, Usage, estimate_from_manifest, registered,
};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_http_api::{Cancel, HttpError, HttpRequest, HttpResponse, HttpStream, HttpTransport, NeverCancel};
use omuzaki_ops_api::{Services, ServicesBuilder};
use omuzaki_store_api::ContentHash;

const MANIFEST: &str = r#"
id = "test-registered"
name = "Registered"
version = "1.0.0"
transport = "fake"
[[capabilities]]
capability = "image"
outputs = ["image"]
max_count = 4
[auth]
kind = "none"
[cost]
model = "per_call"
usd_micros = 70000
priced = "2026-10-13"
[privacy]
leaves_machine = false
destinations = []
trains_on_inputs = "no"
[licence]
register = "Omuzaki mock connector"
outputs = "vendor_terms"
spdx = "LicenseRef-Test"
commercial = "ok"
"#;

static MADE: AtomicUsize = AtomicUsize::new(0);

/// A connector that counts how many were built and does nothing else.
struct Counted;

impl Default for Counted {
    fn default() -> Self {
        MADE.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Connector for Counted {
    fn run(&self, _: &ConnectorManifest, _: &Request, _: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        Ok(RunOutput { outputs: vec![], usage: Usage::default(), stopped: None })
    }

    fn ping(&self, _: &ConnectorManifest, _: &RunCx<'_>) -> Result<(), ConnectError> {
        Ok(())
    }
}

omuzaki_connect_api::register_connector!("test-registered", MANIFEST, Counted);

#[test]
fn a_registration_is_found_once_and_recorded_for_the_link_gate() {
    let found: Vec<_> = registered().filter(|r| r.id == "test-registered").collect();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].manifest, MANIFEST);
    assert_eq!(found[0].parse().unwrap().validate(), Ok(()));
    let records: Vec<_> = omuzaki_core::link::registered().into_iter().filter(|r| r.kind == "connector").collect();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].name, "\"test-registered\"");
    let id = omuzaki_connect_api::ConnectorId::new("test-registered").unwrap();
    assert!(omuzaki_connect_api::find(&id).is_some());
}

#[test]
fn make_builds_a_fresh_connector_each_call() {
    let r = registered().find(|r| r.id == "test-registered").unwrap();
    let before = MADE.load(Ordering::SeqCst);
    let (_a, _b) = ((r.make)(), (r.make)());
    assert_eq!(MADE.load(Ordering::SeqCst), before + 2);
}

#[test]
fn a_registration_naming_another_manifest_is_refused() {
    let r = omuzaki_connect_api::ConnectorRegistration::of::<Counted>("someone-else", MANIFEST);
    assert!(r.parse().unwrap_err().to_string().starts_with("id:"));
}

#[test]
fn the_defaults_read_the_manifest() {
    let m = ConnectorManifest::from_toml(MANIFEST).unwrap();
    let config = ConnectorConfig::default();
    let target = Counted.target(&m, &config, &Services::none()).unwrap();
    assert_eq!(target, Target::from_manifest(&m, &config).unwrap());
    let req: Request = serde_json::from_value(serde_json::json!(
        {"capability": "image", "prompt": "a", "width": 8, "height": 8, "count": 2}
    ))
    .unwrap();
    assert_eq!(Counted.estimate(&m, &target, &req), estimate_from_manifest(&m, &req));
}

struct NoHttp;

impl HttpTransport for NoHttp {
    fn send(&self, _: &HttpRequest, _: &dyn Cancel) -> Result<HttpResponse, HttpError> {
        Err(HttpError::Refused("no network in this test".into()))
    }

    fn open(&self, _: &HttpRequest, _: Arc<dyn Cancel>) -> Result<HttpStream, HttpError> {
        Err(HttpError::Refused("no network in this test".into()))
    }
}

struct NoInputs;

impl Inputs for NoInputs {
    fn get(&self, hash: &ContentHash) -> Result<InputAsset, ConnectError> {
        Err(ConnectError::refused(format!("{hash} is not an input of this request")))
    }
}

struct Instant;

impl Pace for Instant {
    fn wait(&self, _: f64, _: &dyn Progress) {}
}

struct Free;

impl Meter for Free {
    fn charge(&self, _: u64) -> Result<(), ConnectError> {
        Ok(())
    }
}

trait Greeter: Send + Sync {
    fn hello(&self) -> &'static str;
}

struct Hi;

impl Greeter for Hi {
    fn hello(&self) -> &'static str {
        "hi"
    }
}

#[test]
fn the_run_context_hands_out_its_parts_and_services() {
    let m = ConnectorManifest::from_toml(MANIFEST).unwrap();
    let target = Target::from_manifest(&m, &ConnectorConfig::default()).unwrap();
    let env = RunEnv { progress: &NoProgress, cancel: Arc::new(NeverCancel), meter: &Free };
    let clock = FakeClock::new(Seconds(0.0));
    let cx = RunCx::new(&target, &NoHttp, &NoInputs, &env, &Instant, &clock, 5);
    assert!(cx.service::<dyn Greeter>().is_none());
    assert_eq!((cx.cap_usd_micros, cx.config().enabled), (5, false));
    assert!(cx.inputs.get(&ContentHash::of(b"x")).is_err() && cx.meter.charge(1).is_ok());
    let services = ServicesBuilder::new().insert::<dyn Greeter>(Arc::new(Hi)).build();
    let cx = cx.with_services(&services);
    assert_eq!(cx.service::<dyn Greeter>().map(Greeter::hello), Some("hi"));
    assert!(Counted.ping(&m, &cx).is_ok());
}
