//! Hand-written cassettes: the JSON that loads a [`FakeHttp`](crate::FakeHttp) with routes and replies.
//!
//! Cassettes are written by hand from a vendor's public API documentation, never recorded, and they hold no keys,
//! accounts or private material. Every problem is reported with the index of the route it is in.

use base64::alphabet;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::{DecodeError, Engine as _};
use omuzaki_http_api::{HttpResponse, Method};
use serde_json::{Map, Value};

use crate::fake::{Reply, Route};
use crate::stream::{Chunk, StreamScript};

/// Why a cassette did not load.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CassetteError {
    /// The text is not JSON, or not a document with a `routes` array.
    #[error("the cassette could not be read: {0}")]
    Json(String),
    /// A route is malformed. The message names its position and says what is wrong with it.
    #[error("cassette route {index} is malformed: {problem}")]
    Route {
        /// The route's position in the `routes` array, counting from 0.
        index: usize,
        /// What is wrong with it.
        problem: String,
    },
}

/// The keys that carry a reply's body. A reply uses at most one.
const BODY_KEYS: [&str; 4] = ["body_json", "body_text", "body_base64", "chunks"];

/// Parses a cassette document into its routes, in document order.
pub(crate) fn parse(json: &str) -> Result<Vec<Route>, CassetteError> {
    let document: Value = serde_json::from_str(json).map_err(|e| CassetteError::Json(e.to_string()))?;
    let routes = document
        .get("routes")
        .and_then(Value::as_array)
        .ok_or_else(|| CassetteError::Json("expected an object with a \"routes\" array".to_owned()))?;
    routes
        .iter()
        .enumerate()
        .map(|(index, value)| parse_route(value).map_err(|problem| CassetteError::Route { index, problem }))
        .collect()
}

fn parse_route(value: &Value) -> Result<Route, String> {
    let object = value.as_object().ok_or("a route must be an object")?;
    let method = parse_method(text_field(object, "method")?)?;
    let url = text_field(object, "url")?.to_owned();
    let body_contains = match object.get("body_contains") {
        None => None,
        Some(value) => Some(text_value(value, "\"body_contains\"")?.to_owned()),
    };
    let replies = object.get("replies").and_then(Value::as_array).ok_or("\"replies\" must be an array of replies")?;
    if replies.is_empty() {
        return Err("\"replies\" must hold at least one reply".to_owned());
    }
    let replies = replies
        .iter()
        .enumerate()
        .map(|(index, value)| parse_reply(value).map_err(|problem| format!("reply {index}: {problem}")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Route::new(method, url, body_contains, replies))
}

fn parse_method(name: &str) -> Result<Method, String> {
    match name {
        "GET" => Ok(Method::Get),
        "POST" => Ok(Method::Post),
        "PUT" => Ok(Method::Put),
        "PATCH" => Ok(Method::Patch),
        "DELETE" => Ok(Method::Delete),
        _ => Err(format!("unknown method \"{name}\"; use GET, POST, PUT, PATCH or DELETE")),
    }
}

fn parse_reply(value: &Value) -> Result<Reply, String> {
    let object = value.as_object().ok_or("a reply must be an object")?;
    let status = object
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .ok_or("\"status\" must be a number from 0 to 65535")?;
    let headers = parse_headers(object)?;
    let present: Vec<(&str, &Value)> =
        BODY_KEYS.iter().filter_map(|&key| object.get(key).map(|value| (key, value))).collect();
    match present.as_slice() {
        [] => Ok(Reply::Response(HttpResponse { status, headers, body: Vec::new() })),
        [("chunks", value)] => Ok(Reply::Stream(StreamScript { status, headers, chunks: parse_chunks(value)? })),
        [("body_json", value)] => {
            Ok(Reply::Response(HttpResponse { status, headers, body: value.to_string().into_bytes() }))
        }
        [("body_text", value)] => {
            let body = text_value(value, "\"body_text\"")?.as_bytes().to_vec();
            Ok(Reply::Response(HttpResponse { status, headers, body }))
        }
        [("body_base64", value)] => {
            let body = decode_base64(text_value(value, "\"body_base64\"")?)
                .map_err(|why| format!("\"body_base64\" is not valid base64: {why}"))?;
            Ok(Reply::Response(HttpResponse { status, headers, body }))
        }
        _ => {
            let keys: Vec<&str> = present.iter().map(|(key, _)| *key).collect();
            Err(format!(
                "a reply has one body but found {}; use one of body_json, body_text, body_base64 or chunks",
                keys.join(", ")
            ))
        }
    }
}

fn parse_headers(object: &Map<String, Value>) -> Result<Vec<(String, String)>, String> {
    let Some(value) = object.get("headers") else {
        return Ok(Vec::new());
    };
    let headers = value.as_object().ok_or("\"headers\" must be an object of strings")?;
    headers
        .iter()
        .map(|(name, value)| -> Result<(String, String), String> {
            Ok((name.clone(), text_value(value, &format!("header \"{name}\""))?.to_owned()))
        })
        .collect()
}

fn parse_chunks(value: &Value) -> Result<Vec<Chunk>, String> {
    let items = value.as_array().ok_or("\"chunks\" must be an array")?;
    items
        .iter()
        .enumerate()
        .map(|(index, item)| parse_chunk(item).map_err(|problem| format!("chunk {index}: {problem}")))
        .collect()
}

/// One chunk: `"stall"`, or an object holding exactly one of `text` and `base64`.
fn parse_chunk(item: &Value) -> Result<Chunk, String> {
    if item.as_str() == Some("stall") {
        return Ok(Chunk::Stall);
    }
    let object = item.as_object().ok_or("a chunk must be {\"text\": ...}, {\"base64\": ...} or \"stall\"")?;
    match (object.get("text"), object.get("base64"), object.len()) {
        (Some(text), None, 1) => Ok(Chunk::Data(text_value(text, "\"text\"")?.as_bytes().to_vec())),
        (None, Some(base64), 1) => decode_base64(text_value(base64, "\"base64\"")?)
            .map(Chunk::Data)
            .map_err(|why| format!("\"base64\" is not valid base64: {why}")),
        _ => Err("a data chunk holds exactly one of \"text\" or \"base64\"".to_owned()),
    }
}

fn text_field<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    let value = object.get(key).ok_or_else(|| format!("missing \"{key}\""))?;
    text_value(value, &format!("\"{key}\""))
}

