//! The Anthropic Messages request body (llm-api D2, D6).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use omuzaki_llm_api::message::{
    ContentBlock, Effort, ImageMime, Message, Role, SystemBlock, ToolChoice, ToolDef, TurnRequest,
};
use serde_json::{Value, json};

/// The provider name an [`Opaque`](ContentBlock::Opaque) block carries when this provider made it.
pub(crate) const PROVIDER: &str = "anthropic";

fn ephemeral() -> Value {
    json!({"type": "ephemeral"})
}

fn media_type(mime: ImageMime) -> &'static str {
    match mime {
        ImageMime::Png => "image/png",
        ImageMime::Jpeg => "image/jpeg",
        ImageMime::Webp => "image/webp",
        ImageMime::Gif => "image/gif",
        // A format added to the contract later: the API refuses it by name rather than guessing a type.
        _ => "application/octet-stream",
    }
}

fn content_block(block: &ContentBlock) -> Option<Value> {
    Some(match block {
        // The API refuses an empty text block.
        ContentBlock::Text(t) if t.is_empty() => return None,
        ContentBlock::Text(t) => json!({"type": "text", "text": t}),
        ContentBlock::Image { mime, bytes, .. } => json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type(*mime), "data": STANDARD.encode(bytes)},
        }),
        ContentBlock::ToolUse { id, name, input, .. } => {
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentBlock::ToolResult { id, blocks, is_error, .. } => {
            let mut result = json!({"type": "tool_result", "tool_use_id": id});
            let content: Vec<Value> = blocks.iter().filter_map(content_block).collect();
            if !content.is_empty() {
                result["content"] = Value::Array(content);
            }
            if *is_error {
                result["is_error"] = Value::Bool(true);
            }
            result
        }
        // Thinking and redacted thinking go back byte for byte, and only to the provider that made them.
        ContentBlock::Opaque { provider, block, .. } if provider == PROVIDER => block.clone(),
        _ => return None,
    })
}

fn message(m: &Message) -> Option<Value> {
    let content: Vec<Value> = m.content.iter().filter_map(content_block).collect();
    if content.is_empty() {
        return None;
    }
    let role = match m.role {
        Role::User => "user",
        _ => "assistant",
    };
    Some(json!({"role": role, "content": content}))
}

fn system(blocks: &[SystemBlock], caches: bool) -> Vec<Value> {
    blocks
        .iter()
        .map(|b| {
            let mut v = json!({"type": "text", "text": b.text});
            if b.cache_after && caches {
                v["cache_control"] = ephemeral();
            }
            v
        })
        .collect()
}

/// Tools sorted by name, so the cache prefix is stable however the caller ordered them.
fn tools(defs: &[ToolDef]) -> Vec<Value> {
    let mut sorted: Vec<&ToolDef> = defs.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    sorted
        .into_iter()
        .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema}))
        .collect()
}

fn tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Any => json!({"type": "any"}),
        ToolChoice::Tool(name) => json!({"type": "tool", "name": name}),
        ToolChoice::None => json!({"type": "none"}),
        _ => json!({"type": "auto"}),
    }
}

/// The `output_config.effort` value of `effort` (verified 2026-10-15 against
/// platform.claude.com/docs/en/build-with-claude/effort); `Default` sends nothing.
fn effort(effort: Effort) -> Option<&'static str> {
    match effort {
        Effort::Low => Some("low"),
        Effort::Medium => Some("medium"),
        Effort::High => Some("high"),
        Effort::Max => Some("max"),
        _ => None,
    }
}

