//! The conformance suite (#998): every registered fake passes with no findings; deliberately broken connectors fail
//! exactly their own check; an HTTP adapter written against `FakeHttp` passes the transport checks; and the suite
//! reports every finding, not the first.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_connect_api::{
    Artifact, Capability, ConnectError, Connector, ConnectorManifest, Estimate, MediaKind, Output, Request, RunCx,
    RunEnv, RunOutput, Target, Usage, registered,
};
use omuzaki_connect_mock::conformance::{self, CHECKS, Env, Finding, sample_request};
use omuzaki_connect_mock::{FakeConnector, MemoryInputs, Script};
use omuzaki_http_api::{HttpBody, HttpRequest, Method, NeverCancel};
use omuzaki_http_mock::FakeHttp;

fn manifest(id: &str) -> ConnectorManifest {
    omuzaki_connect_mock::link();
    registered().find(|r| r.id == id).unwrap().parse().unwrap()
}

/// The suite's findings for `c` under `m`, with `requests` and an empty `FakeHttp`.
fn findings(c: &dyn Connector, m: &ConnectorManifest, requests: Vec<Request>, probe: Option<&str>) -> Vec<Finding> {
    let http = FakeHttp::new();
    let seen = || http.calls().len() + http.unmatched().len();
    let mut inputs = MemoryInputs::new();
    inputs.add(conformance::SAMPLE_IMAGE.to_vec(), "image/png");
    let mut env = Env::new(&http, &seen, &inputs, requests);
    env.secret_probe = probe;
    conformance::check(c, m, &env)
}

fn checks(found: &[Finding]) -> Vec<&'static str> {
    found.iter().map(|f| f.check).collect()
}

#[test]
fn every_registered_fake_conforms() {
    omuzaki_connect_mock::link();
    let mut ran = 0;
    for r in registered() {
        let m = r.parse().unwrap();
        let requests = m.capabilities.iter().map(|d| sample_request(d.capability)).collect();
        let found = findings(&*(r.make)(), &m, requests, Some("sk-probe-key"));
        assert_eq!(found, vec![], "{}", r.id);
        ran += 1;
    }
    assert_eq!(ran, 8);
}

fn image() -> Request {
    sample_request(Capability::Image)
}

/// Its estimate sends a request through a transport it holds.
struct ChattyEstimate(FakeConnector, Arc<FakeHttp>);

impl Connector for ChattyEstimate {
    fn estimate(&self, m: &ConnectorManifest, t: &Target, req: &Request) -> Result<Estimate, ConnectError> {
        let probe = HttpRequest {
            method: Method::Get,
            url: "https://price.invalid/".into(),
            headers: vec![],
            body: HttpBody::Empty,
            timeout_s: 1,
            max_response: 1,
        };
        let _answer = omuzaki_http_api::HttpTransport::send(&*self.1, &probe, &NeverCancel);
        self.0.estimate(m, t, req)
    }

    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        self.0.run(m, req, cx)
    }

    fn ping(&self, m: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError> {
        self.0.ping(m, cx)
    }
}

/// It runs as if its progress were never cancelled.
struct IgnoresCancel(FakeConnector);

impl Connector for IgnoresCancel {
    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        let env = RunEnv { progress: &omuzaki_ai_api::NoProgress, cancel: Arc::new(NeverCancel), meter: cx.meter };
        let deaf = RunCx::new(cx.target, cx.http, cx.inputs, &env, cx.pace, cx.clock, cx.cap_usd_micros);
        self.0.run(m, req, &deaf)
    }

    fn ping(&self, m: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError> {
        self.0.ping(m, cx)
    }
}

