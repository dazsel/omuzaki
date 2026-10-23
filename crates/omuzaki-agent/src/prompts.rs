//! The host's MCP prompt cache and `prompt` (§7, #1134).

use std::collections::BTreeMap;

use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::prompts::{PromptInfo, PromptsState};
use rmcp::model::JsonObject;
use serde_json::Value;

use crate::link::{LinkError, Pending};
use crate::shared::Shared;
use crate::worker::Worker;

/// A prompt's text is cut off at this size: a longer one is an error, not a truncation (§7).
pub(crate) const MAX_PROMPT_BYTES: usize = 16 * 1024;
/// How long `prompt` waits for the server, in clock seconds (§7).
pub(crate) const PROMPT_TIMEOUT_S: f64 = 2.0;

type Fetch = Pending<Result<Vec<PromptInfo>, LinkError>>;

/// The refetch state of the host thread.
#[derive(Default)]
pub(crate) struct PromptFetch {
    pending: Option<Fetch>,
    /// Another fetch is wanted as soon as this one is done.
    again: bool,
}

impl Worker {
    /// Starts the first fetch, at `start`.
    pub(crate) fn start_prompt_fetch(&mut self) {
        self.shared.lock().refresh_prompts = true;
    }

    /// One tick of the cache (the host thread calls it between commands): collects a finished fetch and starts the next
    /// one when the panel asked (`refresh_prompts`), the server said the list changed, or an answer just ended.
    pub(crate) fn service_prompts(&mut self) {
        let finished = self.prompt_fetch.pending.as_ref().and_then(|p| p.poll(std::time::Duration::ZERO));
        if let Some(result) = finished {
            self.prompt_fetch.pending = None;
            let state = match result {
                Ok(Ok(list)) => PromptsState::Ready(list),
                Ok(Err(error)) | Err(error) => PromptsState::Failed(error.into()),
            };
            self.shared.lock().prompts = state;
        }
        let asked = std::mem::take(&mut self.shared.lock().refresh_prompts);
        if asked || self.link.take_prompts_changed() || self.refetch_after_answer {
            self.prompt_fetch.again = true;
            self.refetch_after_answer = false;
        }
        if self.prompt_fetch.again && self.prompt_fetch.pending.is_none() {
            self.prompt_fetch.again = false;
            self.prompt_fetch.pending = Some(self.link.list_prompts());
        }
    }
}

impl Shared {
    /// The prompt list for the panel: the cache, or why there is none.
    pub(crate) fn prompts_state(&self) -> PromptsState {
        if self.session_over() {
            return PromptsState::Failed(HostError::SessionEnded);
        }
        self.lock().prompts.clone()
    }

    /// Marks the list stale; the host thread refetches. `Loading` at once.
    pub(crate) fn mark_prompts_stale(&self) {
        let mut ctl = self.lock();
        ctl.prompts = PromptsState::Loading;
        ctl.refresh_prompts = true;
    }
}

/// The arguments of `prompts/get` as JSON strings.
pub(crate) fn prompt_arguments(args: &BTreeMap<String, String>) -> JsonObject {
    args.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect()
}

/// Checks a fetched prompt text against the size limit.
pub(crate) fn check_prompt_text(text: String) -> Result<String, HostError> {
    if text.len() > MAX_PROMPT_BYTES {
        return Err(HostError::Mcp("prompt text too long".into()));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_kib_passes_and_one_more_byte_fails() {
        assert!(check_prompt_text("a".repeat(16_384)).is_ok());
        assert_eq!(check_prompt_text("a".repeat(16_385)), Err(HostError::Mcp("prompt text too long".into())));
    }

    #[test]
    fn arguments_reach_the_server_unchanged() {
        let args = BTreeMap::from([("character".to_owned(), " Mira ".to_owned())]);
        assert_eq!(prompt_arguments(&args)["character"], " Mira ");
    }
}
