//! Compaction (D5): when the window fills, the old history is never sent again. The host builds a bounded digest from
//! the stored conversation with no LLM, one summary turn writes it up, and the conversation continues from one rebuilt
//! user message whose size is checked as a whole.
//!
//! Sizes are the `Ledger`'s estimate, an upper bound: 1 token per byte, 8 per block, images at the model's
//! `image_cost`. The request the rebuilt message makes is the same system prefix and tools as every other request.

use std::fmt::Write as _;

use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::{PlanItem, PlanStatus};
use omuzaki_llm_api::message::{
    ContentBlock, ImageOrigin, Message, Role, SystemBlock, ToolChoice, ToolDef, TurnRequest,
};
use omuzaki_llm_api::usage::{Ledger, ModelInfo};

use crate::context::person_text;
use crate::run::{AnswerRun, Flow};
use crate::turn::TurnMode;
use crate::worker::Worker;

/// The most output tokens of the summary turn.
pub(crate) const SUMMARY_MAX_OUTPUT: u32 = 2_000;
/// The digest stops growing at this share of the window, in percent.
const DIGEST_PERCENT: u64 = 40;
/// The rebuilt message grows past its mandatory part only while the whole request stays within this share.
const KEEP_PERCENT: u64 = 60;
/// Compaction starts when the measured prompt passes this share of the window.
const TRIGGER_PERCENT: u64 = 75;
/// Optional earlier messages are cut to this many characters when they do not fit whole.
const CUT_CHARS: usize = 200;
/// The most plan items kept, and the most characters of each.
const PLAN_ITEMS: usize = 20;
const PLAN_ITEM_CHARS: usize = 200;
/// What the summary turn is asked.
const SUMMARY_ASK: &str = "Below is the record of a conversation. Write a summary of what was asked, what was done and decided, and what is left, so the work can go on from it. Keep names, numbers and the person's preferences. Do not call tools.";

/// Whether a measured prompt of `measured` tokens passes 75 % of `window`.
pub(crate) fn should_compact(measured: u64, window: u64) -> bool {
    u128::from(measured) * 100 > u128::from(window) * u128::from(TRIGGER_PERCENT)
}

fn chars(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

/// The whole request's size: the prefix, the blocks of one user message, the block overhead and `max_output`.
fn request_size(
    system: &[SystemBlock],
    tools: &[ToolDef],
    blocks: &[ContentBlock],
    max_output: u32,
    info: &ModelInfo,
) -> u64 {
    let request = TurnRequest::new(vec![Message::new(Role::User, blocks.to_vec())], max_output)
        .with_system(system.to_vec())
        .with_tools(tools.to_vec());
    Ledger::new().prompt_estimate(&request, info).saturating_add(u64::from(max_output))
}

/// What goes into the rebuilt message.
pub(crate) struct RebuildInput<'a> {
    pub system: &'a [SystemBlock],
    pub tools: &'a [ToolDef],
    pub max_output: u32,
    /// The summary turn's output.
    pub summary: String,
    /// Pending approvals and questions, verbatim.
    pub pending: Option<String>,
    /// The plan, verbatim.
    pub plan: Option<String>,
    /// The latest message of the person, verbatim.
    pub latest: String,
    /// The person's earlier messages, oldest first.
    pub earlier: Vec<String>,
    /// The latest render in the history.
    pub render: Option<ContentBlock>,
}

fn left_out_line(earlier: usize, render: bool) -> Option<String> {
    let mut parts = Vec::new();
    if earlier > 0 {
        parts.push(format!("{earlier} earlier message{}", if earlier == 1 { "" } else { "s" }));
    }
    if render {
        parts.push("the last render".to_owned());
    }
    (!parts.is_empty()).then(|| format!("{}, kept in this conversation's history", parts.join(" and ")))
}

