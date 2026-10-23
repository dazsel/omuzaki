//! Folds the events of one streamed turn into the blocks of its assistant message. The seam's own fold is private and
//! refuses a stream that was pulled event by event, which the host does so it can forward text as it arrives.

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::ContentBlock;
use omuzaki_llm_api::stream::{LlmEvent, StopReason};

/// An open tool call: id, name and the raw argument text so far.
struct OpenCall {
    id: String,
    name: String,
    json: String,
}

/// The turn so far.
#[derive(Default)]
pub(crate) struct TurnFold {
    pub model: String,
    blocks: Vec<ContentBlock>,
    text: String,
    call: Option<OpenCall>,
    pub stop: Option<StopReason>,
}

impl TurnFold {
    fn flush_text(&mut self) {
        if !self.text.is_empty() {
            self.blocks.push(ContentBlock::Text(std::mem::take(&mut self.text)));
        }
    }

    /// Takes one event. Text deltas are forwarded by the caller; this only keeps them for the final blocks.
    pub(crate) fn push(&mut self, event: LlmEvent) -> Result<(), LlmError> {
        match event {
            LlmEvent::Start { model, .. } => self.model = model,
            LlmEvent::TextDelta(t) => self.text.push_str(&t),
            LlmEvent::ToolUseStart { id, name, .. } => {
                if let Some(open) = &self.call {
                    return Err(LlmError::Malformed(format!("tool call {id} starts inside open call {}", open.id)));
                }
                self.flush_text();
                self.call = Some(OpenCall { id, name, json: String::new() });
            }
            LlmEvent::ToolUseInput(fragment) => match &mut self.call {
                Some(call) => call.json.push_str(&fragment),
                None => return Err(LlmError::Malformed("tool input outside a tool call".into())),
            },
            LlmEvent::ToolUseEnd { id, .. } => {
                let call = self.call.take().filter(|c| c.id == id);
                let Some(call) = call else {
                    return Err(LlmError::Malformed(format!("tool call end for unopened id {id}")));
                };
                let source = if call.json.trim().is_empty() { "{}" } else { call.json.as_str() };
                let input = serde_json::from_str(source)
                    .map_err(|e| LlmError::Malformed(format!("tool call {id} has invalid JSON arguments: {e}")))?;
                self.blocks.push(ContentBlock::tool_use(call.id, call.name, input));
            }
            LlmEvent::Opaque { provider, block, .. } => {
                self.flush_text();
                self.blocks.push(ContentBlock::opaque(provider, block));
            }
            LlmEvent::Stop(reason) => self.stop = Some(reason),
            // Retries are the client's business and usage is read from the stream.
            _ => {}
        }
        Ok(())
    }

    /// The blocks of the finished turn.
    pub(crate) fn finish(mut self) -> Result<(Vec<ContentBlock>, StopReason), LlmError> {
        self.flush_text();
        if self.call.is_some() {
            return Err(LlmError::Malformed("the stream ended inside a tool call".into()));
        }
        let stop = self.stop.ok_or_else(|| LlmError::Malformed("the stream ended without a stop".into()))?;
        Ok((self.blocks, stop))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn text_and_a_call_fold_into_blocks_in_order() {
        let mut fold = TurnFold::default();
        for e in [
            LlmEvent::start("m"),
            LlmEvent::TextDelta("Let me ".into()),
            LlmEvent::TextDelta("look.".into()),
            LlmEvent::tool_use_start("c1", "render_preview"),
            LlmEvent::ToolUseInput("{\"view\":".into()),
            LlmEvent::ToolUseInput("\"front\"}".into()),
            LlmEvent::tool_use_end("c1"),
            LlmEvent::Stop(StopReason::ToolUse),
        ] {
            fold.push(e).unwrap();
        }
        let (blocks, stop) = fold.finish().unwrap();
        assert_eq!(stop, StopReason::ToolUse);
        assert_eq!(
            blocks,
            vec![
                ContentBlock::text("Let me look."),
                ContentBlock::tool_use("c1", "render_preview", json!({"view": "front"}))
            ]
        );
    }

    #[test]
    fn a_stream_without_a_stop_is_malformed() {
        let mut fold = TurnFold::default();
        fold.push(LlmEvent::TextDelta("x".into())).unwrap();
        assert!(matches!(fold.finish(), Err(LlmError::Malformed(_))));
    }
}
