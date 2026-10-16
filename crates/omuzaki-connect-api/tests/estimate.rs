//! Estimates, previews and settings (#993, design D2, D4, D5, D12): exact integer prices, the English sentence, the
//! widening rule a run checks against its plan, the target resolved from settings, and `ConnectorConfig`'s form.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use omuzaki_connect_api::{
    AuthSpec, Capability, ChatMessage, ConnectError, ConnectorConfig, ConnectorId, ConnectorManifest, CostModel,
    DeclaredModel, Deliver, Estimate, ImageRequest, LlmRequest, MusicRequest, Outbound, Preview, Request, Role, Sends,
    Target, Tri, Unit, VoiceChoice, VoiceRequest, Widening, estimate_from_manifest,
};
use omuzaki_store_api::{Commercial, ContentHash};

const MANIFEST: &str = r#"
id = "hosted"
name = "Hosted"
version = "1.0.0"
transport = "http"
endpoint = "https://api.example.com"
[[capabilities]]
capability = "image"
outputs = ["image"]
max_count = 8
[auth]
kind = "api_key"
secret = "connector/hosted"
deliver = { via = "header", name = "authorization", scheme = "Bearer" }
[cost]
model = "free"
[privacy]
leaves_machine = true
destinations = ["api.example.com"]
retention = "It keeps them for 30 days."
trains_on_inputs = "no"
[licence]
register = "Hosted"
outputs = "vendor_terms"
spdx = "LicenseRef-Hosted"
commercial = "ok"
"#;

fn manifest(cost: CostModel) -> ConnectorManifest {
    let mut m = ConnectorManifest::from_toml(MANIFEST).unwrap();
    m.cost = cost;
    m
}

fn images(count: u8) -> Request {
    Request::Image(ImageRequest { prompt: "fox".into(), width: 64, height: 64, count, ..ImageRequest::default() })
}

fn estimate(cost: CostModel, req: &Request) -> Estimate {
    estimate_from_manifest(&manifest(cost), req).unwrap()
}

#[test]
fn per_unit_prices_are_exact() {
    let e = estimate(CostModel::per_unit(Unit::Image, 40_000, "2026-10-13"), &images(4));
    assert_eq!((e.usd_micros, e.exact, e.basis.as_str()), (160_000, true, "4 images x $0.04"));
    assert_eq!(estimate(CostModel::Free, &images(4)).usd_micros, 0);
    let call = estimate(CostModel::per_call(50_000, "2026-10-13"), &images(4));
    assert_eq!((call.usd_micros, call.exact), (50_000, true));
    let music = Request::Music(MusicRequest { prompt: "a".into(), seconds: 30, count: 2, ..MusicRequest::default() });
    assert_eq!(estimate(CostModel::per_unit(Unit::Second, 10_000, "2026-10-13"), &music).usd_micros, 600_000);
    let voice = Request::Voice(VoiceRequest {
        text: "a".repeat(2_001),
        voice: VoiceChoice::preset("kit"),
        language: None,
        seed: None,
        params: BTreeMap::new(),
    });
    let chars = estimate(CostModel::per_unit(Unit::ThousandChars, 1, "2026-10-13"), &voice);
    assert_eq!((chars.usd_micros, chars.basis.as_str()), (3, "3 thousand characters x $0.000001"));
}

#[test]
fn per_token_is_an_upper_bound() {
    let req = Request::Llm(LlmRequest {
        messages: vec![ChatMessage { role: Role::User, text: "x".repeat(3_000) }],
        max_tokens: 500,
        ..LlmRequest::default()
    });
    // A vision-like request with one image: build it as an LLM request's text plus one input asset.
    let vision = Request::Vision(omuzaki_connect_api::VisionRequest {
        instruction: "x".repeat(3_000),
        images: vec![ContentHash::of(b"img")],
        max_tokens: 500,
        ..Default::default()
    });
    let price = CostModel::per_token(3_000_000, 15_000_000, "2026-10-13");
    let e = estimate(price.clone(), &vision);
    // 1,000 text tokens + 1,600 for the image = 2,600 in; 500 out.
    assert_eq!((e.usd_micros, e.exact), (7_800 + 7_500, false));
    assert_eq!(e.basis, "at most 2600 input and 500 output tokens");
    assert_eq!(estimate(price, &req).usd_micros, 3_000 + 7_500);
}

