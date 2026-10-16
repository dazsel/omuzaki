//! The context block each user message carries before the person's text (D5), built at send time.
//!
//! It holds, when the server has them: the UI context (selection, workspace, viewport render), the project summary
//! (in full the first time, afterwards only when it changed), what others changed since the last answer, results that
//! arrived after a Stop, and what the person attached. Everything the person did not type is wrapped as data.

use omuzaki_agent_api::entry::{AnswerId, Entry};
use omuzaki_agent_api::message::{ContextChip, UserMessage};
use omuzaki_llm_api::message::{ContentBlock, ImageMime, ImageOrigin};

use crate::content::{plain_text, wrap_data};
use crate::history::{Delivery, delivery_of, owed_sections, tool_of};
use crate::link::{ReplyBlock, ToolReply};
use crate::worker::Worker;

/// The resource that describes the person's screen: selection, workspace, focused panel, viewport, notes.
const UI_CONTEXT_URI: &str = "omuzaki://ui/context";
/// The tool that summarises the project.
const DESCRIBE_TOOL: &str = "project_describe";
/// The tool whose result carries the heads-up.
const SESSION_INFO_TOOL: &str = "session_info";

impl Worker {
    /// The blocks of the context for `msg`, the message of `answer`; empty when there is nothing to add. The text is
    /// stored as the answer's `Context` entry before anything it carries counts as told (see `history.rs`).
    pub(crate) fn context_block(&mut self, msg: &UserMessage, answer: AnswerId) -> Vec<ContentBlock> {
        let mut sections: Vec<String> = Vec::new();
        let mut images: Vec<ContentBlock> = Vec::new();

        if let Some(blocks) = self.read_blocks(UI_CONTEXT_URI) {
            for block in blocks {
                match block {
                    ReplyBlock::Text(text) => sections.push(wrap_data(UI_CONTEXT_URI, &text)),
                    ReplyBlock::Image { mime, bytes } => {
                        images.push(ContentBlock::image(mime, bytes, ImageOrigin::Render));
                    }
                    ReplyBlock::Note(_) => {}
                }
            }
        }
        // The summary and `session_info`'s heads-up are given in this context: their heads-ups wait for it.
        let mut deliveries = Vec::new();
        let describe = self.describe_if_new(&mut deliveries);
        if let Some(summary) = &describe {
            sections.push(wrap_data(DESCRIBE_TOOL, summary));
        }
        if let Some(heads_up) = self.heads_up(&mut deliveries) {
            sections.push(wrap_data(SESSION_INFO_TOOL, &heads_up));
        }
        let mut chips = Vec::new();
        for chip in &msg.chips {
            self.chip_section(chip, &mut chips, &mut images);
        }
        for delivery in &deliveries {
            self.hold_for_delivery(delivery);
        }
        // Every other heads-up received so far is owed: stored now, so this context gives it.
        self.settle_pending();

        let told = self.log.accounted();
        let shared = std::sync::Arc::clone(&self.shared);
        let recorded = shared.record_built(|owed, transcript| {
            let (owed_sections, given) = owed_sections(owed, &told, &|id: &str| tool_of(transcript, id));
            sections.extend(owed_sections);
            sections.extend(chips);
            if sections.is_empty() && owed.is_empty() {
                return None;
            }
            let text = if sections.is_empty() {
                String::new()
            } else {
                format!("Context for this message:\n{}", sections.join("\n"))
            };
            Some((vec![Entry::context(text.clone()).in_answer(answer)], (text, given)))
        });
        let text = match recorded {
            // Stored: what it carries is told once the request that sends it is (`history.rs`).
            Some((text, given)) => {
                self.queue_delivery(Delivery::context(&given, deliveries, describe));
                text
            }
            None => String::new(),
        };
        // A read of the log that failed before this context leaves the store saying so after it too.
        self.keep_unread_past_context();
        let mut blocks = Vec::with_capacity(images.len() + 1);
        if !text.is_empty() {
            blocks.push(ContentBlock::text(text));
        }
        blocks.extend(images);
        blocks
    }

    /// Reads a resource, or `None` if the server does not have it (the block is then simply shorter) or Stop abandoned
    /// the read (`await_mcp` marks the log unread then).
    fn read_blocks(&mut self, uri: &str) -> Option<Vec<ReplyBlock>> {
        let pending = self.link.read_resource(uri);
        self.await_mcp(&pending).ok()?.ok()
    }

    /// A tool of the server that may be missing; its result when present and not an error.
    fn optional_call(&mut self, tool: &str) -> Option<ToolReply> {
        let known = self.tools.as_ref().is_some_and(|tools| tools.iter().any(|t| t.name == tool));
        if !known {
            return None;
        }
        self.call_plain(tool, None).ok().filter(|reply| !reply.is_error)
    }

    /// The project summary: in full the first time, afterwards only when it changed (`last_describe`, set once a
    /// request carrying the context that gives it is sent). A heads-up leads its text, so the model reads it there.
    fn describe_if_new(&mut self, deliveries: &mut Vec<Delivery>) -> Option<String> {
        let reply = self.optional_call(DESCRIBE_TOOL)?;
        let text = plain_text(&reply.blocks);
        if self.last_describe.as_deref() == Some(text.as_str()) {
            return None;
        }
        deliveries.push(delivery_of(DESCRIBE_TOOL, None, &reply));
        Some(text)
    }

    /// What others changed since the last answer, from the structured result of `session_info`.
    fn heads_up(&mut self, deliveries: &mut Vec<Delivery>) -> Option<String> {
        let reply = self.optional_call(SESSION_INFO_TOOL)?;
        let heads_up = crate::history::heads_up_of(&reply)?;
        deliveries.push(delivery_of(SESSION_INFO_TOOL, None, &reply));
        Some(heads_up.json)
    }

    fn chip_section(&mut self, chip: &ContextChip, sections: &mut Vec<String>, images: &mut Vec<ContentBlock>) {
        match chip {
            ContextChip::Selection(paths) => {
                let listed: Vec<String> = paths.iter().map(|p| format!("{p:?}")).collect();
                sections.push(wrap_data("chip:selection", &listed.join("\n")));
            }
            ContextChip::Image { png, .. } => {
                images.push(ContentBlock::image(ImageMime::Png, png.clone(), ImageOrigin::File));
            }
            ContextChip::Resource(uri) => {
                if let Some(blocks) = self.read_blocks(uri) {
                    for block in blocks {
                        match block {
                            ReplyBlock::Text(text) => sections.push(wrap_data(uri, &text)),
                            ReplyBlock::Image { mime, bytes } => {
                                images.push(ContentBlock::image(mime, bytes, ImageOrigin::File));
                            }
                            ReplyBlock::Note(_) => {}
                        }
                    }
                }
            }
            // The viewport and notes are part of the UI context the server already described.
            _ => {}
        }
    }
}