/// Builds the rebuilt message, sizing the whole request at each step (D5, step 3). `None` when the mandatory part and
/// `max_output` already pass the window: nothing is sent and the answer ends with `ContextFull`.
pub(crate) fn rebuild(input: &RebuildInput<'_>, info: &ModelInfo) -> Option<Vec<ContentBlock>> {
    let size = |blocks: &[ContentBlock]| request_size(input.system, input.tools, blocks, input.max_output, info);
    let window = info.context_window;
    let keep_limit = window * KEEP_PERCENT / 100;

    let mut head = vec![ContentBlock::text(input.summary.clone())];
    head.extend(input.pending.clone().map(ContentBlock::text));
    head.extend(input.plan.clone().map(ContentBlock::text));
    let latest = ContentBlock::text(input.latest.clone());
    let assemble = |earlier: &[String], render: Option<&ContentBlock>, line: Option<String>| {
        let mut blocks = head.clone();
        blocks.extend(earlier.iter().cloned().map(ContentBlock::text));
        blocks.extend(render.cloned());
        blocks.extend(line.map(ContentBlock::text));
        blocks.push(latest.clone());
        blocks
    };

    if size(&assemble(&[], None, None)) > window {
        return None;
    }

    // Earlier messages, newest first: whole if they fit, else cut, else this and every older one stays out.
    let mut chosen: Vec<String> = Vec::new();
    let mut cut_off = false;
    let mut left = 0;
    for message in input.earlier.iter().rev() {
        if cut_off {
            left += 1;
            continue;
        }
        let cut = chars(message, CUT_CHARS);
        let candidates = if cut == *message { vec![cut] } else { vec![message.clone(), cut] };
        let fitted = candidates.into_iter().find(|candidate| {
            let mut trial: Vec<String> = chosen.iter().rev().cloned().collect();
            trial.push(candidate.clone());
            // `trial` is oldest first once the new, older message is put in front.
            let ordered: Vec<String> = std::iter::once(candidate.clone()).chain(chosen.iter().rev().cloned()).collect();
            let _ = trial;
            size(&assemble(&ordered, None, None)) <= keep_limit
        });
        if let Some(candidate) = fitted {
            chosen.insert(0, candidate);
        } else {
            cut_off = true;
            left += 1;
        }
    }
    let render_fits =
        input.render.as_ref().is_none_or(|image| size(&assemble(&chosen, Some(image), None)) <= keep_limit);
    let render = input.render.as_ref().filter(|_| render_fits);
    let render_left_out = input.render.is_some() && !render_fits;

    let line = left_out_line(left, render_left_out);
    let mut blocks = assemble(&chosen, render, line.clone());
    if size(&blocks) > window {
        // The note itself must fit: drop the optional part and say so.
        let all_left = input.earlier.len();
        blocks = assemble(&[], None, left_out_line(all_left, input.render.is_some()));
        if size(&blocks) > window {
            return None;
        }
        return Some(blocks);
    }
    Some(blocks)
}

/// The digest the summary turn is given: step labels, the plan, what is pending, and as much of the person's and the
/// model's text as fits under 40 % of the window, newest first. Images and tool results are left out.
pub(crate) fn digest(entries: &[Entry], plan: Option<&str>, pending: Option<&str>, window: u64) -> String {
    let limit = (window * DIGEST_PERCENT / 100).saturating_sub(u64::try_from(SUMMARY_ASK.len()).unwrap_or(0) + 64);
    let mut head = String::new();
    let steps: Vec<&str> = entries
        .iter()
        .filter_map(|e| if let Entry::Step { label, .. } = e { Some(label.as_str()) } else { None })
        .collect();
    if !steps.is_empty() {
        let _ = writeln!(head, "Steps made: {}", steps.join("; "));
    }
    for part in [plan, pending].into_iter().flatten() {
        head.push_str(part);
        head.push('\n');
    }
    let mut used = u64::try_from(head.len()).unwrap_or(u64::MAX);
    let mut picked: Vec<(usize, String)> = Vec::new();
    let mut persons_open = true;
    for (at, entry) in entries.iter().enumerate().rev() {
        let (label, texts): (&str, Vec<String>) = match entry {
            Entry::User { text, chips, .. } => ("Person", vec![person_text(text, chips)]),
            Entry::Assistant { blocks, .. } => (
                "Assistant",
                blocks
                    .iter()
                    .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.clone()) } else { None })
                    .collect(),
            ),
            _ => continue,
        };
        if label == "Person" && !persons_open {
            continue;
        }
        for text in texts.into_iter().rev() {
            let whole = format!("{label}: {text}\n");
            let cut = format!("{label}: {}\n", chars(&text, CUT_CHARS));
            // The person's words are cut to 200 characters when they do not fit whole; the model's text is whole or left out.
            let candidates = if label == "Person" { vec![whole, cut] } else { vec![whole] };
            let choice = candidates
                .into_iter()
                .find(|candidate| used + u64::try_from(candidate.len()).unwrap_or(u64::MAX) <= limit);
            match choice {
                Some(line) => {
                    used += u64::try_from(line.len()).unwrap_or(u64::MAX);
                    picked.push((at, line));
                }
                None if label == "Person" => persons_open = false,
                None => {}
            }
        }
    }
    picked.sort_by_key(|(at, _)| *at);
    let mut out = head;
    for (_, line) in picked {
        out.push_str(&line);
    }
    out
}