#[test]
fn sums_are_exact_integers() {
    let total: u64 = (0..10).map(|_| estimate(CostModel::per_call(100, "2026-10-13"), &images(1)).usd_micros).sum();
    assert_eq!(total, 1_000);
}

#[test]
fn unpriceable_requests_are_unsupported() {
    let provider = estimate_from_manifest(&manifest(CostModel::Provider), &images(1));
    assert!(matches!(provider, Err(ConnectError::Unsupported { .. })));
    let seconds = estimate_from_manifest(&manifest(CostModel::per_unit(Unit::Second, 1, "2026-10-13")), &images(1));
    assert!(matches!(seconds, Err(ConnectError::Unsupported { .. })));
}

fn preview() -> Preview {
    Preview {
        connector: ConnectorId::new("hosted").unwrap(),
        name: "Hosted".into(),
        capability: Capability::Image,
        leaves_machine: true,
        destinations: vec!["api.example.com".into()],
        endpoint: Some("https://api.example.com".into()),
        outbound: vec![
            Outbound { what: Sends::Images, count: 3, bytes: 3_000 },
            Outbound { what: Sends::Prompt, count: 1, bytes: 12 },
        ],
        estimate: Estimate { usd_micros: 120_000, exact: true, basis: "3 images x $0.04".into(), seconds: None },
        retention: "It keeps them for 30 days.".into(),
        trains_on_inputs: Tri::No,
        commercial: Commercial::Ok,
        licence: "LicenseRef-Hosted".into(),
        plan: None,
        secret: None,
        needs_spend: true,
        needs_person: None,
        warnings: vec![],
    }
}

#[test]
fn english_words_the_preview() {
    assert_eq!(
        preview().english(),
        "This sends 3 images and a prompt to api.example.com and costs $0.12. It keeps them for 30 days."
    );
    let local = Preview { leaves_machine: false, estimate: Estimate::free(), ..preview() };
    assert_eq!(local.english(), "This stays on this computer and costs nothing.");
    let trains = Preview {
        trains_on_inputs: Tri::Yes,
        estimate: Estimate { exact: false, ..preview().estimate },
        destinations: vec!["a.example.com".into(), "b.example.com".into()],
        ..preview()
    };
    assert_eq!(
        trains.english(),
        "This sends 3 images and a prompt to a.example.com and b.example.com and costs at most $0.12. \
         It keeps them for 30 days. It may train on what it is sent."
    );
}

