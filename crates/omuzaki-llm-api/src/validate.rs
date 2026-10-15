//! The checks every client makes before it sends anything (llm-api D2, D5). `FakeLlm` runs the same ones, so a test
//! cannot pass a flow that a real client rejects.

use crate::error::LlmError;
use crate::image::{self, MAX_IMAGE_BYTES, MAX_IMAGE_SIDE};
use crate::message::{ContentBlock, ToolChoice, TurnRequest};
use crate::usage::ModelInfo;

/// The most cache breakpoints Anthropic accepts in one request; the conversation's own counts as one.
pub const MAX_CACHE_BREAKPOINTS: usize = 4;
/// The longest tool name (the stricter OpenAI limit).
pub const MAX_TOOL_NAME: usize = 64;

fn safe_name(name: &str) -> bool {
    (1..=MAX_TOOL_NAME).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Checks `req` against the contract and against what `info` says the model can do.
///
/// # Errors
/// `Malformed` for a request the contract forbids (no messages, a zero `max_output`, a forced tool that is not
/// offered, a bad or repeated tool name, a non-text, non-image block in a tool result). `Unsupported` for a request
/// this model cannot take (`max_output` above its limit, tools or images it does not accept, an image over 768 px on
/// its long side or over 5 MB, more cache breakpoints than the provider allows): never silently changed.
pub fn request(req: &TurnRequest, info: &ModelInfo) -> Result<(), LlmError> {
    if req.messages.is_empty() {
        return Err(LlmError::Malformed("the request has no messages".into()));
    }
    if req.max_output == 0 {
        return Err(LlmError::Malformed("max_output is 0".into()));
    }
    if u64::from(req.max_output) > info.max_output {
        return Err(LlmError::Unsupported(format!(
            "max_output {} is above the {} tokens {} can produce",
            req.max_output, info.max_output, info.id
        )));
    }
    tools(req, info)?;
    let breakpoints = req.system.iter().filter(|s| s.cache_after).count() + usize::from(req.cache.conversation);
    if info.caching && breakpoints > MAX_CACHE_BREAKPOINTS {
        return Err(LlmError::Unsupported(format!("{breakpoints} cache breakpoints; at most {MAX_CACHE_BREAKPOINTS}")));
    }
    for message in &req.messages {
        for block in &message.content {
            content(block, info)?;
        }
    }
    Ok(())
}

fn tools(req: &TurnRequest, info: &ModelInfo) -> Result<(), LlmError> {
    if !req.tools.is_empty() && !info.tools {
        return Err(LlmError::Unsupported(format!("{} does not accept tools", info.id)));
    }
    let mut seen = std::collections::BTreeSet::new();
    for tool in &req.tools {
        if !safe_name(&tool.name) {
            return Err(LlmError::Malformed(format!(
                "the tool name {:?} is outside ^[a-zA-Z0-9_-]{{1,{MAX_TOOL_NAME}}}$",
                tool.name
            )));
        }
        if !seen.insert(tool.name.as_str()) {
            return Err(LlmError::Malformed(format!("the tool {} is offered twice", tool.name)));
        }
    }
    match &req.tool_choice {
        ToolChoice::Any | ToolChoice::Tool(_) if req.tools.is_empty() => {
            Err(LlmError::Malformed("a tool is required but none is offered".into()))
        }
        ToolChoice::Tool(name) if !seen.contains(name.as_str()) => {
            Err(LlmError::Malformed(format!("the required tool {name} is not offered")))
        }
        _ => Ok(()),
    }
}

fn content(block: &ContentBlock, info: &ModelInfo) -> Result<(), LlmError> {
    match block {
        ContentBlock::Image { mime, bytes, .. } => {
            if !info.vision {
                return Err(LlmError::Unsupported(format!("{} does not accept images", info.id)));
            }
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(LlmError::Unsupported(format!(
                    "an image of {} bytes is over the {MAX_IMAGE_BYTES}-byte limit",
                    bytes.len()
                )));
            }
            if let Some((w, h)) = image::size(*mime, bytes)
                && w.max(h) > MAX_IMAGE_SIDE
            {
                return Err(LlmError::Unsupported(format!(
                    "an image of {w} x {h} px is over {MAX_IMAGE_SIDE} px on its long side"
                )));
            }
            Ok(())
        }
        ContentBlock::ToolResult { blocks, .. } => {
            for inner in blocks {
                if !matches!(inner, ContentBlock::Text(_) | ContentBlock::Image { .. }) {
                    return Err(LlmError::Malformed("a tool result holds only text and images".into()));
                }
                content(inner, info)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{CachePlan, ImageMime, ImageOrigin, Message, Role, SystemBlock, ToolDef};
    use crate::usage::ImageCost;

    fn info() -> ModelInfo {
        ModelInfo::new("m", 1000, 100, ImageCost::tiles(28)).with_vision(true).with_tools(true).with_caching(true)
    }

    fn user(blocks: Vec<ContentBlock>) -> Vec<Message> {
        vec![Message::new(Role::User, blocks)]
    }

    fn text_request() -> TurnRequest {
        TurnRequest::new(user(vec![ContentBlock::text("hi")]), 10)
    }

    fn png(w: u32, h: u32) -> ContentBlock {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        ContentBlock::image(ImageMime::Png, b, ImageOrigin::Render)
    }

    fn tool(name: &str) -> ToolDef {
        ToolDef::new(name, "d", serde_json::json!({}))
    }

    #[test]
    fn a_plain_request_passes() {
        assert_eq!(request(&text_request(), &info()), Ok(()));
    }

    #[test]
    fn output_limits_and_empty_requests_are_refused() {
        assert!(matches!(request(&TurnRequest::new(Vec::new(), 10), &info()), Err(LlmError::Malformed(_))));
        let mut zero = text_request();
        zero.max_output = 0;
        assert!(matches!(request(&zero, &info()), Err(LlmError::Malformed(_))));
        let mut big = text_request();
        big.max_output = 101;
        assert!(matches!(request(&big, &info()), Err(LlmError::Unsupported(m)) if m.contains("101")));
    }

    #[test]
    fn images_over_the_limits_or_for_a_blind_model_are_unsupported() {
        let with = |block| TurnRequest::new(user(vec![block]), 10);
        assert_eq!(request(&with(png(768, 500)), &info()), Ok(()));
        assert!(matches!(request(&with(png(769, 10)), &info()), Err(LlmError::Unsupported(_))));
        assert!(matches!(request(&with(png(10, 2000)), &info()), Err(LlmError::Unsupported(_))));
        let huge = ContentBlock::image(ImageMime::Png, vec![0; MAX_IMAGE_BYTES + 1], ImageOrigin::File);
        assert!(matches!(request(&with(huge), &info()), Err(LlmError::Unsupported(_))));
        let blind = info().with_vision(false);
        assert!(matches!(request(&with(png(10, 10)), &blind), Err(LlmError::Unsupported(_))));
        // The same rule inside a tool result.
        let nested = ContentBlock::tool_result("t", vec![png(900, 10)]);
        assert!(matches!(request(&with(nested), &info()), Err(LlmError::Unsupported(_))));
    }

    #[test]
    fn tools_and_forced_choices_are_checked() {
        let offered = text_request().with_tools(vec![tool("a"), tool("b")]);
        assert_eq!(request(&offered.clone().with_tool_choice(ToolChoice::Tool("b".into())), &info()), Ok(()));
        let missing = offered.clone().with_tool_choice(ToolChoice::Tool("c".into()));
        assert!(matches!(request(&missing, &info()), Err(LlmError::Malformed(_))));
        let none_offered = text_request().with_tool_choice(ToolChoice::Any);
        assert!(matches!(request(&none_offered, &info()), Err(LlmError::Malformed(_))));
        let twice = text_request().with_tools(vec![tool("a"), tool("a")]);
        assert!(matches!(request(&twice, &info()), Err(LlmError::Malformed(_))));
        let bad = text_request().with_tools(vec![tool("has space")]);
        assert!(matches!(request(&bad, &info()), Err(LlmError::Malformed(_))));
        assert!(matches!(request(&offered, &info().with_tools(false)), Err(LlmError::Unsupported(_))));
    }

    #[test]
    fn too_many_cache_breakpoints_are_unsupported_only_where_caching_exists() {
        let blocks: Vec<SystemBlock> =
            (0..4).map(|i| SystemBlock::new(format!("s{i}")).with_cache_after(true)).collect();
        let req = text_request().with_system(blocks).with_cache(CachePlan::new(true));
        assert!(matches!(request(&req, &info()), Err(LlmError::Unsupported(_))));
        assert_eq!(request(&req, &info().with_caching(false)), Ok(()));
    }

    #[test]
    fn a_tool_result_holds_only_text_and_images() {
        let nested = ContentBlock::tool_result("t", vec![ContentBlock::tool_use("x", "n", serde_json::json!({}))]);
        let req = TurnRequest::new(user(vec![nested]), 10);
        assert!(matches!(request(&req, &info()), Err(LlmError::Malformed(_))));
    }
}
