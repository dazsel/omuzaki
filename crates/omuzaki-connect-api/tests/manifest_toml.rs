//! The manifest (#991, design D3): three realistic manifests read and validate, each rule gives exactly one error at
//! its field path, every problem is reported, and the TOML and schema forms are stable.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_connect_api::{
    AuthSpec, Capability, ConnectorManifest, CostModel, Deliver, ManifestError, MediaKind, ParamKind, Transport, Unit,
};

/// A ComfyUI-like local connector (the design's worked example).
const LOCAL: &str = r#"
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

/// A hosted connector, paid per image, key by header.
const HOSTED: &str = r#"
id = "hosted-image"
name = "Hosted images"
version = "2.1.0"
transport = "http"
endpoint = "https://api.example.com/v1"

[[capabilities]]
capability = "image"
outputs = ["image"]
max_count = 4

[auth]
kind = "api_key"
secret = "connector/hosted-image"
deliver = { via = "header", name = "authorization", scheme = "Bearer" }

[cost]
model = "per_unit"
unit = "image"
usd_micros = 40000
priced = "2026-10-13"

[privacy]
leaves_machine = true
destinations = ["api.example.com"]
sends = ["prompt", "images"]
retention = "It keeps them for 30 days."
trains_on_inputs = "unknown"

[licence]
register = "Hosted images"
outputs = "vendor_terms"
spdx = "LicenseRef-Hosted-Terms"
commercial = "ok"
requires_plan = ["pro"]
"#;

/// An MCP connector over stdio with an environment key.
const MCP: &str = r#"
id = "mesh-mcp"
name = "Mesh over MCP"
version = "0.3.0"
transport = "mcp"

[[capabilities]]
capability = "model3d"
inputs = ["image", "text"]
outputs = ["mesh"]
max_count = 1

[auth]
kind = "api_key"
secret = "connector/mesh-mcp"
deliver = { via = "env", name = "MESH_API_KEY" }

[cost]
model = "per_call"
usd_micros = 200000
priced = "2026-10-01"

[privacy]
leaves_machine = true
destinations = ["mesh.example.com"]
sends = ["prompt", "images"]
retention = "Kept for 7 days."
trains_on_inputs = "no"

[licence]
register = "Mesh over MCP"
outputs = "vendor_terms"
spdx = "LicenseRef-Mesh-Terms"
commercial = "ok"

[mcp]
tools = [
  { capability = "model3d", tool = "generate_mesh", args = { prompt = { field = "prompt" }, format = { const = "glb" } }, url_field = "download_url" },
]
"#;

fn parse(text: &str) -> ConnectorManifest {
    ConnectorManifest::from_toml(text).unwrap()
}