/// It puts the key it saw into the reason it stopped.
struct LeaksKey(FakeConnector, &'static str);

impl Connector for LeaksKey {
    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        self.0.run(m, req, cx).map(|out| RunOutput {
            stopped: Some(ConnectError::failed(format!("the service said: bad key {}", self.1))),
            ..out
        })
    }

    fn ping(&self, m: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError> {
        self.0.ping(m, cx)
    }
}

#[test]
fn each_broken_connector_fails_only_its_own_check() {
    let m = manifest("mock-image");
    let chatty = ChattyEstimate(FakeConnector::image(Script::default()), Arc::new(FakeHttp::new()));
    // The chatty connector's own transport is the one the suite watches.
    let http = Arc::clone(&chatty.1);
    let seen = || http.calls().len() + http.unmatched().len();
    let inputs = MemoryInputs::new();
    let env = Env::new(&*http, &seen, &inputs, vec![image()]);
    assert_eq!(checks(&conformance::check(&chatty, &m, &env)), [conformance::ESTIMATE_IS_PURE]);

    let deaf = IgnoresCancel(FakeConnector::image(Script::default()));
    assert_eq!(checks(&findings(&deaf, &m, vec![image()], None)), [conformance::CANCELLED_MAKES_NO_REQUEST]);

    let leaky = LeaksKey(FakeConnector::image(Script::default()), "sk-probe-key");
    assert_eq!(checks(&findings(&leaky, &m, vec![image()], Some("sk-probe-key"))), [conformance::SECRETS_STAY_OUT]);
}

/// Bills more than its manifest's price: over its estimate, and more than it meters.
struct Overbills(FakeConnector);

impl Connector for Overbills {
    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        self.0
            .run(m, req, cx)
            .map(|out| RunOutput { usage: Usage { usd_micros: out.usage.usd_micros + 1, ..out.usage }, ..out })
    }

    fn ping(&self, m: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError> {
        self.0.ping(m, cx)
    }
}

#[test]
fn every_finding_is_reported() {
    let m = manifest("mock-image-paid");
    let found = findings(&Overbills(FakeConnector::image(Script::priced(40_000))), &m, vec![image()], None);
    assert_eq!(checks(&found), [conformance::ESTIMATE_BOUNDS_USAGE, conformance::USAGE_IS_METERED]);
    // A free price on a paid manifest skips the cap check of a fake, and a missing request is the manifest's.
    let free = FakeConnector::image(Script::default());
    let found = findings(&free, &m, vec![], None);
    assert_eq!(checks(&found), [conformance::MANIFEST]);
    assert!(CHECKS.iter().all(|c| !c.is_empty()));
}

#[test]
fn a_cap_the_connector_ignores_is_found() {
    let m = manifest("mock-image-paid");
    // Free per output though the manifest is paid: a cap of 0 does not stop it, and it never bills.
    let found = findings(&FakeConnector::image(Script::default()), &m, vec![image()], None);
    assert_eq!(checks(&found), [conformance::METER_REFUSAL_STOPS, conformance::CAP_IS_CHECKED_FIRST]);
}

/// Checks the cap first but bills nobody: it runs against a meter that keeps nothing and reports no usage.
struct NeverMeters(FakeConnector);

/// Takes every charge and keeps none of them.
struct Unwatched;

impl omuzaki_connect_api::Meter for Unwatched {
    fn charge(&self, _: u64) -> Result<(), ConnectError> {
        Ok(())
    }
}

impl Connector for NeverMeters {
    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        let env = RunEnv { progress: cx.progress, cancel: Arc::clone(&cx.cancel), meter: &Unwatched };
        let unbilled = RunCx::new(cx.target, cx.http, cx.inputs, &env, cx.pace, cx.clock, cx.cap_usd_micros);
        self.0.run(m, req, &unbilled).map(|out| RunOutput { usage: Usage::default(), ..out })
    }

    fn ping(&self, m: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError> {
        self.0.ping(m, cx)
    }
}

/// Never makes anything: a run that would have made something fails instead.
struct AlwaysFails(FakeConnector);

impl Connector for AlwaysFails {
    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        self.0.run(m, req, cx).and_then(|_| Err(ConnectError::failed("down")))
    }

    fn ping(&self, m: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError> {
        self.0.ping(m, cx)
    }
}

#[test]
fn a_paid_run_that_never_meters_is_found() {
    let m = manifest("mock-image-paid");
    let found = findings(&NeverMeters(FakeConnector::image(Script::priced(40_000))), &m, vec![image()], None);
    assert_eq!(checks(&found), [conformance::METER_REFUSAL_STOPS]);
}

/// Rewrites what its fake returns: `Empty` drops every output, `Mislabelled` calls its images `audio/wav`.
enum Tampers {
    Empty(FakeConnector),
    Mislabelled(FakeConnector),
}

impl Connector for Tampers {
    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        match self {
            Tampers::Empty(fake) => fake.run(m, req, cx).map(|out| RunOutput { outputs: vec![], ..out }),
            Tampers::Mislabelled(fake) => fake.run(m, req, cx).map(|mut out| {
                for output in &mut out.outputs {
                    if let Output::Asset(a) = output {
                        a.media_type = "audio/wav".into();
                    }
                }
                out
            }),
        }
    }

    fn ping(&self, m: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError> {
        match self {
            Tampers::Empty(fake) | Tampers::Mislabelled(fake) => fake.ping(m, cx),
        }
    }
}

#[test]
fn empty_or_mislabelled_outputs_are_found() {
    let m = manifest("mock-image");
    for tampered in [
        Tampers::Empty(FakeConnector::image(Script::default())),
        Tampers::Mislabelled(FakeConnector::image(Script::default())),
    ] {
        assert_eq!(checks(&findings(&tampered, &m, vec![image()], None)), [conformance::OUTPUTS_MATCH_MANIFEST]);
    }
    assert!(
        conformance::media_type_fits(MediaKind::Image, "image/png")
            && conformance::media_type_fits(MediaKind::Motion, "text/plain")
    );
    assert!(
        !conformance::media_type_fits(MediaKind::Image, "audio/wav")
            && !conformance::media_type_fits(MediaKind::Json, "text/plain")
    );
    for malformed in [
        "image",
        "image/",
        "/png",
        "image/png/extra",
        "image/ png",
        "image/png;",
        "image/png; charset",
        "image/png; =x",
    ] {
        assert!(!conformance::media_type_fits(MediaKind::Image, malformed), "{malformed}");
    }
    assert!(conformance::media_type_fits(MediaKind::Json, "application/json; charset=utf-8"));
    assert!(conformance::media_type_fits(MediaKind::Text, r#"text/plain; charset="utf-8"; format=flowed"#));
}

#[test]
fn a_connector_that_never_makes_anything_is_found() {
    let m = manifest("mock-image");
    let found = findings(&AlwaysFails(FakeConnector::image(Script::default())), &m, vec![image()], None);
    assert_eq!(checks(&found), [conformance::OUTPUTS_MATCH_MANIFEST]);
}

/// A small HTTP adapter written the way real ones are: cap and cancel first, the shared error mapping, the charge as
/// the request completes.
#[derive(Default)]
struct EchoHttp;

const URL: &str = "https://echo.invalid/generate";

impl Connector for EchoHttp {
    fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
        if m.declares(req.capability()).is_none() {
            return Err(ConnectError::unsupported(format!("{} makes images only", m.id)));
        }
        if cx.progress.is_cancelled() {
            return Err(ConnectError::Cancelled);
        }
        let price = 10_000;
        if price > cx.cap_usd_micros {
            return Err(ConnectError::cap_reached(cx.cap_usd_micros, price));
        }
        let timeout_s = 30;
        let request = HttpRequest {
            method: Method::Post,
            url: URL.into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: HttpBody::Json(serde_json::to_value(req).map_err(|e| ConnectError::failed(e.to_string()))?),
            timeout_s,
            max_response: 1 << 20,
        };
        let response = cx.http.send(&request, &*cx.cancel).map_err(|e| ConnectError::from_http(&e, timeout_s))?;
        if let Some(e) = ConnectError::from_status(&response) {
            return Err(e);
        }
        cx.meter.charge(price)?;
        let image = Artifact {
            bytes: response.body,
            media_type: "image/png".into(),
            kind: MediaKind::Image,
            name: None,
            model: "echo@1".into(),
            seed: req.seed(),
            rights: None,
        };
        Ok(RunOutput {
            outputs: vec![Output::Asset(image)],
            usage: Usage { usd_micros: price, ..Usage::default() },
            stopped: None,
        })
    }

    fn ping(&self, _: &ConnectorManifest, _: &RunCx<'_>) -> Result<(), ConnectError> {
        Ok(())
    }
}

const ECHO: &str = r#"
id = "echo-http"
name = "Echo"
version = "1.0.0"
transport = "http"
endpoint = "https://echo.invalid"
[[capabilities]]
capability = "image"
outputs = ["image"]
max_count = 1
[auth]
kind = "api_key"
secret = "connector/echo-http"
deliver = { via = "header", name = "x-api-key", scheme = "" }
[cost]
model = "per_call"
usd_micros = 10000
priced = "2026-10-13"
[privacy]
leaves_machine = true
destinations = ["echo.invalid"]
retention = "It keeps nothing."
trains_on_inputs = "no"
[licence]
register = "Echo"
outputs = "vendor_terms"
spdx = "LicenseRef-Echo"
commercial = "ok"
"#;

#[test]
fn an_http_adapter_passes_the_transport_checks() {
    let m = ConnectorManifest::from_toml(ECHO).unwrap();
    let http = FakeHttp::new().json(Method::Post, URL, 200, serde_json::json!({"ok": true}));
    let seen = || http.calls().len() + http.unmatched().len();
    let inputs = MemoryInputs::new();
    let mut suite = Env::new(&http, &seen, &inputs, vec![image()]);
    suite.secret_probe = Some("sk-probe-key");
    assert_eq!(conformance::check(&EchoHttp, &m, &suite), vec![]);
    assert!(!http.calls().is_empty(), "the suite ran it against the fake for real");
}
