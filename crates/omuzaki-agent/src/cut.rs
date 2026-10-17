//! Stop's cutoff: a byte count into the text of one answer, applied wherever that text is kept (the transcript, the
//! model's history, the stored log, the export). Every place counts and cuts through this module, so the unit of
//! the count (UTF-8 bytes of text blocks, assistant turns only) is defined once.

use omuzaki_agent_api::entry::Entry;
use omuzaki_llm_api::message::{ContentBlock, Message, Role};

/// Bytes of text in `blocks`; other blocks count nothing.
pub(crate) fn text_bytes<'a>(blocks: impl IntoIterator<Item = &'a ContentBlock>) -> usize {
    blocks.into_iter().map(|b| if let ContentBlock::Text(t) = b { t.len() } else { 0 }).sum()
}

/// Bytes of assistant text in `entries`.
pub(crate) fn assistant_text_bytes(entries: &[Entry]) -> usize {
    entries
        .iter()
        .filter_map(|e| if let Entry::Assistant { blocks, .. } = e { Some(text_bytes(blocks)) } else { None })
        .sum()
}

/// The blocks of the assistant turns in `entries`, in order.
pub(crate) fn assistant_blocks_mut(entries: &mut [Entry]) -> impl Iterator<Item = &mut Vec<ContentBlock>> {
    entries.iter_mut().filter_map(|e| if let Entry::Assistant { blocks, .. } = e { Some(blocks) } else { None })
}

/// Cuts the answer in the model's history, which starts at message `from`, to the `read` bytes the person saw. Stop
/// counts the answer's text from its first byte; `folded` of those bytes were folded into a summary by a compaction, so
/// they are not in `messages`.
pub(crate) fn cut_history(messages: &mut [Message], from: usize, read: usize, folded: usize) {
    // `from` is rebased when the history is compacted, but a stale one must never index past the end.
    let turns = messages.iter_mut().skip(from).filter(|m| m.role == Role::Assistant).map(|m| &mut m.content);
    cut_text_blocks(turns, read.saturating_sub(folded));
}

/// Cuts the text of `blocks` (the text blocks of consecutive assistant turns, in order) so that at most `budget` bytes
/// remain in total. Returns what is left of the budget. Blocks that become empty are dropped.
pub(crate) fn cut_text_blocks<'a>(
    turns: impl IntoIterator<Item = &'a mut Vec<ContentBlock>>,
    mut budget: usize,
) -> usize {
    for blocks in turns {
        blocks.retain_mut(|block| {
            let ContentBlock::Text(text) = block else { return true };
            if text.len() <= budget {
                budget -= text.len();
                return !text.is_empty();
            }
            let mut end = budget;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            budget = 0;
            end > 0
        });
    }
    budget
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cut_keeps_text_in_order_until_the_budget_is_spent() {
        let mut first = vec![ContentBlock::text("abc"), ContentBlock::tool_use("c", "t", serde_json::json!({}))];
        let mut second = vec![ContentBlock::text("defg")];
        let left = cut_text_blocks([&mut first, &mut second], 5);
        assert_eq!(left, 0);
        assert_eq!(first[0], ContentBlock::text("abc"));
        assert_eq!(second, vec![ContentBlock::text("de")]);
    }

    #[test]
    fn a_cut_to_nothing_drops_the_block_and_never_splits_a_character() {
        let mut blocks = vec![ContentBlock::text("€€")];
        cut_text_blocks([&mut blocks], 4);
        assert_eq!(blocks, vec![ContentBlock::text("€")]);
        cut_text_blocks([&mut blocks], 0);
        assert_eq!(blocks, Vec::<ContentBlock>::new());
    }
}
