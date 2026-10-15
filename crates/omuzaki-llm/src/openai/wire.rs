//! The OpenAI-compatible chat request body (llm-api D2, D6): Ollama, llama.cpp `llama-server`, LM Studio, vLLM.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use omuzaki_llm_api::message::{ContentBlock, Effort, ImageMime, Message, Role, ToolChoice, TurnRequest};
use serde_json::{Value, json};

fn mime_text(mime: ImageMime) -> &'static str {
    match mime {
        ImageMime::Png => "image/png",
        ImageMime::Jpeg => "image/jpeg",
        ImageMime::Webp => "image/webp",
        ImageMime::Gif => "image/gif",
        // A format added to the contract later: the server refuses it by name rather than guessing a type.
        _ => "application/octet-stream",
    }
}

/// An image as a content part; images travel as data URLs (llm-api D2).
pub(crate) fn image_part(mime: ImageMime, bytes: &[u8]) -> Value {
    json!({"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", mime_text(mime), STANDARD.encode(bytes))}})
}

/// A user message's blocks as content parts; text and images only.
fn user_parts(blocks: &[ContentBlock]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) if !t.is_empty() => Some(json!({"type": "text", "text": t})),
            ContentBlock::Image { mime, bytes, .. } => Some(image_part(*mime, bytes)),
            _ => None,
        })
        .collect()
}

fn joined_text(blocks: &[ContentBlock]) -> String {
    let texts: Vec<&str> =
        blocks.iter().filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None }).collect();
    texts.join("\n")
}

/// The message list: the system prompt first, then the history. A tool result becomes a `tool` message of its text;
/// its images, which a `tool` message cannot hold, follow in a user message that names the call.
fn messages(req: &TurnRequest) -> Vec<Value> {
    let mut out = Vec::new();
    if !req.system.is_empty() {
        let text: Vec<&str> = req.system.iter().map(|s| s.text.as_str()).collect();
        out.push(json!({"role": "system", "content": text.join("\n\n")}));
    }
    for message in &req.messages {
        match message.role {
            Role::User => user_message(message, &mut out),
            _ => assistant_message(message, &mut out),
        }
    }
    out
}

fn user_message(message: &Message, out: &mut Vec<Value>) {
    let mut parts = Vec::new();
    for block in &message.content {
        if let ContentBlock::ToolResult { id, blocks, is_error, .. } = block {
            // Tool results come first in their message, in the order the model made the calls.
            let text = joined_text(blocks);
            let text = match (*is_error, text.is_empty()) {
                (true, _) => format!("Error: {text}"),
                (false, true) => "(no text)".to_owned(),
                (false, false) => text,
            };
            out.push(json!({"role": "tool", "tool_call_id": id, "content": text}));
            let images = user_parts(blocks).into_iter().filter(|p| p["type"] == "image_url");
            let mut with_images = vec![json!({"type": "text", "text": format!("Images from the result of {id}:")})];
            with_images.extend(images);
            if with_images.len() > 1 {
                out.push(json!({"role": "user", "content": with_images}));
            }
        } else {
            parts.extend(user_parts(std::slice::from_ref(block)));
        }
    }
    if !parts.is_empty() {
        out.push(json!({"role": "user", "content": parts}));
    }
}

fn assistant_message(message: &Message, out: &mut Vec<Value>) {
    let text = joined_text(&message.content);
    let calls: Vec<Value> = message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input, .. } => {
                Some(json!({"id": id, "type": "function", "function": {"name": name, "arguments": input.to_string()}}))
            }
            _ => None,
        })
        .collect();
    // Another provider's opaque blocks (thinking, signatures) are not sent here.
    if text.is_empty() && calls.is_empty() {
        return;
    }
    let mut value =
        json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { Value::String(text) }});
    if !calls.is_empty() {
        value["tool_calls"] = Value::Array(calls);
    }
    out.push(value);
}

fn tools(req: &TurnRequest) -> Vec<Value> {
    let mut sorted: Vec<_> = req.tools.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    sorted
        .into_iter()
        .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}}))
        .collect()
}

fn tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Any => json!("required"),
        ToolChoice::Tool(name) => json!({"type": "function", "function": {"name": name}}),
        ToolChoice::None => json!("none"),
        _ => json!("auto"),
    }
}

/// `reasoning_effort`: `OpenAI` has no level above `high`, so `Max` is `high`.
fn effort(effort: Effort) -> Option<&'static str> {
    match effort {
        Effort::Low => Some("low"),
        Effort::Medium => Some("medium"),
        Effort::High | Effort::Max => Some("high"),
        _ => None,
    }
}

