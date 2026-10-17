//! A deliberately broken fake, to prove the suite catches what it names.

use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::TurnRequest;
use omuzaki_llm_api::stream::{Cancel, LlmEvent, TurnStream};
use omuzaki_llm_api::usage::ModelInfo;

use crate::{FakeLlm, Injected, Script};

/// A fake that retries a stream that failed after `Start`: it reports a `Retrying` just before the error, which the
/// contract forbids (llm-api D5: retries happen only before the first event, so no text is ever duplicated). The case
/// `a drop after Start is never retried` must fail on it.
pub struct RetriesAfterStart(FakeLlm);

impl RetriesAfterStart {
    /// A broken fake that plays `script`, except that every stream error after the first event is preceded by a retry.
    pub fn new(mut script: Script) -> Self {
        for turn in &mut script.turns {
            if let Some(Injected { before, .. }) = turn.error.as_mut().filter(|e| e.before >= 1) {
                let at = (*before).min(turn.events.len());
                turn.events.insert(at, LlmEvent::retrying(1, std::time::Duration::ZERO, "retrying after the drop"));
                // The error now comes after the retry it provoked.
                *before = at + 1;
            }
        }
        Self(FakeLlm::new(script))
    }
}

impl LlmClient for RetriesAfterStart {
    fn info(&self) -> &ModelInfo {
        self.0.info()
    }

    fn stream(&self, req: TurnRequest, cancel: &Cancel) -> Result<TurnStream, LlmError> {
        self.0.stream(req, cancel)
    }
}
