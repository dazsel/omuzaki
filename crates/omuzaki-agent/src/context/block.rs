//! The context block each user message carries before the person's text (D5), built at send time.
//!
//! It holds, when the server has them: the UI context (selection, workspace, viewport render), the project summary
//! (in full the first time, afterwards only when it changed), what others changed since the last answer, results that
//! arrived after a Stop, and what the person attached. Everything the person did not type is wrapped as data.

use std::time::Duration;

use omuzaki_agent_api::message::{ContextChip, UserMessage};
use omuzaki_llm_api::message::{ContentBlock, ImageMime, ImageOrigin};

use crate::content::{plain_text, wrap_data};
use crate::link::{ReplyBlock, ToolReply};
use crate::worker::Worker;

/// The resource that describes the person's screen: selection, workspace, focused panel, viewport, notes.
const UI_CONTEXT_URI: &str = "omuzaki://ui/context";
/// The tool that summarises the project.
const DESCRIBE_TOOL: &str = "project_describe";
/// The tool whose result carries the heads-up.
const SESSION_INFO_TOOL: &str = "session_info";

impl Worker {
    /// The blocks of the context for `msg`; empty when there is nothing to add.
    pub(crate) fn context_block(&mut self, msg: &UserMessage) -> Vec<ContentBlock> {
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
        if let Some(summary) = self.describe_if_new() {
            sections.push(wrap_data(DESCRIBE_TOOL, &summary));
        }
        if let Some(heads_up) = self.heads_up() {
            sections.push(wrap_data(SESSION_INFO_TOOL, &heads_up));
        }
        // Late results first: a heads-up one carried joins the changes held for this message.
        sections.extend(self.late_notes());
        sections.extend(self.missed_note());
        for chip in &msg.chips {
            self.chip_section(chip, &mut sections, &mut images);
        }

        if sections.is_empty() && images.is_empty() {
            return Vec::new();
        }
        let mut blocks = Vec::with_capacity(images.len() + 1);
        if !sections.is_empty() {
            blocks.push(ContentBlock::text(format!("Context for this message:\n{}", sections.join("\n"))));
        }
        blocks.extend(images);
        blocks
    }

    /// Reads a resource, or `None` if the server does not have it (the block is then simply shorter).
    fn read_blocks(&mut self, uri: &str) -> Option<Vec<ReplyBlock>> {
        let pending = self.link.read_resource(uri);
        loop {
            match pending.poll(Duration::from_millis(1)) {
                Some(Ok(Ok(blocks))) => return Some(blocks),
                Some(_) => return None,
                None if self.shared.stopping() => return None,
                None => {}
            }
        }
    }

    /// A tool of the server that may be missing; its result when present and not an error.
    fn optional_call(&mut self, tool: &str) -> Option<ToolReply> {
        let known = self.tools.as_ref().is_some_and(|tools| tools.iter().any(|t| t.name == tool));
        if !known {
            return None;
        }
        self.call_plain(tool, None).ok().filter(|reply| !reply.is_error)
    }

    /// The project summary: in full the first time, afterwards only when it changed. A heads-up leads its text, so
    /// the model reads it here when the summary is given.
    fn describe_if_new(&mut self) -> Option<String> {
        let reply = self.optional_call(DESCRIBE_TOOL)?;
        let text = plain_text(&reply.blocks);
        if self.last_describe.as_deref() == Some(text.as_str()) {
            return None;
        }
        self.delivered(DESCRIBE_TOOL, None, &reply);
        self.last_describe = Some(text.clone());
        Some(text)
    }

    /// What others changed since the last answer, from the structured result of `session_info`.
    fn heads_up(&mut self) -> Option<String> {
        let reply = self.optional_call(SESSION_INFO_TOOL)?;
        let heads_up = crate::history::heads_up_of(&reply)?;
        self.delivered(SESSION_INFO_TOOL, None, &reply);
        Some(heads_up.json)
    }

    /// Results that arrived after Stop, once, in the next message (D4).
    fn late_notes(&mut self) -> Vec<String> {
        let late = match self.late.lock() {
            Ok(mut late) => std::mem::take(&mut *late),
            Err(_) => return Vec::new(),
        };
        // The outcome can carry the first line of a server's result, so the whole note is data like any tool result.
        late.into_iter()
            .map(|l| {
                if let Some(heads_up) = l.heads_up {
                    self.log.hold(heads_up);
                }
                wrap_data("late_result", &format!("[your call `{}` finished after the stop: {}]", l.tool, l.outcome))
            })
            .collect()
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
