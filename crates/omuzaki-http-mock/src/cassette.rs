//! Hand-written cassettes: the JSON that loads a [`FakeHttp`](crate::FakeHttp) with routes and replies.
//!
//! Cassettes are written by hand from a vendor's public API documentation, never recorded, and they hold no keys,
//! accounts or private material. Every problem is reported with the index of the route it is in.

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

/// Decodes standard base64 (RFC 4648 alphabet, `=` padding, length a multiple of four). Written here so the crate
/// needs no dependency for it.
pub(crate) fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(format!("its length {} is not a multiple of 4", bytes.len()));
    }
    let (blocks, _) = bytes.as_chunks::<4>();
    let mut out = Vec::with_capacity(blocks.len() * 3);
    for (block_index, block) in blocks.iter().enumerate() {
        let padding = block.iter().rev().take_while(|&&b| b == b'=').count();
        let is_last = block_index + 1 == blocks.len();
        if padding > 2 || (padding > 0 && !is_last) {
            return Err("'=' may only pad the final four characters, at most twice".to_owned());
        }
        // The data characters pack into one 24-bit group. Padding contributes zero bits, so the group is shifted up as
        // if the missing characters were there, and its top byte is dropped below.
        let data_len = 4 - padding;
        let mut group = 0_u32;
        for &c in &block[..data_len] {
            let value = sextet(c).ok_or_else(|| format!("{:?} is not a base64 character", char::from(c)))?;
            group = (group << 6) | value;
        }
        for _ in 0..padding {
            group <<= 6;
        }
        // The group sits in the low three bytes; the byte before them is zero, and so are the bytes padding left.
        out.extend_from_slice(&group.to_be_bytes()[1..data_len]);
    }
    Ok(out)
}

/// The 6-bit value of one base64 character, or `None` for a character outside the alphabet.
fn sextet(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some(u32::from(c - b'A')),
        b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::decode_base64;

    #[test]
    fn decodes_the_rfc_4648_vectors() {
        let cases = [
            ("", ""),
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("Zm9vYg==", "foob"),
            ("Zm9vYmE=", "fooba"),
            ("Zm9vYmFy", "foobar"),
            ("AAECAw==", "\u{0}\u{1}\u{2}\u{3}"),
        ];
        for (text, expected) in cases {
            let decoded = decode_base64(text).unwrap_or_else(|why| panic!("{text:?} should decode: {why}"));
            assert_eq!(decoded, expected.as_bytes(), "{text:?}");
        }
    }

    #[test]
    fn rejects_bad_base64() {
        for text in ["Zm9", "Zm9*", "Z===", "Zg=x", "Zg==Zg=="] {
            assert!(decode_base64(text).is_err(), "{text:?} should be rejected");
        }
    }
}