/// The JSON body of a streaming chat request for `model`. Cache breakpoints are ignored: these servers cache
/// prefixes themselves or not at all.
pub(crate) fn body(req: &TurnRequest, model: &str, honours_effort: bool) -> Vec<u8> {
    let mut body = json!({
        "model": model,
        "messages": messages(req),
        "max_tokens": req.max_output,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(tools(req));
        body["tool_choice"] = tool_choice(&req.tool_choice);
    }
    if let Some(level) = effort(req.effort).filter(|_| honours_effort) {
        body["reasoning_effort"] = json!(level);
    }
    body.to_string().into_bytes()
}

/// A probe request: one token, with the trivial `tool` (when `with_tool`) or a 1×1 PNG (when `with_image`).
pub(crate) fn probe_body(model: &str, with_tool: bool, with_image: bool) -> Vec<u8> {
    let content: Value = if with_image {
        json!([{"type": "text", "text": "hi"}, image_part(ImageMime::Png, PIXEL)])
    } else {
        json!("hi")
    };
    let mut body =
        json!({"model": model, "messages": [{"role": "user", "content": content}], "max_tokens": 1, "stream": false});
    if with_tool {
        body["tools"] = json!([{"type": "function", "function": {
            "name": "noop", "description": "does nothing", "parameters": {"type": "object", "properties": {}},
        }}]);
    }
    body.to_string().into_bytes()
}

/// A valid 1×1 PNG.
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49,
    0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f, 0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba,
    0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_llm_api::message::{ImageOrigin, SystemBlock, ToolDef};

    fn parsed(req: &TurnRequest) -> Value {
        serde_json::from_slice(&body(req, "llama3.2", true)).unwrap_or_else(|e| panic!("{e}"))
    }

    fn hi() -> Vec<Message> {
        vec![Message::new(Role::User, vec![ContentBlock::text("hi")])]
    }

    #[test]
    fn a_plain_request_streams_with_usage_and_a_system_message() {
        let req = TurnRequest::new(hi(), 64)
            .with_system(vec![SystemBlock::new("a"), SystemBlock::new("b").with_cache_after(true)]);
        assert_eq!(
            parsed(&req),
            json!({"model": "llama3.2", "max_tokens": 64, "stream": true, "stream_options": {"include_usage": true},
                   "messages": [{"role": "system", "content": "a\n\nb"},
                                {"role": "user", "content": [{"type": "text", "text": "hi"}]}]})
        );
    }

    #[test]
    fn tools_choice_and_effort_follow_the_openai_names() {
        let req = TurnRequest::new(hi(), 8)
            .with_tools(vec![ToolDef::new("b", "B", json!({"type": "object"})), ToolDef::new("a", "A", json!({}))])
            .with_tool_choice(ToolChoice::Tool("a".into()))
            .with_effort(Effort::Max);
        let v = parsed(&req);
        assert_eq!(v["tools"][0]["function"]["name"], "a");
        assert_eq!(v["tools"][1]["function"]["parameters"], json!({"type": "object"}));
        assert_eq!(v["tool_choice"], json!({"type": "function", "function": {"name": "a"}}));
        assert_eq!(v["reasoning_effort"], "high");
        let any = parsed(
            &TurnRequest::new(hi(), 8)
                .with_tools(vec![ToolDef::new("a", "A", json!({}))])
                .with_tool_choice(ToolChoice::Any),
        );
        assert_eq!(any["tool_choice"], "required");
        assert!(parsed(&TurnRequest::new(hi(), 8)).get("reasoning_effort").is_none());
        let off: Value = serde_json::from_slice(&body(&req, "m", false)).unwrap_or_else(|e| panic!("{e}"));
        assert!(off.get("reasoning_effort").is_none());
    }

    #[test]
    fn images_are_data_urls_and_a_tool_result_becomes_a_tool_message_then_its_images() {
        let call = ContentBlock::tool_use("call_1", "render", json!({"size": 512}));
        let result = ContentBlock::tool_result(
            "call_1",
            vec![
                ContentBlock::text("Rendered."),
                ContentBlock::image(ImageMime::Png, vec![1, 2, 3], ImageOrigin::Render),
            ],
        );
        let history = vec![
            Message::new(
                Role::User,
                vec![ContentBlock::text("go"), ContentBlock::image(ImageMime::Jpeg, vec![9], ImageOrigin::File)],
            ),
            Message::new(
                Role::Assistant,
                vec![ContentBlock::text("ok"), call, ContentBlock::opaque("anthropic", json!({}))],
            ),
            Message::new(Role::User, vec![result]),
        ];
        let v = parsed(&TurnRequest::new(history, 8));
        let m = &v["messages"];
        assert_eq!(m[0]["content"][1]["image_url"]["url"], "data:image/jpeg;base64,CQ==");
        assert_eq!(m[1]["tool_calls"][0]["function"], json!({"name": "render", "arguments": "{\"size\":512}"}));
        assert_eq!(m[1]["content"], "ok");
        assert_eq!(m[2], json!({"role": "tool", "tool_call_id": "call_1", "content": "Rendered."}));
        assert_eq!(m[3]["content"][1]["image_url"]["url"], "data:image/png;base64,AQID");
        assert_eq!(m.as_array().map(Vec::len), Some(4));
    }

    #[test]
    fn an_error_result_is_marked_and_an_empty_one_is_not_empty() {
        let error = ContentBlock::tool_error("c", vec![ContentBlock::text("boom")]);
        let empty = ContentBlock::tool_result("d", Vec::new());
        let history = vec![Message::new(Role::User, vec![error, empty])];
        let v = parsed(&TurnRequest::new(history, 8));
        assert_eq!(v["messages"][0]["content"], "Error: boom");
        assert_eq!(v["messages"][1]["content"], "(no text)");
    }

    #[test]
    fn an_assistant_message_of_only_foreign_blocks_is_left_out() {
        let history = vec![
            Message::new(Role::User, vec![ContentBlock::text("a")]),
            Message::new(Role::Assistant, vec![ContentBlock::opaque("anthropic", json!({}))]),
            Message::new(Role::User, vec![ContentBlock::text("b")]),
        ];
        assert_eq!(parsed(&TurnRequest::new(history, 8))["messages"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn the_probe_bodies_ask_for_one_token_with_a_tool_or_a_pixel() {
        let tool: Value = serde_json::from_slice(&probe_body("m", true, false)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            (tool["max_tokens"].clone(), tool["tools"][0]["function"]["name"].clone()),
            (json!(1), json!("noop"))
        );
        let image: Value = serde_json::from_slice(&probe_body("m", false, true)).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            image["messages"][0]["content"][1]["image_url"]["url"]
                .as_str()
                .is_some_and(|u| u.starts_with("data:image/png;base64,"))
        );
        assert_eq!(omuzaki_llm_api::image::size(ImageMime::Png, PIXEL), Some((1, 1)));
    }
}
