//! The host over the mock connectors: the set, status, the preview, the run (caps, seeds, cancel, store with
//! provenance, candidates), the widening check, rights and the publish block, and the key's path end to end.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::{ECHO, KEY, Rig, Seen, agent, person, png_bytes};
use omuzaki_ai_api::{NoProgress, Progress, Requester};
use omuzaki_connect::{ConnectorSet, SetError};
use omuzaki_connect_api::{
    Call, CandidateState, Capability, ConnectError, ConnectorConfig, ConnectorId, ConnectorManifest,
    ConnectorRegistration, Connectors, DeclaredModel, ImageRequest, InputRights, Output, PreviewError, Request, RunEnv,
    RunError, RunId, RunResult, Sends, Status, VoiceChoice, VoiceRequest, Widening,
};
use omuzaki_connect_mock::{FakeConnector, RecordingMeter, Script};
use omuzaki_core::rng::Rng;
use omuzaki_http_api::Cancel;
use omuzaki_store_api::{Commercial, Consent, GateError, Source, Store, licence_gate};
use serde_json::json;

/// The gate's answer by kind: `pass`, `non_commercial`, `unknown`, `private_test`, `no_consent`.
fn gate(p: &omuzaki_store_api::Provenance, real_person: bool) -> &'static str {
    match licence_gate(p, real_person) {
        Ok(()) => "pass",
        Err(GateError::NonCommercial { .. }) => "non_commercial",
        Err(GateError::Unknown { .. }) => "unknown",
        Err(GateError::PrivateTest) => "private_test",
        Err(GateError::NoConsent) => "no_consent",
    }
}

/// What a reader of the hash uses (D6): the store's first record joined with the candidate's own. The store keeps
/// the first provenance of identical bytes, so a later, stricter run must not inherit an earlier, looser record.
fn effective(rig: &Rig, c: &omuzaki_connect_api::Candidate) -> omuzaki_store_api::Provenance {
    omuzaki_connect_api::join_provenance(rig.store.provenance(&c.asset).unwrap(), [&c.provenance])
}

fn id(s: &str) -> ConnectorId {
    ConnectorId::new(s).unwrap()
}

fn images(count: u8) -> Request {
    Request::Image(ImageRequest {
        prompt: "chibi fox mascot, front view".into(),
        width: 64,
        height: 64,
        count,
        ..ImageRequest::default()
    })
}

fn with_refs(count: u8, refs: Vec<omuzaki_store_api::ContentHash>) -> Request {
    match images(count) {
        Request::Image(r) => Request::Image(ImageRequest { references: refs, ..r }),
        other => other,
    }
}

/// The rights `tray::input_rights` would give, straight from the store (no tray records in these tests).
fn rights(rig: &Rig, req: &Request) -> InputRights {
    req.input_assets().into_iter().map(|h| (h, rig.store.provenance(&h).unwrap())).collect()
}

struct NoCancel;
impl Cancel for NoCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Cancelled once it has been asked `after` times.
struct CancelAfter(std::sync::atomic::AtomicU32, u32);
impl Progress for CancelAfter {
    fn stage(&self, _: &str, _: f32) {}
    fn is_cancelled(&self) -> bool {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= self.1
    }
}

fn call(rig: &Rig, connector: &str, request: Request, max: u64, run: u64) -> Call {
    Call {
        connector: Some(id(connector)),
        inputs: rights(rig, &request),
        request,
        max_usd_micros: max,
        expect_usd_micros: None,
        run: RunId(run),
        requested_by: person(),
        requester: Requester::Person,
        regenerated_from: None,
        planned: None,
    }
}

fn run_with(rig: &Rig, call: &Call, progress: &dyn Progress, meter: &RecordingMeter) -> Result<RunResult, RunError> {
    let env = RunEnv { progress, cancel: Arc::new(NoCancel), meter };
    rig.host.run(call, &env)
}

fn run(rig: &Rig, call: &Call) -> Result<RunResult, RunError> {
    run_with(rig, call, &NoProgress, &RecordingMeter::new())
}

