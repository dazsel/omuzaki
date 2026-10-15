//! The MCP mapping (llm-api D3), over the JSON shapes, with no `rmcp` dependency.
//!
//! [`tool_defs`] turns a `tools/list` result into [`ToolDef`]s; [`tool_result`] turns a `tools/call` result into the
//! blocks of a [`ContentBlock::ToolResult`].

use crate::message::{ContentBlock, ImageMime, ImageOrigin, ToolDef};
use serde::Serialize;
use serde_json::Value;

/// The URI prefix of a render that the MCP server serves.
const RENDER_PREFIX: &str = "omuzaki://render/";
/// The longest tool name the stricter provider (`OpenAI`) accepts.
const MAX_NAME: usize = 64;

/// What a `tools/call` result becomes.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolResult {
    /// The result, `Text` and `Image` blocks only, in the server's order.
    pub blocks: Vec<ContentBlock>,
    /// The tool failed (`isError`).
    pub is_error: bool,
}

impl ToolResult {
    /// The block that answers the call `id`.
    pub fn into_block(self, id: impl Into<String>) -> ContentBlock {
        if self.is_error {
            ContentBlock::tool_error(id, self.blocks)
        } else {
            ContentBlock::tool_result(id, self.blocks)
        }
    }
}

/// True if `name` matches `^[a-zA-Z0-9_-]{1,64}$`.
fn safe_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The tools in a `tools/list` result: its `tools` array, or the array itself.
fn listed(list: &Value) -> &[Value] {
    list.get("tools").unwrap_or(list).as_array().map_or(&[], Vec::as_slice)
}

/// The tools of a `tools/list` result as [`ToolDef`]s, in the server's order.
///
/// The name is unchanged. The description is the `title`, a blank line and the `description` (either alone if the
/// other is missing). `inputSchema` is passed on unchanged; `outputSchema` and `annotations` are not sent. A tool
/// whose name is outside `^[a-zA-Z0-9_-]{1,64}$` is left out, not renamed, because a rename would break the call
/// path; [`skipped_tools`] names them so the caller can warn.
pub fn tool_defs(list: &Value) -> Vec<ToolDef> {
    listed(list)
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name")?.as_str().filter(|n| safe_name(n))?;
            let text = |key: &str| tool.get(key).and_then(Value::as_str).filter(|t| !t.is_empty());
            let description = match (text("title"), text("description")) {
                (Some(title), Some(description)) => format!("{title}\n\n{description}"),
                (Some(only), None) | (None, Some(only)) => only.to_owned(),
                (None, None) => String::new(),
            };
            let schema = tool.get("inputSchema").cloned().unwrap_or_else(|| serde_json::json!({"type": "object"}));
            Some(ToolDef::new(name, description, schema))
        })
        .collect()
}

/// The names (or `<unnamed>`) of the tools [`tool_defs`] leaves out, for the caller's warning.
pub fn skipped_tools(list: &Value) -> Vec<String> {
    listed(list)
        .iter()
        .filter_map(|tool| match tool.get("name").and_then(Value::as_str) {
            Some(name) if safe_name(name) => None,
            Some(name) => Some(name.to_owned()),
            None => Some("<unnamed>".to_owned()),
        })
        .collect()
}

/// The `tools/call` result `result` as blocks (`isError` becomes `is_error`).
///
/// Each `content` item, in order: `text` is `Text`; `image` is `Image` (base64 decoded; origin `Render` for an
/// `omuzaki://render/...` `uri`, otherwise `File`); a `resource_link` is `Text` of its URI; an embedded text resource
/// is `Text`; an embedded image blob is `Image`; audio and any other blob are `Text("[audio omitted]")`, because audio
/// never leaves through a model (SE-04). When `content` is empty and `structuredContent` is present, it is sent as one
/// compact JSON `Text`.
pub fn tool_result(result: &Value) -> ToolResult {
    let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
    let items = result.get("content").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let mut blocks: Vec<ContentBlock> = items.iter().map(content_block).collect();
    if blocks.is_empty()
        && let Some(structured) = result.get("structuredContent")
    {
        blocks.push(ContentBlock::text(structured.to_string()));
    }
    ToolResult { blocks, is_error }
}