/// The single field error a broken manifest gives.
fn only_error(m: &ConnectorManifest) -> String {
    let errors = m.validate().unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    match &errors[0] {
        ManifestError::Field { field, .. } => field.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_three_examples_read_and_validate() {
    for text in [LOCAL, HOSTED, MCP] {
        let m = parse(text);
        assert_eq!(m.validate(), Ok(()), "{}", m.id);
    }
    let local = parse(LOCAL);
    assert_eq!(local.capabilities[0].params[0].kind, ParamKind::int(1, 150));
    assert_eq!(parse(HOSTED).cost, CostModel::per_unit(Unit::Image, 40_000, "2026-10-13"));
    assert_eq!(parse(MCP).auth, AuthSpec::api_key(Some("connector/mesh-mcp".into()), Deliver::env("MESH_API_KEY")));
}

#[test]
fn paid_and_declares() {
    assert!(!parse(LOCAL).paid());
    assert!(parse(HOSTED).paid() && parse(MCP).paid());
    let m = parse(HOSTED);
    assert_eq!(m.declares(Capability::Image).map(|d| d.max_count), Some(4));
    assert!(m.declares(Capability::Voice).is_none());
}

#[test]
fn unknown_keys_are_parse_errors() {
    let text = LOCAL.replace("transport = \"http\"", "transport = \"http\"\ncolour = \"red\"");
    assert!(matches!(ConnectorManifest::from_toml(&text), Err(ManifestError::Parse(e)) if e.contains("colour")));
    let text = HOSTED.replace("scheme = \"Bearer\"", "scheme = \"Bearer\", extra = 1");
    assert!(matches!(ConnectorManifest::from_toml(&text), Err(ManifestError::Parse(_))));
}

/// Each rule broken alone gives exactly one error, at its field.
#[test]
fn each_rule_names_its_field() {
    let cases: Vec<(ConnectorManifest, &str)> = vec![
        (with(LOCAL, |m| m.name = String::new()), "name"),
        (with(LOCAL, |m| m.name = "x".repeat(81)), "name"),
        (with(LOCAL, |m| m.capabilities.clear()), "capabilities"),
        (with(LOCAL, |m| m.capabilities.push(m.capabilities[0].clone())), "capabilities.1.capability"),
        (with(LOCAL, |m| m.capabilities[0].max_count = 9), "capabilities.0.max_count"),
        (with(LOCAL, |m| m.capabilities[0].max_count = 0), "capabilities.0.max_count"),
        (with(LOCAL, |m| m.capabilities[0].outputs = vec![MediaKind::Mesh]), "capabilities.0.outputs.0"),
        (with(LOCAL, |m| m.capabilities[0].params[1].name = "steps".into()), "capabilities.0.params.1.name"),
        (with(LOCAL, |m| m.capabilities[0].params[0].name = "Steps".into()), "capabilities.0.params.0.name"),
        (with(LOCAL, |m| m.capabilities[0].params[0].kind = ParamKind::int(5, 1)), "capabilities.0.params.0.kind"),
        (with(LOCAL, |m| m.capabilities[0].params[0].default = Some(999.into())), "capabilities.0.params.0.default"),
        (with(HOSTED, |m| set_secret(m, "connector/other")), "auth.secret"),
        (with(HOSTED, |m| set_deliver(m, Deliver::header("bad header", "Bearer"))), "auth.deliver.name"),
        (with(HOSTED, |m| set_deliver(m, Deliver::header("authorization", "Bea rer"))), "auth.deliver.scheme"),
        (with(HOSTED, |m| set_deliver(m, Deliver::env("API_KEY"))), "auth.deliver"),
        (with(MCP, |m| set_deliver(m, Deliver::env("api_key"))), "auth.deliver.name"),
        (with(MCP, |m| set_deliver(m, Deliver::header("x-key", ""))), "auth.deliver"),
        (with(LOCAL, |m| m.privacy.destinations.push("api.example.com".into())), "privacy.destinations.1"),
        (with(HOSTED, |m| m.privacy.destinations.push("localhost".into())), "privacy.destinations.1"),
        (with(MCP, |m| m.privacy.destinations.clear()), "privacy.destinations"),
        (with(HOSTED, |m| m.privacy.retention = " ".into()), "privacy.retention"),
        (with(HOSTED, |m| m.cost = CostModel::per_unit(Unit::Image, 1, "13-10-2026")), "cost.priced"),
        (with(HOSTED, |m| m.licence.register = String::new()), "licence.register"),
        (with(HOSTED, |m| m.licence.spdx = String::new()), "licence.spdx"),
        (with(LOCAL, |m| m.licence.commercial = omuzaki_store_api::Commercial::Ok), "licence.commercial"),
        (with(HOSTED, |m| m.licence.requires_plan = vec!["Pro Plan".into()]), "licence.requires_plan.0"),
        (with(LOCAL, |m| m.endpoint = None), "endpoint"),
        (with(HOSTED, |m| m.endpoint = Some("http://api.example.com".into())), "endpoint"),
        (with(HOSTED, |m| m.endpoint = Some("https://elsewhere.example.com".into())), "endpoint"),
        (with(MCP, |m| m.endpoint = Some("https://mesh.example.com".into())), "endpoint"),
        (with(MCP, |m| m.mcp = None), "mcp"),
        (with(LOCAL, |m| m.mcp = parse(MCP).mcp), "mcp"),
        (with(MCP, |m| m.mcp.as_mut().unwrap().tools[0].capability = Capability::Image), "mcp.tools.0.capability"),
    ];
    for (i, (m, field)) in cases.iter().enumerate() {
        // The tool map case also leaves model3d unserved: that is its second, separate rule, so it is excluded here.
        if *field == "mcp.tools.0.capability" {
            let errors = m.validate().unwrap_err();
            assert!(errors.iter().any(|e| matches!(e, ManifestError::Field { field: f, .. } if f == field)));
            continue;
        }
        assert_eq!(only_error(m), *field, "case {i}");
    }
}

#[test]
fn llm_transport_rules() {
    let llm = |f: fn(&mut ConnectorManifest)| {
        with(HOSTED, |m| {
            m.transport = Transport::Llm;
            m.endpoint = None;
            m.cost = CostModel::Provider;
            m.auth = AuthSpec::api_key(None, Deliver::Provider);
            m.capabilities[0].capability = Capability::Llm;
            m.capabilities[0].outputs = vec![MediaKind::Text, MediaKind::Json];
            f(m);
        })
    };
    assert_eq!(llm(|_| {}).validate(), Ok(()));
    assert_eq!(only_error(&llm(|m| m.cost = CostModel::per_call(1, "2026-10-13"))), "cost.model");
    assert_eq!(
        only_error(&llm(|m| m.auth = AuthSpec::api_key(Some("connector/hosted-image".into()), Deliver::Provider))),
        "auth.secret"
    );
    assert_eq!(only_error(&llm(|m| m.endpoint = Some("https://api.example.com".into()))), "endpoint");
    assert_eq!(only_error(&with(HOSTED, |m| m.cost = CostModel::Provider)), "cost.model");
    assert_eq!(only_error(&with(HOSTED, |m| m.auth = AuthSpec::api_key(None, Deliver::Provider))), "auth.deliver");
    let image = llm(|m| m.capabilities[0].capability = Capability::Image);
    assert!(image.validate().unwrap_err().iter().any(|e| e.to_string().starts_with("capabilities.0.capability")));
}

#[test]
fn dcc_maps_several_tools_and_others_one() {
    let dcc = |tools: &[(&str, &str)]| {
        with(MCP, |m| {
            m.capabilities[0].capability = Capability::Dcc;
            let tool = m.mcp.as_ref().unwrap().tools[0].clone();
            m.mcp.as_mut().unwrap().tools = tools
                .iter()
                .map(|(cap, name)| {
                    let mut t = tool.clone();
                    t.capability = serde_json::from_value(serde_json::json!(cap)).unwrap();
                    t.tool = (*name).into();
                    t
                })
                .collect();
        })
    };
    assert_eq!(dcc(&[("dcc", "export"), ("dcc", "render")]).validate(), Ok(()));
    assert_eq!(only_error(&dcc(&[("dcc", "export"), ("dcc", "export")])), "mcp.tools.1.tool");
    let two = with(MCP, |m| {
        let tool = m.mcp.as_ref().unwrap().tools[0].clone();
        let mut other = tool.clone();
        other.tool = "second".into();
        m.mcp.as_mut().unwrap().tools.push(other);
    });
    assert_eq!(only_error(&two), "mcp.tools.1");
}

#[test]
fn every_problem_is_reported() {
    let m = with(HOSTED, |m| {
        m.name = String::new();
        m.licence.spdx = String::new();
        m.privacy.retention = String::new();
    });
    let fields: Vec<String> = m.validate().unwrap_err().iter().map(ToString::to_string).collect();
    assert_eq!(fields.len(), 3, "{fields:?}");
    assert!(fields[0].starts_with("name:") && fields[1].starts_with("privacy.retention:"));
    assert!(fields[2].starts_with("licence.spdx:"));
}

#[test]
fn toml_round_trip_and_schema() {
    for text in [LOCAL, HOSTED, MCP] {
        let m = parse(text);
        let again = parse(&m.to_toml().unwrap());
        assert_eq!(again, m);
        assert_eq!(again.to_toml().unwrap(), m.to_toml().unwrap());
    }
    let schema = serde_json::to_value(schemars::schema_for!(ConnectorManifest)).unwrap();
    assert_eq!(schema["type"], "object");
    assert!(schema["required"].as_array().unwrap().iter().any(|f| f == "capabilities"));
}

fn with(text: &str, f: impl FnOnce(&mut ConnectorManifest)) -> ConnectorManifest {
    let mut m = parse(text);
    f(&mut m);
    m
}

fn set_secret(m: &mut ConnectorManifest, label: &str) {
    let AuthSpec::ApiKey { deliver, .. } = m.auth.clone() else { panic!("an api key") };
    m.auth = AuthSpec::api_key(Some(label.into()), deliver);
}

fn set_deliver(m: &mut ConnectorManifest, deliver: Deliver) {
    let AuthSpec::ApiKey { secret, .. } = m.auth.clone() else { panic!("an api key") };
    m.auth = AuthSpec::api_key(secret, deliver);
}
