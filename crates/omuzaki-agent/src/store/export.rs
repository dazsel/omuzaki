//! Export: a Markdown transcript, the images it shows beside it, and the raw `log.jsonl`.
//!
//! The transcript is for reading, so it shows what was said, what was done and what it cost; opaque provider blocks
//! (thinking) and the tool-use blocks (shown once, as their `tool_call` entries) are left out. The raw log next to it
//! is the complete record. Output depends only on the log, so exporting twice gives the same bytes.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use omuzaki_agent_api::conversations::ConversationId;
use omuzaki_agent_api::entry::{Entry, SessionNote, StoredBlock};
use omuzaki_agent_api::error::HostError;
use omuzaki_llm_api::message::ContentBlock;

use super::log::{self, Parsed, Stamped};
use super::{ConversationStore, LOG, blobs, io_err};

impl ConversationStore {
    /// Writes `conversation-<id>.md`, the images it links (named by hash, beside it) and a byte-for-byte copy of
    /// `log.jsonl` into `dest`, which is created if it does not exist.
    ///
    /// # Errors
    /// [`HostError::Store`] for an unknown id or a failed write.
    pub fn export(&self, id: ConversationId, dest: &Path) -> Result<(), HostError> {
        // One locked snapshot: the transcript and the copied log come from the same bytes, and a delete or an append
        // cannot slip in between.
        let _guard = self.guard();
        let log_path = self.conversation_dir(id)?.join(LOG);
        let bytes = fs::read(&log_path).map_err(|e| io_err("read the log", e))?;
        let parsed = log::parse(&bytes)?;
        let project_dir = self.project_dir()?;
        fs::create_dir_all(dest).map_err(|e| io_err("create the export folder", e))?;

        // The transcript shows what the person saw: where a Stop recorded how much of its answer was read, the earlier
        // turns of that answer are cut to it. The raw log, copied below, is untouched.
        let cut = crate::resume::apply_stop_cuts(parsed.entries.iter().map(|s| s.entry.clone()).collect());
        let mut markdown = header(id, &parsed);
        for (stamped, entry) in parsed.entries.iter().zip(cut) {
            let missing = copy_images(&project_dir, dest, &entry)?;
            markdown.push_str(&render(&Stamped { at: stamped.at.clone(), entry }, &missing));
        }
        fs::write(dest.join(format!("conversation-{}.md", id.0)), markdown)
            .map_err(|e| io_err("write the transcript", e))?;
        fs::write(dest.join(LOG), &bytes).map_err(|e| io_err("copy the log", e))?;
        Ok(())
    }
}

/// Copies the blobs a `ToolResult` shows into `dest`. Returns the hashes whose blob file is gone, so the transcript
/// can say so instead of linking nothing.
fn copy_images(project_dir: &Path, dest: &Path, entry: &Entry) -> Result<Vec<String>, HostError> {
    let mut missing = Vec::new();
    if let Entry::ToolResult { blocks, .. } = entry {
        for block in blocks {
            if let StoredBlock::Image { mime, blob, .. } = block {
                let source = blobs::path(project_dir, blob, *mime)?;
                let target = dest.join(file_name(&source));
                if source.is_file() {
                    fs::copy(&source, &target).map_err(|e| io_err("copy an image", e))?;
                } else {
                    missing.push(blob.clone());
                }
            }
        }
    }
    Ok(missing)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn header(id: ConversationId, parsed: &Parsed) -> String {
    let h = &parsed.header;
    format!("# Conversation {}\n\n- Model: {}\n- Created: {}\n- Project: {}\n", id.0, h.model, h.created, h.project)
}

/// One entry as Markdown, preceded by a blank line.
fn render(stamped: &Stamped, missing: &[String]) -> String {
    let at = &stamped.at;
    match &stamped.entry {
        Entry::User { text, chips, .. } => {
            let mut out = format!("\n## You ({at})\n\n{text}\n");
            if !chips.is_empty() {
                let labels: Vec<String> = chips.iter().map(|c| format!("{c:?}")).collect();
                let _ = write!(out, "\nAttached: {}\n", labels.join(", "));
            }
            out
        }
        Entry::Assistant { blocks, .. } => {
            let texts: Vec<&str> = blocks.iter().filter_map(text_of).collect();
            if texts.is_empty() { String::new() } else { format!("\n## Assistant ({at})\n\n{}\n", texts.join("\n\n")) }
        }
        Entry::ToolCall { tool, summary, .. } => format!("\n- Tool call `{tool}`: {summary}\n"),
        Entry::ToolResult { ok, blocks, .. } => render_result(*ok, blocks, missing),
        Entry::Approval { id, need, resolution, .. } => match resolution {
            Some(r) => format!("\n- Approval #{} ({need:?}): {:?}\n", id.0, r.result),
            None => format!("\n- Approval #{} ({need:?}): waiting\n", id.0),
        },
        Entry::Question { id, text, choices, .. } => {
            let options = if choices.is_empty() { String::new() } else { format!(" [{}]", choices.join(" / ")) };
            format!("\n- Question #{}: {text}{options}\n", id.0)
        }
        Entry::Answer { id, text, .. } => format!("\n- Answer to #{}: {text}\n", id.0),
        Entry::Charge { usage, usd_micros, .. } => format!(
            "\n- Cost: {usd_micros} \u{b5}$ ({} in, {} out, {} cache read, {} cache write)\n",
            usage.input, usage.output, usage.cache_read, usage.cache_write
        ),
        Entry::Step { tx, label, rationale, .. } => {
            let mut out = format!("\n- Undo step {}: {label}\n", tx.0);
            if !rationale.is_empty() {
                out.push_str(&quote(rationale));
            }
            out
        }
        Entry::Session { id, note, .. } => match note {
            SessionNote::Opened => format!("\n- Session {} opened\n", id.0),
            SessionNote::Ended(reason) => format!("\n- Session {} ended: {reason:?}\n", id.0),
            _ => format!("\n- Session {} changed\n", id.0),
        },
        Entry::Compacted { .. } => "\n- The history was compacted.\n".to_owned(),
        Entry::Stopped { .. } => "\n- Stopped by the person.\n".to_owned(),
        _ => String::new(),
    }
}

fn text_of(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::Text(text) => Some(text),
        _ => None,
    }
}

fn render_result(ok: bool, blocks: &[StoredBlock], missing: &[String]) -> String {
    let mut out = format!("\n- Result ({})\n", if ok { "ok" } else { "failed" });
    for block in blocks {
        match block {
            StoredBlock::Text(text) => out.push_str(&quote(text)),
            StoredBlock::Image { mime, blob, .. } if missing.contains(blob) => {
                let _ = write!(out, "\n  (image {blob} is no longer stored, {mime:?})\n");
            }
            StoredBlock::Image { mime, blob, .. } => {
                let _ = write!(out, "\n  ![{blob}]({blob}.{})\n", blobs::extension(*mime));
            }
            _ => {}
        }
    }
    out
}

/// `text` as an indented block quote under a list item, one `> ` per line.
fn quote(text: &str) -> String {
    let mut out = String::from("\n");
    for line in text.lines() {
        let _ = writeln!(out, "{}", format!("  > {line}").trim_end());
    }
    out
}