#[test]
fn only_wider_previews_are_refused() {
    let planned = preview();
    assert_eq!(planned.widens(&planned), None);
    let cases: Vec<(Preview, Widening)> = vec![
        (Preview { leaves_machine: true, ..planned.clone() }, Widening::LeavesMachine),
        (
            Preview { destinations: vec!["api.example.com".into(), "x.example.com".into()], ..planned.clone() },
            Widening::destination("x.example.com"),
        ),
        (Preview { endpoint: Some("https://lan".into()), ..planned.clone() }, Widening::endpoint("https://lan")),
        (
            Preview { outbound: vec![Outbound { what: Sends::Images, count: 3, bytes: 3_001 }], ..planned.clone() },
            Widening::outbound(Sends::Images),
        ),
        (
            // Two rows of the planned size send twice what was planned.
            Preview { outbound: vec![Outbound { what: Sends::Images, count: 3, bytes: 3_000 }; 2], ..planned.clone() },
            Widening::outbound(Sends::Images),
        ),
        (Preview { trains_on_inputs: Tri::Yes, ..planned.clone() }, Widening::TrainsOnInputs),
        (Preview { retention: "Forever.".into(), ..planned.clone() }, Widening::Retention),
        (
            Preview {
                secret: Some(omuzaki_connect_api::SecretLabel::new("connector/llm/b").unwrap()),
                ..planned.clone()
            },
            Widening::Secret,
        ),
        (Preview { needs_person: Some("confirm".into()), ..planned.clone() }, Widening::NeedsPerson),
        (Preview { plan: Some("free".into()), ..planned.clone() }, Widening::Plan),
        (
            Preview { estimate: Estimate { usd_micros: 120_001, ..planned.estimate.clone() }, ..planned.clone() },
            Widening::estimate(120_000, 120_001),
        ),
    ];
    // The first case only widens against a plan that stayed on this computer.
    let stayed = Preview { leaves_machine: false, ..planned.clone() };
    assert_eq!(cases[0].0.widens(&stayed), Some(cases[0].1.clone()));
    for (i, (now, why)) in cases.iter().enumerate().skip(1) {
        assert_eq!(now.widens(&planned).as_ref(), Some(why), "case {i}");
    }
    let narrower = Preview {
        outbound: vec![Outbound { what: Sends::Images, count: 1, bytes: 10 }],
        estimate: Estimate { usd_micros: 40_000, ..planned.estimate.clone() },
        commercial: Commercial::No,
        destinations: vec![],
        ..planned.clone()
    };
    assert_eq!(narrower.widens(&planned), None);
}

#[test]
fn config_round_trips_and_starts_disabled() {
    assert!(!ConnectorConfig::default().enabled);
    let config = ConnectorConfig {
        enabled: true,
        endpoint: Some("http://192.168.1.4:8188".into()),
        plan: Some("pro".into()),
        command: Some(vec!["blender-mcp".into(), "--stdio".into()]),
        models: BTreeMap::from([(
            "sd_xl_base_1.0.safetensors".to_owned(),
            DeclaredModel { spdx: "LicenseRef-OpenRAIL-M".into(), commercial: Commercial::Ok },
        )]),
        provider: Some("local".into()),
    };
    let json = serde_json::to_string(&config).unwrap();
    assert_eq!(serde_json::from_str::<ConnectorConfig>(&json).unwrap(), config);
    let toml_form: ConnectorConfig = toml::from_str("enabled = true").unwrap();
    assert_eq!(toml_form, ConnectorConfig { enabled: true, ..ConnectorConfig::default() });
}

#[test]
fn a_lan_endpoint_makes_the_target_leave_the_machine() {
    let mut m = manifest(CostModel::Free);
    m.endpoint = Some("http://127.0.0.1:8188".into());
    m.privacy.leaves_machine = false;
    m.privacy.destinations = vec!["127.0.0.1".into()];
    m.auth = AuthSpec::None;
    let local = Target::from_manifest(&m, &ConnectorConfig::default()).unwrap();
    assert!(!local.leaves_machine && local.secret.is_none());
    assert_eq!(local.endpoint.as_deref(), Some("http://127.0.0.1:8188"));
    let lan = ConnectorConfig { endpoint: Some("http://192.168.1.4:8188".into()), ..ConnectorConfig::default() };
    let target = Target::from_manifest(&m, &lan).unwrap();
    assert!(target.leaves_machine);
    assert_eq!(target.destinations, ["127.0.0.1", "192.168.1.4"]);
    let bad = ConnectorConfig { endpoint: Some("ftp://x".into()), ..ConnectorConfig::default() };
    assert!(matches!(Target::from_manifest(&m, &bad), Err(ConnectError::Unavailable { .. })));
    m.auth = AuthSpec::api_key(Some("connector/hosted".into()), Deliver::header("authorization", "Bearer"));
    let keyed = Target::from_manifest(&m, &ConnectorConfig::default()).unwrap();
    assert_eq!(keyed.secret.map(|s| s.label().to_owned()).as_deref(), Some("connector/hosted"));
}
