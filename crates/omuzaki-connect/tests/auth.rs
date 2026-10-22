//! `AuthedHttp` over `FakeHttp`: the key attached only to the target's hosts, never set by a connector, never sent
//! over plain http:// to another computer, and scrubbed (raw, percent-encoded, base64) from headers, bodies, streams
//! and errors.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::io::Read;
use std::sync::Arc;

use omuzaki_connect::{AuthedHttp, scrub_str};
use omuzaki_connect_api::{ConnectorConfig, ConnectorManifest, Target};
use omuzaki_http_api::{Cancel, HttpBody, HttpError, HttpRequest, HttpResponse, HttpTransport, Method};
use omuzaki_http_mock::{Chunk, FakeHttp, Reply, StreamScript};
use omuzaki_secrets_api::prelude::SecretValue;

const KEY: &str = "test-key-0123456789abcdef";
const B64: &str = "dGVzdC1rZXktMDEyMzQ1Njc4OWFiY2RlZg==";

fn manifest(auth: &str) -> ConnectorManifest {
    ConnectorManifest::from_toml(&format!(
        r#"
id = "hosted"
name = "Hosted"
version = "1.0.0"
transport = "http"
endpoint = "https://api.example.com"

[[capabilities]]
capability = "image"
inputs = ["text"]
outputs = ["image"]
max_count = 1

{auth}

[cost]
model = "free"

[privacy]
leaves_machine = true
destinations = ["api.example.com"]
sends = ["prompt"]
retention = "Thirty days."
trains_on_inputs = "no"

[licence]
register = "Hosted"
outputs = "vendor_terms"
spdx = "LicenseRef-Hosted"
commercial = "ok"
"#
    ))
    .unwrap()
}

const HEADER: &str = r#"[auth]
kind = "api_key"
secret = "connector/hosted"
deliver = { via = "header", name = "x-api-key", scheme = "" }"#;

fn target(m: &ConnectorManifest, endpoint: Option<&str>) -> Target {
    let config = ConnectorConfig { enabled: true, endpoint: endpoint.map(str::to_owned), ..Default::default() };
    Target::from_manifest(m, &config).unwrap()
}

fn get(url: &str) -> HttpRequest {
    HttpRequest {
        method: Method::Get,
        url: url.into(),
        headers: vec![],
        body: HttpBody::Empty,
        timeout_s: 5,
        max_response: 1 << 20,
    }
}

struct No;
impl Cancel for No {
    fn is_cancelled(&self) -> bool {
        false
    }
}

fn reply(headers: Vec<(String, String)>, body: Vec<u8>) -> Vec<Reply> {
    vec![Reply::Response(HttpResponse { status: 200, headers, body })]
}

#[test]
fn the_key_goes_in_the_manifests_header_to_the_allowed_host_only() {
    let m = manifest(HEADER);
    let t = target(&m, None);
    let key = SecretValue::new(KEY);
    let fake = FakeHttp::new().route(Method::Get, "https://api.example.com/v1", reply(vec![], b"{}".to_vec()));
    let http = AuthedHttp::new(&fake, &m, &t, Some(&key));
    http.send(&get("https://api.example.com/v1"), &No).unwrap();
    assert_eq!(fake.calls()[0].headers, vec![("x-api-key".to_owned(), KEY.to_owned())]);
    // Another host: refused, nothing sent.
    let e = http.send(&get("https://elsewhere.example.org/v1"), &No).unwrap_err();
    assert!(matches!(e, HttpError::Refused(ref why) if why.contains("host not allowed")), "{e:?}");
    // A connector setting an auth header itself: refused.
    let mut own = get("https://api.example.com/v1");
    own.headers.push(("Authorization".into(), "Bearer mine".into()));
    assert!(matches!(http.send(&own, &No), Err(HttpError::Refused(ref why)) if why.contains("never set auth")));
    assert_eq!(fake.calls().len(), 1);
    // A connector without auth sends no header.
    let free = manifest("[auth]\nkind = \"none\"");
    let plain = AuthedHttp::new(&fake, &free, &target(&free, None), Some(&key));
    plain.send(&get("https://api.example.com/v1"), &No).unwrap();
    assert_eq!(fake.calls()[1].headers, vec![]);
}