fn plan_text(plan: &[PlanItem]) -> Option<String> {
    if plan.is_empty() {
        return None;
    }
    let lines: Vec<String> = plan
        .iter()
        .take(PLAN_ITEMS)
        .map(|item| {
            let mark = match item.status {
                PlanStatus::Todo => "todo",
                PlanStatus::Doing => "doing",
                PlanStatus::Done => "done",
                _ => "?",
            };
            format!("- [{mark}] {}", chars(&item.text, PLAN_ITEM_CHARS))
        })
        .collect();
    Some(format!("Plan:\n{}", lines.join("\n")))
}

impl Worker {
    /// Whether the next request should be preceded by a compaction.
    pub(crate) fn wants_compaction(&self) -> bool {
        should_compact(self.measured, self.llm.info().context_window)
    }

    fn pending_text(&self, entries: &[Entry]) -> Option<String> {
        let mut lines: Vec<String> =
            self.sessions.approvals().iter().map(|a| format!("Pending approval #{}: {:?}", a.id.0, a.need)).collect();
        let answered: Vec<_> =
            entries.iter().filter_map(|e| if let Entry::Answer { id, .. } = e { Some(*id) } else { None }).collect();
        for entry in entries {
            if let Entry::Question { id, text, .. } = entry
                && !answered.contains(id)
            {
                lines.push(format!("Open question: {text}"));
            }
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    fn latest_render(&self) -> Option<ContentBlock> {
        fn find(blocks: &[ContentBlock]) -> Option<ContentBlock> {
            blocks.iter().rev().find_map(|b| match b {
                ContentBlock::Image { origin: ImageOrigin::Render, .. } => Some(b.clone()),
                ContentBlock::ToolResult { blocks, .. } => find(blocks),
                _ => None,
            })
        }
        self.messages.iter().rev().find_map(|m| find(&m.content))
    }

    /// Compacts the conversation: digest, one summary turn, the rebuilt first message (D5).
    ///
    /// # Errors
    /// `ContextFull` if the mandatory part cannot fit; otherwise the summary turn's own failures.
    pub(crate) fn compact(&mut self, run: &mut AnswerRun) -> Result<(), Flow> {
        let info = self.llm.info().clone();
        let window = info.context_window;
        let entries = self.shared.transcript();
        let plan = plan_text(&self.plan);
        let pending = self.pending_text(&entries);
        let digest = digest(&entries, plan.as_deref(), pending.as_deref(), window);

        let system = self.system_blocks();
        let tools = self.tool_defs_for_request();
        let ask = ContentBlock::text(format!("{SUMMARY_ASK}\n\n{digest}"));
        let request = TurnRequest::new(vec![Message::new(Role::User, vec![ask])], SUMMARY_MAX_OUTPUT)
            .with_system(system.clone())
            .with_tools(tools.clone())
            .with_tool_choice(ToolChoice::None)
            .with_effort(self.cfg.effort);
        self.reserve_budget(run, &request)?;
        let done = self.run_turn(run, &request, TurnMode::Summary)?;
        let summary: String = done
            .blocks
            .iter()
            .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
            .collect();
        let summary = if summary.trim().is_empty() { "(no summary was written)".to_owned() } else { summary };

        let users: Vec<String> = entries
            .iter()
            .filter_map(|e| if let Entry::User { text, chips, .. } = e { Some(person_text(text, chips)) } else { None })
            .collect();
        let (latest, earlier) =
            users.split_last().map_or((String::new(), Vec::new()), |(l, e)| (l.clone(), e.to_vec()));
        let input = RebuildInput {
            system: &system,
            tools: &tools,
            max_output: self.cfg.max_output,
            summary,
            pending,
            plan,
            latest,
            earlier,
            render: self.latest_render(),
        };
        let Some(rebuilt) = rebuild(&input, &info) else {
            return Err(Flow::Fail(HostError::ContextFull));
        };
        let message: String = rebuilt
            .iter()
            .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("\n");
        self.messages = vec![Message::new(Role::User, rebuilt)];
        self.ledger = Ledger::new();
        self.measured = 0;
        run.compacted = true;
        self.shared.record(Entry::compacted(message));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_llm_api::usage::ImageCost;

    use super::*;

    fn info(window: u64) -> ModelInfo {
        ModelInfo::new("m", window, 4_096, ImageCost::tiles(28))
    }

    /// A system block whose prefix estimate is exactly `tokens`: its text plus 8 for the block.
    fn prefix(tokens: usize) -> Vec<SystemBlock> {
        vec![SystemBlock::new("p".repeat(tokens - 8))]
    }

    fn input(system: &[SystemBlock], earlier: Vec<String>, render: Option<ContentBlock>) -> RebuildInput<'_> {
        RebuildInput {
            system,
            tools: &[],
            max_output: 2_048,
            summary: "s".repeat(2_000),
            pending: Some("a".repeat(50)),
            plan: None,
            latest: "l".repeat(150),
            earlier,
            render,
        }
    }

    /// An image block of 361 tokens: 19 x 19 tiles of 28 px on a 532 x 532 picture.
    fn render() -> ContentBlock {
        let mut png = vec![0u8; 24];
        png[16..20].copy_from_slice(&532u32.to_be_bytes());
        png[20..24].copy_from_slice(&532u32.to_be_bytes());
        ContentBlock::image(omuzaki_llm_api::message::ImageMime::Png, png, ImageOrigin::Render)
    }

    #[test]
    fn the_trigger_is_strictly_above_three_quarters_of_the_window() {
        assert!(!should_compact(7_500, 10_000));
        assert!(should_compact(7_501, 10_000));
    }

    #[test]
    fn the_boundary_case_sends_8172_and_names_what_it_left_out() {
        let system = prefix(3_900);
        let earlier = vec!["e".repeat(2_000 - 8)];
        let rebuilt = rebuild(&input(&system, earlier, Some(render())), &info(10_000)).expect("fits");
        let size = |blocks: &[ContentBlock]| request_size(&system, &[], blocks, 2_048, &info(10_000));
        let mandatory: Vec<ContentBlock> = rebuilt
            .iter()
            .filter(|b| !matches!(b, ContentBlock::Text(t) if t.contains("kept in this")))
            .cloned()
            .collect();
        assert_eq!(size(&mandatory), 8_172, "3,900 + 2,200 + 3 blocks of 8 + 2,048");
        assert!(size(&rebuilt) <= 10_000, "with the note it still fits");
        let line = rebuilt.iter().find_map(|b| match b {
            ContentBlock::Text(t) if t.contains("kept in this conversation's history") => Some(t.clone()),
            _ => None,
        });
        assert_eq!(line.as_deref(), Some("1 earlier message and the last render, kept in this conversation's history"));
        assert!(!rebuilt.iter().any(|b| matches!(b, ContentBlock::Image { .. })));
    }

    #[test]
    fn with_room_the_earlier_messages_and_the_render_come_back_within_sixty_percent() {
        let system = prefix(3_900);
        let earlier = vec!["a".repeat(500), "b".repeat(500)];
        let rebuilt = rebuild(&input(&system, earlier.clone(), Some(render())), &info(20_000)).expect("fits");
        assert!(
            !rebuilt.iter().any(|b| matches!(b, ContentBlock::Text(t) if t.contains("kept in this"))),
            "nothing left out"
        );
        let texts: Vec<&str> = rebuilt
            .iter()
            .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
            .collect();
        let at = |needle: &str| texts.iter().position(|t| t.starts_with(needle)).unwrap();
        assert!(at("aaa") < at("bbb"), "oldest first");
        assert!(rebuilt.iter().any(|b| matches!(b, ContentBlock::Image { .. })));
        let size = request_size(&system, &[], &rebuilt, 2_048, &info(20_000));
        assert!(size <= 12_000, "{size} is within 60 % of 20,000");
    }

    #[test]
    fn a_message_that_does_not_fit_whole_is_cut_to_200_characters() {
        let system = prefix(3_900);
        // 20,000 x 60 % = 12,000; the fixed part is 8,172; 4,500 more do not fit whole but 200 do.
        let earlier = vec!["z".repeat(4_500)];
        let rebuilt = rebuild(&input(&system, earlier, None), &info(20_000)).expect("fits");
        let kept: Vec<&str> = rebuilt
            .iter()
            .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
            .filter(|t| t.starts_with('z'))
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].len(), 200);
    }

    #[test]
    fn when_the_mandatory_part_passes_the_window_nothing_is_built() {
        let system = prefix(3_900);
        let mut big = input(&system, vec![], None);
        big.latest = "l".repeat(2_000);
        assert!(rebuild(&big, &info(10_000)).is_none(), "3,900 + 2,000 + 2,050 + 2,048 passes 10,000");
    }

    #[test]
    fn the_digest_keeps_steps_and_the_newest_text_under_forty_percent() {
        let entries = vec![
            Entry::user("old request", vec![]),
            Entry::assistant(vec![ContentBlock::text("x".repeat(30_000))]),
            Entry::step(omuzaki_ops_api::TxId(1), "Assistant: oval", "r"),
            Entry::user("newest request", vec![]),
            Entry::assistant(vec![ContentBlock::text("newest answer")]),
        ];
        let text = digest(&entries, None, None, 10_000);
        assert!(text.len() <= 4_000, "{}", text.len());
        assert!(text.contains("Steps made: Assistant: oval"));
        assert!(text.contains("Person: newest request") && text.contains("Assistant: newest answer"));
        assert!(!text.contains("xxxx"), "the huge text did not fit and is left out");
    }
}
