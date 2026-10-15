//! What the model sees each turn, in a byte-stable cache order (D5).

use omuzaki_llm_api::message::{CachePlan, SystemBlock, ToolChoice, ToolDef, TurnRequest};

use crate::content::tool_defs;
use crate::link::ToolInfo;
use crate::tools::host_tools;
use crate::worker::Worker;

/// The identity and rules, English, versioned (the first line names the version).
pub(crate) const SYSTEM_PROMPT: &str = include_str!("../../prompts/system.md");

/// The transaction tools the model is not offered: the host opens and closes the answer's transaction (D2.5).
fn is_hidden(name: &str) -> bool {
    matches!(name, "tx_begin" | "tx_commit" | "tx_rollback")
}

/// The tool list of every request: the server's tools without the `tx_*` built-ins, plus the host's own, sorted by name.
pub(crate) fn request_tools(server: &[ToolInfo]) -> Vec<ToolDef> {
    let visible: Vec<ToolInfo> = server.iter().filter(|t| !is_hidden(&t.name)).cloned().collect();
    let mut tools = tool_defs(&visible);
    tools.extend(host_tools());
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}

impl Worker {
    /// System 3: what the host knows that changes rarely.
    fn host_facts(&self) -> String {
        let caps = &self.cfg.caps;
        format!(
            "Reply in {} ({}); tool names, arguments and ids stay English.\nModel: {}.\nPer message you may use at most {} turns, {} tool calls, {} s of wall time and {} tokens.",
            self.cfg.language.name,
            self.cfg.language.tag,
            self.llm.info().id,
            caps.turns,
            caps.tool_calls,
            caps.wall_s,
            caps.tokens,
        )
    }

    /// The request for the next turn: tools, three system blocks, the history.
    pub(crate) fn build_request(&self, choice: ToolChoice) -> TurnRequest {
        let mut system = vec![SystemBlock::new(SYSTEM_PROMPT)];
        if let Some(instructions) = self.link.instructions() {
            system.push(SystemBlock::new(instructions));
        }
        system.push(SystemBlock::new(self.host_facts()).with_cache_after(true));
        let tools = request_tools(self.tools.as_deref().unwrap_or_default());
        TurnRequest::new(self.messages.clone(), self.cfg.max_output)
            .with_system(system)
            .with_tools(tools)
            .with_tool_choice(choice)
            .with_effort(self.cfg.effort)
            .with_cache(CachePlan::new(true))
    }
}

/// A chip as the model reads it, in the person's message and in a rebuilt history.
pub(crate) fn chip_note(chip: &omuzaki_agent_api::message::ContextChip) -> String {
    use omuzaki_agent_api::message::ContextChip;
    match chip {
        ContextChip::Selection(paths) => format!("[attached: selection of {} paths]", paths.len()),
        ContextChip::Viewport => "[attached: the viewport]".to_owned(),
        ContextChip::Note(id) => format!("[attached: note {}]", id.0),
        ContextChip::Image { png, .. } => format!("[attached: an image of {} bytes]", png.len()),
        ContextChip::Resource(uri) => format!("[attached: resource {uri}]"),
        _ => "[attached: something]".to_owned(),
    }
}

/// The person's own text with a note for each chip: what is stored and what is rebuilt on resume.
pub(crate) fn person_text(text: &str, chips: &[omuzaki_agent_api::message::ContextChip]) -> String {
    let mut out = text.to_owned();
    for chip in chips {
        out.push('\n');
        out.push_str(&chip_note(chip));
    }
    out
}

impl Worker {
    /// The blocks of a user message: what the person typed, with a note for each chip.
    pub(crate) fn user_blocks(&mut self, msg: &omuzaki_agent_api::message::UserMessage) -> Vec<omuzaki_llm_api::message::ContentBlock> {
        vec![omuzaki_llm_api::message::ContentBlock::text(person_text(&msg.text, &msg.chips))]
    }
}