#[test]
fn the_set_reports_every_bad_registration_at_once() {
    const GOOD: &str = include_str!("../../omuzaki-connect-mock/connectors/mock-image.connector.toml");
    let bad_rule = GOOD.replace("max_count = 8", "max_count = 0");
    let bad_rule: &'static str = Box::leak(bad_rule.into_boxed_str());
    let regs = [
        ConnectorRegistration::of::<omuzaki_connect_mock::MockImage>("mock-other", GOOD),
        ConnectorRegistration::of::<omuzaki_connect_mock::MockImage>("mock-image", GOOD),
        ConnectorRegistration::of::<omuzaki_connect_mock::MockImage>("mock-image", GOOD),
        ConnectorRegistration::of::<omuzaki_connect_mock::MockImage>("mock-image", bad_rule),
    ];
    let errors = ConnectorSet::from_registrations(regs.iter()).unwrap_err();
    assert!(errors.iter().any(|e| matches!(e, SetError::IdMismatch { .. })), "{errors:?}");
    assert!(errors.contains(&SetError::Duplicate("mock-image".into())), "{errors:?}");
    let manifest = errors.iter().find(|e| matches!(e, SetError::Manifest { .. })).unwrap();
    assert!(manifest.to_string().contains("max_count"), "{manifest}");
    // The binary's own registrations are all valid.
    omuzaki_connect_mock::link();
    let set = ConnectorSet::from_registered().unwrap();
    assert!(set.len() >= 8 && set.get(&id("mock-image-paid")).is_some());
}