/// The JSON body of a streaming Messages request for `model`. `honours_effort` and `caches` come from the model's
/// capabilities: a model that ignores them is not sent the field.
pub(crate) fn body(req: &TurnRequest, model: &str, honours_effort: bool, caches: bool) -> Vec<u8> {
    let mut body = json!({
        "model": model,
        "max_tokens": req.max_output,
        "stream": true,
        "messages": req.messages.iter().filter_map(message).collect::<Vec<_>>(),
    });
    if !req.system.is_empty() {
        body["system"] = Value::Array(system(&req.system, caches));
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(tools(&req.tools));
        // A choice is only meaningful, and only accepted, with tools to choose from.
        body["tool_choice"] = tool_choice(&req.tool_choice);
    }
    if let Some(level) = effort(req.effort).filter(|_| honours_effort) {
        body["output_config"] = json!({"effort": level});
    }
    if req.cache.conversation && caches {
        // Anthropic's automatic caching: the breakpoint follows the end of the growing conversation.
        body["cache_control"] = ephemeral();
    }
    body.to_string().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_llm_api::message::{CachePlan, ImageOrigin};

    fn msg(role: Role, blocks: Vec<ContentBlock>) -> Message {
        Message::new(role, blocks)
    }

    fn parsed(req: &TurnRequest) -> Value {
        serde_json::from_slice(&body(req, "claude-haiku-5-5", true, true)).unwrap_or_else(|e| panic!("{e}"))
    }

    fn hi() -> Vec<Message> {
        vec![msg(Role::User, vec![ContentBlock::text("hi")])]
    }

    #[test]
    fn a_plain_request_has_the_model_limit_stream_flag_and_messages() {
        assert_eq!(
            parsed(&TurnRequest::new(hi(), 64)),
            json!({"model": "claude-haiku-5-5", "max_tokens": 64, "stream": true,
                   "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]})
        );
    }

    #[test]
    fn system_breakpoints_tools_sorted_and_the_forced_tool_are_sent() {
        let req = TurnRequest::new(hi(), 8)
            .with_system(vec![SystemBlock::new("a").with_cache_after(true), SystemBlock::new("b")])
            .with_tools(vec![
                ToolDef::new("zeta", "z", json!({})),
                ToolDef::new("alpha", "a", json!({"type": "object"})),
            ])
            .with_tool_choice(ToolChoice::Tool("alpha".into()));
        let v = parsed(&req);
        assert_eq!(v["system"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert!(v["system"][1].get("cache_control").is_none());
        assert_eq!(v["tools"][0]["name"], "alpha");
        assert_eq!(v["tools"][1]["name"], "zeta");
        assert_eq!(v["tool_choice"], json!({"type": "tool", "name": "alpha"}));
    }

    #[test]
    fn effort_and_conversation_caching_follow_the_model() {
        let req = TurnRequest::new(hi(), 8).with_effort(Effort::Max).with_cache(CachePlan::new(true));
        let v = parsed(&req);
        assert_eq!(v["output_config"], json!({"effort": "max"}));
        assert_eq!(v["cache_control"], json!({"type": "ephemeral"}));
        let plain: Value = serde_json::from_slice(&body(&req, "m", false, false)).unwrap_or_else(|e| panic!("{e}"));
        assert!(plain.get("output_config").is_none() && plain.get("cache_control").is_none());
        assert!(parsed(&TurnRequest::new(hi(), 8)).get("output_config").is_none(), "Default sends nothing");
    }

    #[test]
    fn tool_results_images_and_own_opaque_blocks_round_trip_and_foreign_ones_are_dropped() {
        let thinking = json!({"type": "thinking", "thinking": "hmm", "signature": "sig=="});
        let assistant = msg(
            Role::Assistant,
            vec![
                ContentBlock::opaque("anthropic", thinking.clone()),
                ContentBlock::opaque("openai", json!({"type": "reasoning"})),
                ContentBlock::tool_use("tu_1", "t", json!({"a": 1})),
            ],
        );
        let result = ContentBlock::tool_error(
            "tu_1",
            vec![ContentBlock::text("boom"), ContentBlock::image(ImageMime::Png, vec![1, 2, 3], ImageOrigin::Render)],
        );
        let history = vec![msg(Role::User, vec![ContentBlock::text("q")]), assistant, msg(Role::User, vec![result])];
        let v = parsed(&TurnRequest::new(history, 8));
        assert_eq!(v["messages"][1]["content"][0], thinking);
        assert_eq!(v["messages"][1]["content"].as_array().map(Vec::len), Some(2), "the foreign block is dropped");
        let tool_result = &v["messages"][2]["content"][0];
        assert_eq!(tool_result["tool_use_id"], "tu_1");
        assert_eq!(tool_result["is_error"], true);
        assert_eq!(
            tool_result["content"][1]["source"],
            json!({"type": "base64", "media_type": "image/png", "data": "AQID"})
        );
    }

    #[test]
    fn empty_text_and_messages_that_end_up_empty_are_left_out() {
        let history = vec![
            msg(Role::User, vec![ContentBlock::text("")]),
            msg(Role::Assistant, vec![ContentBlock::opaque("openai", json!({}))]),
            msg(Role::User, vec![ContentBlock::text("x")]),
        ];
        assert_eq!(parsed(&TurnRequest::new(history, 8))["messages"].as_array().map(Vec::len), Some(1));
    }
}