fn content_block(item: &Value) -> ContentBlock {
    let field = |key: &str| item.get(key).and_then(Value::as_str);
    match field("type") {
        Some("text") => ContentBlock::text(field("text").unwrap_or_default()),
        Some("image") => image(field("data"), field("mimeType"), field("uri")),
        Some("resource_link") => ContentBlock::text(field("uri").unwrap_or_default()),
        Some("resource") => resource(item.get("resource").unwrap_or(&Value::Null)),
        Some("audio") => ContentBlock::text(AUDIO_OMITTED),
        _ => ContentBlock::text("[unsupported content omitted]"),
    }
}

const AUDIO_OMITTED: &str = "[audio omitted]";

fn resource(resource: &Value) -> ContentBlock {
    let field = |key: &str| resource.get(key).and_then(Value::as_str);
    if let Some(text) = field("text") {
        return ContentBlock::text(text);
    }
    match (field("blob"), field("mimeType")) {
        (Some(blob), Some(mime)) if mime.starts_with("image/") => image(Some(blob), Some(mime), field("uri")),
        _ => ContentBlock::text(AUDIO_OMITTED),
    }
}

fn image(data: Option<&str>, mime: Option<&str>, uri: Option<&str>) -> ContentBlock {
    let mime = match mime {
        Some("image/png") => ImageMime::Png,
        Some("image/jpeg") => ImageMime::Jpeg,
        Some("image/webp") => ImageMime::Webp,
        Some("image/gif") => ImageMime::Gif,
        _ => return ContentBlock::text("[image omitted: unsupported type]"),
    };
    let Some(bytes) = data.and_then(base64_decode) else {
        return ContentBlock::text("[image omitted: invalid base64]");
    };
    let origin =
        if uri.is_some_and(|u| u.starts_with(RENDER_PREFIX)) { ImageOrigin::Render } else { ImageOrigin::File };
    ContentBlock::image(mime, bytes, origin)
}

/// Standard base64 (RFC 4648 §4), padded or not, ignoring ASCII whitespace; `None` if it is not base64. Written here
/// because the API crate takes no codec dependency for the one place it decodes.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits, mut padding, mut chars) = (0u32, 0u32, 0u32, 0u32);
    for byte in text.bytes().filter(|b| !b.is_ascii_whitespace()) {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding += 1;
                continue;
            }
            _ => return None,
        };
        if padding > 0 {
            return None;
        }
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        chars += 1;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    // RFC 4648: a lone character is not a byte; padding must finish the quantum; the unused low bits must be zero.
    let whole = chars % 4 != 1 && padding <= 2 && (padding == 0 || (chars + padding) % 4 == 0);
    let clean = acc & ((1u32 << bits) - 1) == 0;
    (whole && clean).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_every_padding_length() {
        assert_eq!(base64_decode("").as_deref(), Some(&b""[..]));
        assert_eq!(base64_decode("Zg==").as_deref(), Some(&b"f"[..]));
        assert_eq!(base64_decode("Zg").as_deref(), Some(&b"f"[..]));
        assert_eq!(base64_decode("Zm8=").as_deref(), Some(&b"fo"[..]));
        assert_eq!(base64_decode("Zm9v\nYmFy").as_deref(), Some(&b"foobar"[..]));
        assert_eq!(base64_decode("Zm9v!"), None);
        assert_eq!(base64_decode("Zg==Zg"), None);
    }

    #[test]
    fn base64_rejects_what_rfc_4648_does_not_allow() {
        assert_eq!(base64_decode("Z"), None, "one character is not a byte");
        assert_eq!(base64_decode("Zh"), None, "non-zero trailing bits");
        assert_eq!(base64_decode("Zm9="), None, "non-zero trailing bits before padding");
        assert_eq!(base64_decode("Zm9v="), None, "padding after a full quantum");
        assert_eq!(base64_decode("Zg="), None, "padding that does not finish the quantum");
        assert_eq!(base64_decode("Zm8").as_deref(), Some(&b"fo"[..]));
    }

    #[test]
    fn a_tool_with_neither_title_nor_description_keeps_an_empty_description() {
        let list = serde_json::json!({"tools": [{"name": "a"}]});
        let defs = tool_defs(&list);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].description, "");
        assert_eq!(defs[0].input_schema, serde_json::json!({"type": "object"}));
    }

    #[test]
    fn skipped_tools_are_named() {
        let list = serde_json::json!([{"name": "ok"}, {"name": "bad name"}, {"title": "x"}]);
        assert_eq!(skipped_tools(&list), vec!["bad name".to_owned(), "<unnamed>".to_owned()]);
    }
}