#[test]
fn status_goes_disabled_key_plan_ready() {
    let rig = Rig::new();
    let paid = id("mock-image-paid");
    assert_eq!(rig.host.status(&paid), Some(Status::Disabled));
    rig.enable("mock-image-paid", Some("free"));
    assert_eq!(rig.host.status(&paid), Some(Status::NeedsKey));
    rig.set_key("mock-image-paid");
    assert_eq!(rig.host.status(&paid), Some(Status::needs_plan(vec!["paid".into()])));
    rig.enable("mock-image-paid", Some("paid"));
    assert_eq!(rig.host.status(&paid), Some(Status::Ready));
    assert_eq!(rig.host.status(&id("nope")), None);
    let ids: Vec<String> = rig.host.list().into_iter().map(|m| m.id.to_string()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
}

#[test]
fn the_preview_of_a_paid_hosted_call_says_what_leaves_and_costs_and_calls_nothing() {
    let rig = Rig::new();
    rig.enable("mock-image-paid", Some("paid"));
    rig.set_key("mock-image-paid");
    let refs: Vec<_> = (0..3u8).map(|i| rig.import(&png_bytes(&[i; 100]), Commercial::Ok, false)).collect();
    let req = with_refs(2, refs);
    let p = rig.host.preview(&req, None, Requester::Person, &rights(&rig, &req)).unwrap();
    assert!(p.leaves_machine);
    assert_eq!(p.destinations, vec!["mock.invalid".to_owned()]);
    let rows: Vec<(Sends, u32, u64)> = p.outbound.iter().map(|o| (o.what, o.count, o.bytes)).collect();
    assert_eq!(rows, vec![(Sends::Prompt, 1, 28), (Sends::Images, 3, 3 * 108)]);
    assert_eq!((p.estimate.usd_micros, p.needs_spend), (80_000, true));
    let english = p.english();
    for part in ["3 images", "mock.invalid", "$0.08"] {
        assert!(english.contains(part), "{english}");
    }
    assert_eq!(p.needs_person, None, "a person is never asked");
    assert_eq!((rig.runs("mock-image-paid"), rig.http.calls().len()), (0, 0));
    assert_eq!(rig.store.len(), 3, "the preview stores nothing");

    // An agent sending assets off the computer needs a person; with a prompt only, it does not.
    let agent = rig.host.preview(&req, None, Requester::Agent, &rights(&rig, &req)).unwrap();
    assert_eq!(agent.needs_person.as_deref(), Some(english.as_str()));
    let prompt_only = rig.host.preview(&images(2), None, Requester::Agent, &InputRights::new()).unwrap();
    assert_eq!(prompt_only.needs_person, None);
}

#[test]
fn an_endpoint_on_another_computer_makes_a_local_connector_leave_it() {
    let rig = Rig::new();
    let lan =
        ConnectorConfig { enabled: true, endpoint: Some("http://192.168.1.20:8188".into()), ..Default::default() };
    rig.configure("mock-image", lan);
    let p = rig.host.preview(&images(1), Some(&id("mock-image")), Requester::Person, &InputRights::new()).unwrap();
    assert!(p.leaves_machine && p.destinations.contains(&"192.168.1.20".to_owned()));
    assert!(p.warnings.iter().any(|w| w.contains("not encrypted")), "{:?}", p.warnings);
    let local = ConnectorConfig { enabled: true, endpoint: Some("http://127.0.0.1:8188".into()), ..Default::default() };
    rig.configure("mock-image", local);
    let p = rig.host.preview(&images(1), Some(&id("mock-image")), Requester::Person, &InputRights::new()).unwrap();
    assert!(!p.leaves_machine);
}

#[test]
fn private_material_never_leaves_and_unknown_assets_are_named() {
    let rig = Rig::new();
    rig.enable("mock-image-paid", Some("paid"));
    rig.set_key("mock-image-paid");
    rig.enable("mock-image", None);
    let private = rig.import(&png_bytes(b"private"), Commercial::Ok, true);
    let req = with_refs(1, vec![private]);
    let hosted = rig.host.preview(&req, Some(&id("mock-image-paid")), Requester::Person, &rights(&rig, &req));
    assert_eq!(hosted, Err(PreviewError::PrivateInput(private)));
    let local = rig.host.preview(&req, Some(&id("mock-image")), Requester::Person, &rights(&rig, &req)).unwrap();
    assert!(!local.leaves_machine);
    let unknown = omuzaki_store_api::ContentHash::of(b"never stored");
    let req = with_refs(1, vec![unknown]);
    let e = rig.host.preview(&req, Some(&id("mock-image")), Requester::Person, &InputRights::new());
    assert_eq!(e, Err(PreviewError::UnknownAsset(unknown)));
}

#[test]
fn a_voice_clone_needs_consent_and_carries_it() {
    let rig = Rig::new();
    rig.enable("mock-voice", Some("paid"));
    rig.set_key("mock-voice");
    let reference = rig.import(b"RIFF\0\0\0\0WAVEfmt ", Commercial::Ok, false);
    let req = Request::Voice(VoiceRequest {
        text: "hello there".into(),
        voice: VoiceChoice::clone_of(reference, "Kit"),
        language: None,
        seed: None,
        params: BTreeMap::new(),
    });
    let e = rig.host.preview(&req, None, Requester::Person, &rights(&rig, &req));
    assert!(matches!(e, Err(PreviewError::NeedsConsent { .. })), "{e:?}");
    let consent = Consent { person: "Kit".into(), scope: "voice cloning for this channel".into(), document: None };
    rig.consents.0.lock().unwrap().push(consent.clone());
    let p = rig.host.preview(&req, None, Requester::Person, &rights(&rig, &req)).unwrap();
    assert_eq!(p.commercial, Commercial::Ok);
    assert_eq!(p.outbound.iter().find(|o| o.what == Sends::Audio).map(|o| o.count), Some(1));
    let r = run(&rig, &call(&rig, "mock-voice", req, p.estimate.usd_micros, 5)).unwrap();
    let stored = rig.store.provenance(&r.candidates[0].asset).unwrap();
    assert_eq!(stored.consent, Some(consent));
    assert_eq!(gate(&stored, true), "pass");
}

#[test]
fn choosing_needs_one_enabled_connector_and_every_problem_is_listed() {
    let rig = Rig::new();
    let none = rig.host.preview(&images(1), None, Requester::Person, &InputRights::new());
    assert_eq!(none, Err(PreviewError::NoConnector(Capability::Image)));
    rig.enable("mock-image", None);
    rig.enable("mock-image-paid", Some("paid"));
    let e = rig.host.preview(&images(1), None, Requester::Person, &InputRights::new()).unwrap_err();
    assert_eq!(e, PreviewError::ambiguous(Capability::Image, vec![id("mock-image"), id("mock-image-paid")]));
    let bad = Request::Image(ImageRequest {
        prompt: String::new(),
        width: 0,
        height: 64,
        count: 1,
        params: BTreeMap::from([("steps".to_owned(), json!(5))]),
        ..ImageRequest::default()
    });
    let Err(PreviewError::Invalid(problems)) =
        rig.host.preview(&bad, Some(&id("mock-image")), Requester::Person, &InputRights::new())
    else {
        panic!("not invalid");
    };
    let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
    assert!(fields.contains(&"width") && fields.iter().any(|f| f.starts_with("params")), "{fields:?}");
    assert!(problems.len() >= 3, "{problems:?}");
}

#[test]
fn four_paid_images_are_stored_with_provenance_and_repeat_from_the_run_id() {
    let rig = Rig::new();
    rig.enable("mock-image-paid", Some("paid"));
    rig.set_key("mock-image-paid");
    let meter = RecordingMeter::new();
    let c = call(&rig, "mock-image-paid", images(4), 160_000, 42);
    let r = run_with(&rig, &c, &NoProgress, &meter).unwrap();
    assert_eq!(r.candidates.len(), 4);
    assert_eq!((r.usage.usd_micros, meter.total()), (160_000, 160_000));
    let seed = Rng::new(42).next_u64();
    let mut hashes: Vec<_> = r.candidates.iter().map(|c| c.asset).collect();
    hashes.dedup();
    assert_eq!(hashes.len(), 4, "distinct outputs");
    for (i, cand) in r.candidates.iter().enumerate() {
        assert_eq!(cand.id.as_str(), format!("r42-{i}"));
        assert_eq!((cand.state.clone(), cand.usd_micros), (CandidateState::Pending, 40_000));
        assert_eq!(cand.request.seed(), Some(seed));
        let p = rig.store.provenance(&cand.asset).unwrap();
        let Source::Generated { tool, model, plan, seed: s, prompt, .. } = &p.source else { panic!("{p:?}") };
        assert_eq!((tool.as_str(), model.as_str(), plan.as_deref()), ("mock-image-paid", "mock-image@1", Some("paid")));
        assert!(s.is_some() && prompt.as_deref() == Some("chibi fox mascot, front view"));
        assert_eq!(p, cand.provenance);
    }
    let again = run(&rig, &c).unwrap();
    assert_eq!(again.candidates.iter().map(|c| c.asset).collect::<Vec<_>>(), hashes, "the seed came from the run");
}

#[test]
fn caps_stop_before_and_during_the_run() {
    let rig = Rig::new();
    rig.enable("mock-image-paid", Some("paid"));
    rig.set_key("mock-image-paid");
    // The estimate (160,000) is above the cap: nothing runs.
    let e = run(&rig, &call(&rig, "mock-image-paid", images(4), 10_000, 1)).unwrap_err();
    assert_eq!(e, RunError::cap_reached(10_000, 160_000));
    assert_eq!(rig.runs("mock-image-paid"), 0);
    // A meter that refuses past 100,000 stops it after two outputs (the third is billed, then refused).
    let meter = RecordingMeter::capped(100_000);
    let r = run_with(&rig, &call(&rig, "mock-image-paid", images(4), 160_000, 2), &NoProgress, &meter).unwrap();
    assert_eq!(r.candidates.len(), 3);
    assert!(matches!(r.stopped, Some(ConnectError::CapReached { .. })), "{:?}", r.stopped);
    // The adapter's own cap check, with the cap below the estimate's run but above the plan's: a dearer
    // expectation fails as a changed preview, with no run.
    let mut c = call(&rig, "mock-image-paid", images(4), 160_000, 3);
    c.expect_usd_micros = Some(100_000);
    let before = rig.runs("mock-image-paid");
    assert_eq!(run(&rig, &c).unwrap_err(), RunError::preview_changed(Widening::estimate(100_000, 160_000)));
    assert_eq!(rig.runs("mock-image-paid"), before);
}

#[test]
fn a_cancel_keeps_the_complete_outputs() {
    let rig = Rig::new();
    rig.enable("mock-image-paid", Some("paid"));
    rig.set_key("mock-image-paid");
    let after_two = CancelAfter(0.into(), 2);
    let meter = RecordingMeter::new();
    let r = run_with(&rig, &call(&rig, "mock-image-paid", images(4), 160_000, 7), &after_two, &meter).unwrap();
    assert_eq!((r.candidates.len(), r.stopped.clone()), (2, Some(ConnectError::Cancelled)));
    assert_eq!((r.usage.usd_micros, meter.total()), (80_000, 80_000), "only what was delivered is charged");
    let before = CancelAfter(0.into(), 0);
    let e = run_with(&rig, &call(&rig, "mock-image-paid", images(4), 160_000, 8), &before, &meter).unwrap_err();
    assert_eq!(e, RunError::Connector(ConnectError::Cancelled));
}

#[test]
fn answers_never_reach_the_store_or_the_tray() {
    let rig = Rig::new();
    rig.enable("mock-dcc", None);
    let req: Request =
        serde_json::from_value(json!({ "capability": "dcc", "tool": "mock-dcc", "action": "render" })).unwrap();
    let before = rig.store.len();
    let r = run(&rig, &call(&rig, "mock-dcc", req, 0, 9)).unwrap();
    assert_eq!(r.candidates.len(), 0);
    assert!(matches!(&r.answers[..], [Output::Json(v)] if v["action"] == "render"), "{:?}", r.answers);
    assert_eq!(rig.store.len(), before);
}

#[test]
fn a_run_refuses_whatever_goes_further_than_its_plan() {
    let rig = Rig::new();
    rig.enable("mock-image", None);
    let planned = rig.host.preview(&images(1), None, Requester::Agent, &InputRights::new()).unwrap();
    // The person points the local connector at another computer before the job starts.
    let lan =
        ConnectorConfig { enabled: true, endpoint: Some("http://192.168.1.20:8188".into()), ..Default::default() };
    rig.configure("mock-image", lan);
    let mut c = call(&rig, "mock-image", images(1), 0, 11);
    c.requester = Requester::Agent;
    c.planned = Some(planned.clone());
    assert_eq!(run(&rig, &c).unwrap_err(), RunError::preview_changed(Widening::LeavesMachine));
    assert_eq!(rig.runs("mock-image"), 0);
    // A second enabled connector for the capability is not chosen: the call names the plan's.
    rig.enable("mock-image", None);
    rig.enable("mock-image-paid", Some("paid"));
    rig.set_key("mock-image-paid");
    assert_eq!(run(&rig, &c).unwrap().connector, id("mock-image"));
    // A price raised between plan and work (here a dearer count) is refused.
    let mut c = call(&rig, "mock-image-paid", images(2), 1_000_000, 12);
    c.planned = Some(
        rig.host.preview(&images(1), Some(&id("mock-image-paid")), Requester::Person, &InputRights::new()).unwrap(),
    );
    assert!(matches!(run(&rig, &c), Err(RunError::PreviewChanged { .. })));
    // An input marked private between plan and work: refused before anything is sent.
    let asset = rig.import(&png_bytes(b"later private"), Commercial::Ok, false);
    let req = with_refs(1, vec![asset]);
    let planned = rig.host.preview(&req, Some(&id("mock-image-paid")), Requester::Person, &rights(&rig, &req)).unwrap();
    let mut c = call(&rig, "mock-image-paid", req, 1_000_000, 13);
    c.planned = Some(planned);
    c.inputs.get_mut(&asset).unwrap().private_test = true;
    let runs = rig.runs("mock-image-paid");
    assert_eq!(run(&rig, &c).unwrap_err(), RunError::Preview(PreviewError::PrivateInput(asset)));
    assert_eq!(rig.runs("mock-image-paid"), runs);
}

/// The publish block (CN-02 end to end): each case's `licence_gate` on the stored provenance.
#[test]
fn the_publish_gate_reads_what_the_host_wrote() {
    let rig = Rig::new();
    let manifest = |commercial: &str| {
        let text = include_str!("../../omuzaki-connect-mock/connectors/mock-image.connector.toml")
            .replace("id = \"mock-image\"", "id = \"gate-test\"")
            .replace("commercial = \"ok\"", &format!("commercial = \"{commercial}\""));
        ConnectorManifest::from_toml(&text).unwrap()
    };
    let mut table = Vec::new();
    for (case, commercial, plan, requires, input) in [
        ("vendor ok", "ok", None, false, None),
        ("vendor no", "no", None, false, None),
        ("vendor unknown", "unknown", None, false, None),
        ("ok on the free plan of a paid-only vendor", "ok", Some("free"), true, None),
        ("ok on the paid plan", "ok", Some("paid"), true, None),
        ("ok with a non-commercial input", "ok", None, false, Some((Commercial::No, false))),
        ("ok with a private test input", "ok", None, false, Some((Commercial::Ok, true))),
    ] {
        let config = Arc::new(omuzaki_connect_mock::MemoryConfig::new());
        config.set(
            id("gate-test"),
            ConnectorConfig { enabled: true, plan: plan.map(str::to_owned), ..Default::default() },
        );
        let mut m = manifest(commercial);
        if requires {
            m.licence.requires_plan = vec!["paid".into()];
        }
        let host = omuzaki_connect::Host::new(omuzaki_connect::HostParts {
            set: ConnectorSet::from_entries(vec![(m, Box::new(FakeConnector::image(Script::default())) as _)]).unwrap(),
            config,
            secrets: rig.secrets.clone(),
            store: rig.store.clone(),
            http: rig.http.clone(),
            pace: Arc::new(omuzaki_connect_mock::FakePace::default()),
            clock: rig.clock.clone(),
            consent: Rig::no_consent(),
            services: omuzaki_ops_api::Services::default(),
        });
        let refs = match input {
            Some((c, private)) => vec![rig.import(&png_bytes(case.as_bytes()), c, private)],
            None => vec![],
        };
        let req = with_refs(1, refs);
        let mut c = call(&rig, "gate-test", req, 0, 100);
        c.request = Request::Image(match c.request {
            Request::Image(r) => ImageRequest { prompt: case.into(), ..r },
            _ => unreachable!(),
        });
        let env = RunEnv { progress: &NoProgress, cancel: Arc::new(NoCancel), meter: &RecordingMeter::new() };
        let r = host.run(&c, &env).unwrap();
        table.push((case, gate(&effective(&rig, &r.candidates[0]), false)));
    }
    // The cases run the same prompt-free request on fresh hosts: some outputs are identical bytes, stored first
    // under looser terms, and the effective record still blocks them.
    let expected = ["pass", "non_commercial", "unknown", "non_commercial", "pass", "non_commercial", "private_test"];
    for ((case, got), want) in table.iter().zip(expected) {
        println!("publish gate | {case:45} | {got}");
        assert_eq!(*got, want, "{case}");
    }
}

#[test]
fn a_declared_user_model_licence_reaches_the_outputs() {
    let rig = Rig::new();
    let declare = |spdx: &str, commercial| ConnectorConfig {
        enabled: true,
        models: BTreeMap::from([("sd.safetensors".to_owned(), DeclaredModel { spdx: spdx.into(), commercial })]),
        ..Default::default()
    };
    let req = Request::Image(ImageRequest { model: Some("sd.safetensors".into()), ..ImageRequest::default() });
    let req = match (images(1), req) {
        (Request::Image(base), Request::Image(m)) => Request::Image(ImageRequest { model: m.model, ..base }),
        _ => unreachable!(),
    };
    for (spdx, commercial, want) in
        [("CC0-1.0", Commercial::Ok, "pass"), ("LicenseRef-NC", Commercial::No, "non_commercial")]
    {
        rig.configure("comfyui", declare(spdx, commercial));
        let r = run(&rig, &call(&rig, "comfyui", req.clone(), 0, 200)).unwrap();
        let p = effective(&rig, &r.candidates[0]);
        assert_eq!((r.candidates[0].provenance.licence.as_str(), gate(&p, false)), (spdx, want));
    }
    rig.configure("comfyui", ConnectorConfig { enabled: true, ..Default::default() });
    let other = match images(1) {
        Request::Image(base) => {
            Request::Image(ImageRequest { model: Some("undeclared.ckpt".into()), seed: Some(9), ..base })
        }
        _ => unreachable!(),
    };
    let r = run(&rig, &call(&rig, "comfyui", other, 0, 201)).unwrap();
    let p = effective(&rig, &r.candidates[0]);
    assert_eq!((p.licence.as_str(), gate(&p, false)), ("LicenseRef-User-Model-Undeclared", "unknown"));
}

#[test]
fn the_key_is_attached_by_the_host_and_scrubbed_from_everything_that_returns() {
    let rig = Rig::new();
    rig.enable("test-echo", None);
    let seen = Seen::default();
    // Without the key: an auth error that says where to add it.
    let e = run(&rig, &call(&rig, "test-echo", images(1), 10_000, 300)).unwrap_err();
    assert!(matches!(&e, RunError::Preview(PreviewError::NeedsKey(_))), "{e:?}");
    rig.set_key("test-echo");
    // The service echoes the key back in a header, the body raw, percent-encoded and base64.
    let b64 = "c2stdGVzdC0wMTIzNDU2Nzg5YWJjZGVm";
    let echo = omuzaki_http_mock::FakeHttp::new().route(
        omuzaki_http_api::Method::Post,
        "https://echo.invalid/generate",
        vec![omuzaki_http_mock::Reply::Response(omuzaki_http_api::HttpResponse {
            status: 200,
            headers: vec![("x-echo".into(), format!("Bearer {KEY}"))],
            body: format!("{{\"raw\":\"{KEY}\",\"pct\":\"sk-test-0123456789abcdef%20\",\"b64\":\"{b64}\"}}")
                .into_bytes(),
        })],
    );
    let rig2 = rig_with_http(&rig, echo);
    let r = run(&rig2.0, &call(&rig2.0, "test-echo", images(1), 10_000, 301)).unwrap();
    let sent = rig2.1.calls();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].headers.contains(&("authorization".to_owned(), format!("Bearer {KEY}"))), "the key arrives");
    seen.json(&r);
    seen.debug(&r);
    for c in &r.candidates {
        seen.add(String::from_utf8_lossy(&rig2.0.store.get(&c.asset).unwrap()).into_owned());
        seen.json(&rig2.0.store.provenance(&c.asset).unwrap());
    }
    let Output::Text(text) = &r.answers[0] else { panic!() };
    assert!(text.matches("[key]").count() >= 4, "{text}");
    // A failing service whose error echoes the key.
    let failing = omuzaki_http_mock::FakeHttp::new().route(
        omuzaki_http_api::Method::Post,
        "https://echo.invalid/generate",
        vec![omuzaki_http_mock::Reply::Error(omuzaki_http_api::HttpError::Network(format!("reset by {KEY}")))],
    );
    let rig3 = rig_with_http(&rig, failing);
    let e = run(&rig3.0, &call(&rig3.0, "test-echo", images(1), 10_000, 302)).unwrap_err();
    seen.debug(&e);
    seen.add(e.to_string());
    assert!(e.to_string().contains("[key]"), "{e}");
    seen.assert_no_key();
}

