//! The JSON client (#2348) against the loopback fake server: the request it sends, the key as a bearer token that never
//! shows in an error, the status mapping, and the refusal of a remote host without `allow_remote`.

mod common;

use std::time::Duration;

use common::{Reply, Server, runtime_handle};
use omuzaki_llm::json::JsonClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::provider::SecretValue;
use serde_json::json;

const KEY: &str = "sk-json-test-0123456789";

fn client(server: &Server) -> JsonClient {
    JsonClient::new(&server.url, Some(SecretValue::new(KEY)), false, &runtime_handle(), Duration::from_secs(5))
        .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn a_post_sends_json_with_the_key_and_reads_the_answer() {
    let server = Server::start(vec![Reply::status(200, r#"{"answers":{"commit":{"type":"noul","noul":0.9}}}"#)]);
    let answer = client(&server).post("/v1/systemone", &json!({ "state": "hi", "questions": {} }));
    assert_eq!(answer, Ok(json!({ "answers": { "commit": { "type": "noul", "noul": 0.9 } } })));
    let seen = &server.requests()[0];
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/v1/systemone"));
    assert_eq!(seen.header("authorization"), Some(format!("Bearer {KEY}").as_str()));
    assert_eq!(seen.header("content-type"), Some("application/json"));
    assert_eq!(seen.json(), json!({ "state": "hi", "questions": {} }));
}

#[test]
fn statuses_map_to_errors_that_never_carry_the_key() {
    let server = Server::start(vec![
        Reply::status(401, "{}"),
        Reply::status(429, "{}"),
        Reply::status(503, "{}"),
        Reply::status(400, &format!(r#"{{"error":"bad key {KEY}"}}"#)),
        Reply::status(200, "not json"),
    ]);
    let client = client(&server);
    let post = || client.post("/v1/systemone", &json!({}));
    assert_eq!(post(), Err(LlmError::Auth));
    assert!(matches!(post(), Err(LlmError::RateLimited { .. })));
    assert_eq!(post(), Err(LlmError::Overloaded));
    match post() {
        Err(LlmError::Network(m)) => assert!(m.contains("400") && !m.contains(KEY), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(post(), Err(LlmError::Malformed(_))));
    assert!(!format!("{client:?}").contains(KEY));
}

#[test]
fn a_remote_host_needs_allow_remote_and_https() {
    let runtime = runtime_handle();
    let limit = Duration::from_secs(1);
    let refused = JsonClient::new("https://api.example.com", None, false, &runtime, limit);
    assert!(matches!(refused, Err(LlmError::Unsupported(m)) if m.contains("allow_remote")));
    assert!(JsonClient::new("http://api.example.com", None, true, &runtime, limit).is_err());
    let allowed = JsonClient::new("https://api.example.com", None, true, &runtime, limit);
    assert!(allowed.is_ok_and(|c| !c.is_local()));
}