fn text_value<'a>(value: &'a Value, what: &str) -> Result<&'a str, String> {
    value.as_str().ok_or_else(|| format!("{what} must be a string"))
}

/// Standard base64 with canonical padding. It also accepts non-zero bits left over after the last character (`"Zh=="`
/// decodes to `f`), as the hand-written decoder it replaced did; the crate's `STANDARD` engine refuses those.
const DECODER: GeneralPurpose =
    GeneralPurpose::new(&alphabet::STANDARD, GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true));

/// Decodes a cassette's base64 text, or says why it is not base64.
pub(crate) fn decode_base64(text: &str) -> Result<Vec<u8>, DecodeError> {
    DECODER.decode(text)
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::Arc;

    use base64::DecodeError;
    use omuzaki_http_api::{HttpBody, HttpRequest, HttpTransport, Method, NeverCancel};

    use super::decode_base64;
    use crate::{CassetteError, FakeHttp};

    /// Loads a one-route cassette whose only reply is `reply`, and returns the body that reply serves.
    fn served_body(reply: &str) -> Vec<u8> {
        let json =
            format!(r#"{{"routes":[{{"method":"GET","url":"https://api.example.com/a","replies":[{reply}]}}]}}"#);
        let fake = FakeHttp::cassette(&json).expect("the cassette parses");
        let request = HttpRequest {
            method: Method::Get,
            url: "https://api.example.com/a".to_owned(),
            headers: vec![],
            body: HttpBody::Empty,
            timeout_s: 5,
            max_response: 1_000,
        };
        let mut body = fake.open(&request, Arc::new(NeverCancel)).expect("opened").body;
        let mut out = Vec::new();
        body.read_to_end(&mut out).expect("the body reads");
        out
    }

    #[test]
    fn a_base64_body_serves_its_bytes() {
        assert_eq!(served_body(r#"{"status":200,"body_base64":"Zm9vYmFy"}"#), b"foobar");
        assert_eq!(served_body(r#"{"status":200,"body_base64":"AAEC/w=="}"#), [0, 1, 2, 255]);
    }

    #[test]
    fn non_zero_trailing_bits_are_still_accepted() {
        assert_eq!(served_body(r#"{"status":200,"body_base64":"Zh=="}"#), b"f");
        assert_eq!(served_body(r#"{"status":200,"chunks":[{"base64":"Zh=="}]}"#), b"f");
    }

    #[test]
    fn bad_base64_is_refused_with_the_crate_error() {
        // The crate's own error values: an invalid symbol names its offset, and a missing pad is `InvalidPadding`.
        assert_eq!(decode_base64("Zm9*"), Err(DecodeError::InvalidByte(3, b'*')));
        assert_eq!(decode_base64("Zm9"), Err(DecodeError::InvalidPadding));
    }

    #[test]
    fn bad_base64_is_refused_with_its_reason() {
        for bad in ["Zm9*", "Zm9", "Z===", "Zg=x", "Zg==Zg=="] {
            let json = format!(
                r#"{{"routes":[{{"method":"GET","url":"https://api.example.com/a","replies":[{{"status":200,"body_base64":"{bad}"}}]}}]}}"#
            );
            match FakeHttp::cassette(&json) {
                Err(CassetteError::Route { index: 0, problem }) => {
                    assert!(problem.contains("\"body_base64\" is not valid base64: "), "{problem:?}");
                }
                other => panic!("{bad:?} should be refused as a route error, got {other:?}"),
            }
        }
    }
}