/// A rig like `rig` (same settings, keys and store) whose HTTP is `http`.
fn rig_with_http(rig: &Rig, http: omuzaki_http_mock::FakeHttp) -> (Rig, Arc<omuzaki_http_mock::FakeHttp>) {
    let http = Arc::new(http);
    let manifest = ConnectorManifest::from_toml(ECHO).unwrap();
    let set = ConnectorSet::from_entries(vec![(manifest, Box::new(common::EchoConnector::default()) as _)]).unwrap();
    let host = Arc::new(omuzaki_connect::Host::new(omuzaki_connect::HostParts {
        set,
        config: rig.config.clone(),
        secrets: rig.secrets.clone(),
        store: rig.store.clone(),
        http: http.clone(),
        pace: Arc::new(omuzaki_connect_mock::FakePace::default()),
        clock: rig.clock.clone(),
        consent: Rig::no_consent(),
        services: omuzaki_ops_api::Services::default(),
    }));
    let mut out = Rig::new();
    out.host = host;
    out.store = rig.store.clone();
    (out, http)
}

#[test]
fn a_ping_is_free_and_stores_nothing() {
    let rig = Rig::new();
    rig.enable("mock-image", None);
    let before = rig.store.len();
    rig.host.ping(&id("mock-image"), &NoProgress).unwrap();
    assert_eq!(rig.counters["mock-image"].pings.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(rig.store.len(), before);
    assert!(rig.host.ping(&id("mock-music"), &NoProgress).is_err(), "a disabled connector is not pinged");
    let _ = agent("a");
}