#[test]
fn a_key_never_goes_over_plain_http_to_another_computer() {
    let m = manifest(HEADER);
    let key = SecretValue::new(KEY);
    let fake = FakeHttp::new().route(Method::Get, "http://192.168.1.20:8188/v1", reply(vec![], vec![])).route(
        Method::Get,
        "http://127.0.0.1:8188/v1",
        reply(vec![], vec![]),
    );
    let lan = target(&m, Some("http://192.168.1.20:8188"));
    let e = AuthedHttp::new(&fake, &m, &lan, Some(&key)).send(&get("http://192.168.1.20:8188/v1"), &No).unwrap_err();
    assert!(matches!(e, HttpError::Refused(ref why) if why.contains("plain http://")), "{e:?}");
    let local = target(&m, Some("http://127.0.0.1:8188"));
    AuthedHttp::new(&fake, &m, &local, Some(&key)).send(&get("http://127.0.0.1:8188/v1"), &No).unwrap();
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn every_form_of_the_key_is_scrubbed_from_what_comes_back() {
    let m = manifest(HEADER);
    let t = target(&m, None);
    let key = SecretValue::new(KEY);
    let body = format!("{{\"raw\":\"{KEY}\",\"pct\":\"q={KEY}%21\",\"b64\":\"{B64}\"}}");
    let mut binary = vec![0xFF, 0x00, 0xFE];
    binary.extend_from_slice(KEY.as_bytes());
    binary.push(0x80);
    let fake = FakeHttp::new()
        .route(
            Method::Get,
            "https://api.example.com/json",
            reply(vec![("x-echo".into(), format!("key={KEY}"))], body.into_bytes()),
        )
        .route(Method::Get, "https://api.example.com/bin", reply(vec![], binary))
        .route(
            Method::Get,
            "https://api.example.com/err",
            vec![Reply::Error(HttpError::Network(format!("reset after {KEY}")))],
        );
    let http = AuthedHttp::new(&fake, &m, &t, Some(&key));
    let r = http.send(&get("https://api.example.com/json"), &No).unwrap();
    let text = String::from_utf8(r.body).unwrap();
    assert_eq!(text, "{\"raw\":\"[key]\",\"pct\":\"q=[key]%21\",\"b64\":\"[key]\"}");
    assert_eq!(r.headers, vec![("x-echo".to_owned(), "key=[key]".to_owned())]);
    let bin = http.send(&get("https://api.example.com/bin"), &No).unwrap().body;
    assert_eq!(bin, [&[0xFF, 0x00, 0xFE][..], b"[key]", &[0x80]].concat());
    let e = http.send(&get("https://api.example.com/err"), &No).unwrap_err();
    assert_eq!(e, HttpError::Network("reset after [key]".into()));
    let all = format!("{KEY} {KEY} {B64}");
    assert_eq!(scrub_str(&key, &all), "[key] [key] [key]");
    let special = SecretValue::new("k3y/with+odd=chars");
    assert_eq!(scrub_str(&special, "a=k3y%2Fwith%2Bodd%3Dchars&b"), "a=[key]&b");
}

#[test]
fn a_streamed_body_is_scrubbed_across_chunk_boundaries() {
    let m = manifest(HEADER);
    let t = target(&m, None);
    let key = SecretValue::new(KEY);
    let (a, b) = KEY.split_at(9);
    let script = StreamScript {
        status: 200,
        headers: vec![],
        chunks: vec![Chunk::Data(format!("data: {a}").into_bytes()), Chunk::Data(format!("{b} end").into_bytes())],
    };
    let fake = FakeHttp::new().route(Method::Get, "https://api.example.com/sse", vec![Reply::Stream(script)]);
    let http = AuthedHttp::new(&fake, &m, &t, Some(&key));
    let mut stream = http.open(&get("https://api.example.com/sse"), Arc::new(No)).unwrap();
    let mut out = String::new();
    stream.body.read_to_string(&mut out).unwrap();
    assert_eq!(out, "data: [key] end");
}

#[test]
fn keys_that_go_elsewhere_or_look_wrong_are_refused_before_any_request() {
    let fake = FakeHttp::new().route(Method::Get, "https://api.example.com/v1", reply(vec![], vec![]));
    let env = manifest(
        "[auth]\nkind = \"api_key\"\nsecret = \"connector/hosted\"\ndeliver = { via = \"env\", name = \"HOSTED_KEY\" }",
    );
    let m = manifest(HEADER);
    let key = SecretValue::new(KEY);
    let e = AuthedHttp::new(&fake, &env, &target(&env, None), Some(&key)).send(&get("https://api.example.com/v1"), &No);
    assert!(matches!(e, Err(HttpError::Refused(ref why)) if why.contains("environment")), "{e:?}");
    let short = SecretValue::new("1234567");
    let e = AuthedHttp::new(&fake, &m, &target(&m, None), Some(&short)).send(&get("https://api.example.com/v1"), &No);
    assert_eq!(e, Err(HttpError::Refused("that does not look like a key".into())));
    let e = AuthedHttp::new(&fake, &m, &target(&m, None), None).send(&get("https://api.example.com/v1"), &No);
    assert!(matches!(e, Err(HttpError::Refused(ref why)) if why.contains("keychain")), "{e:?}");
    assert_eq!(fake.calls().len(), 0);
    let http = AuthedHttp::new(&fake, &m, &target(&m, None), Some(&key));
    let debug = format!("{http:?}");
    assert!(!debug.contains(KEY) && !debug.contains(B64) && debug.contains("api.example.com"), "{debug}");
}
