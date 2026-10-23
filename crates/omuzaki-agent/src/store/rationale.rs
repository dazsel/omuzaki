//! The rationale for "Explain this" (TH-08): the text the model wrote during the answer that made an undo step.
//!
//! It lives here and not in the journal because it quotes the private chat and the journal travels with the project.
//! The host captures the text with no extra tokens and stores it in the `Step` entry; this module serves it.

use omuzaki_agent_api::conversations::{Rationale, RationaleSource};
use omuzaki_agent_api::entry::Entry;
use omuzaki_ops_api::TxId;

use super::ConversationStore;

/// Most characters of a rationale that are kept.
const MAX_CHARS: usize = 2_000;

/// `text` cut to 2,000 characters (characters, not bytes, so a cut never splits a letter). The host calls this before
/// it writes the `Step` entry; the store applies it again on the way out.
pub fn cut_rationale(text: &str) -> String {
    text.chars().take(MAX_CHARS).collect()
}

impl RationaleSource for ConversationStore {
    /// Looks through this project's conversations, newest first, for the `Step` entry of `tx`. A conversation that
    /// cannot be read is skipped: a corrupt old chat must not hide a good newer one.
    fn rationale(&self, tx: TxId) -> Option<Rationale> {
        let rows = self.rows(true).ok()?;
        rows.into_iter().find_map(|row| {
            let entries = self.read(row.id).ok()?;
            entries.into_iter().find_map(|entry| match entry {
                Entry::Step { tx: found, rationale, .. } if found == tx => {
                    Some(Rationale::new(cut_rationale(&rationale), row.id))
                }
                _ => None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cut_counts_characters_not_bytes() {
        let long = "é".repeat(2_500);
        assert_eq!(cut_rationale(&long).chars().count(), 2_000);
        assert_eq!(cut_rationale("short"), "short");
    }
}
