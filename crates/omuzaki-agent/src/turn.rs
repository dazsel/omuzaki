//! One LLM turn: stream it, forward the text, charge it once (D2.3, D3).

use std::time::Duration;

use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_core::time::Seconds;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{ContentBlock, Message, Role, TurnRequest};
use omuzaki_llm_api::stream::{LlmEvent, Next, StopReason, Turn};
use omuzaki_llm_api::usage::{PriceTable, Usage};
use omuzaki_ops_api::Spend;

use crate::fold::TurnFold;
use crate::run::{AnswerRun, Flow};
use crate::worker::Worker;

/// How long one look at the stream waits.
const STREAM_STEP: Duration = Duration::from_millis(10);
/// Text reaches the panel at most this often, in seconds of clock time (D2.3).
const FLUSH_EVERY: f64 = 0.016;

/// What a turn is for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum TurnMode {
    /// A turn of the answer: its text goes to the panel and its blocks join the history.
    Answer,
    /// The compaction's summary turn: charged like any turn, but its text is not shown and joins no history.
    Summary,
}

/// A finished turn.
pub(crate) struct TurnDone {
    pub blocks: Vec<ContentBlock>,
    pub stop: StopReason,
}

/// Text waiting to be sent to the panel, coalesced to one event per [`FLUSH_EVERY`].
struct TextBuf {
    text: String,
    last_flush: Option<Seconds>,
}

impl TextBuf {
    fn new() -> Self {
        Self { text: String::new(), last_flush: None }
    }

    fn due(&self, now: Seconds) -> bool {
        !self.text.is_empty() && self.last_flush.is_none_or(|t| now.0 - t.0 >= FLUSH_EVERY)
    }
}

/// µ$ of `usage` at `prices`, rounded up once per turn (llm-api D4).
pub(crate) fn cost(usage: &Usage, prices: &PriceTable) -> u64 {
    let rates = prices.rates_for(usage.prompt());
    let sum = u128::from(usage.input) * u128::from(rates.input)
        + u128::from(usage.output) * u128::from(rates.output)
        + u128::from(usage.cache_read) * u128::from(rates.cache_read)
        + u128::from(usage.cache_write) * u128::from(rates.cache_write);
    u64::try_from(sum.div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

/// How an LLM failure ends the answer (D3).
pub(crate) fn llm_flow(error: LlmError) -> Flow {
    match error {
        LlmError::Auth => Flow::Fail(HostError::NeedsKey),
        other => Flow::Fail(HostError::Llm(other)),
    }
}

impl Worker {
    fn flush(&self, buf: &mut TextBuf, now: Seconds) {
        if buf.text.is_empty() {
            return;
        }
        self.shared.emit_text(std::mem::take(&mut buf.text));
        buf.last_flush = Some(now);
    }

    /// Streams `request`, forwarding text as it arrives, and charges the turn.
    ///
    /// # Errors
    /// [`Flow::Stopped`] after Stop, [`Flow::Fail`] for a failed turn (the partial text is kept), or `Revoked` if the
    /// charge found the session gone.
    pub(crate) fn run_turn(&mut self, run: &mut AnswerRun, request: &TurnRequest, mode: TurnMode) -> Result<TurnDone, Flow> {
        let cancel = self.shared.fresh_cancel();
        let mut stream = match self.llm.stream(request.clone(), &cancel) {
            Ok(stream) => stream,
            Err(LlmError::Cancelled) => return Err(Flow::Stopped),
            Err(error) => return Err(llm_flow(error)),
        };
        if mode == TurnMode::Answer {
            run.counters.turn();
        }
        let mut fold = TurnFold::default();
        let mut buf = TextBuf::new();
        let failure = loop {
            let now = self.cfg.clock.now();
            match stream.next_timeout(STREAM_STEP) {
                Ok(Next::Event(event)) => {
                    if let LlmEvent::TextDelta(text) = &event {
                        buf.text.push_str(text);
                    }
                    if let Err(error) = fold.push(event) {
                        break Some(error);
                    }
                    if mode == TurnMode::Answer && buf.due(now) {
                        self.flush(&mut buf, now);
                    }
                }
                Ok(Next::Wait) => {
                    if mode == TurnMode::Answer && buf.due(now) {
                        self.flush(&mut buf, now);
                    }
                }
                Ok(Next::End) => break None,
                Ok(_) => {}
                Err(error) => break Some(error),
            }
        };
        let usage = stream.usage_so_far();
        let now = self.cfg.clock.now();
        match failure {
            Some(LlmError::Cancelled) => {
                self.charge(run, usage);
                self.keep_partial_text(run);
                Err(Flow::Stopped)
            }
            Some(error) => {
                if mode == TurnMode::Answer {
                    self.flush(&mut buf, now);
                }
                self.charge(run, usage);
                self.keep_partial_text(run);
                Err(llm_flow(error))
            }
            None => {
                if mode == TurnMode::Answer {
                    self.flush(&mut buf, now);
                }
                self.finish_turn(run, request, fold, usage, mode)
            }
        }
    }

    fn finish_turn(
        &mut self,
        run: &mut AnswerRun,
        request: &TurnRequest,
        fold: TurnFold,
        usage: Option<Usage>,
        mode: TurnMode,
    ) -> Result<TurnDone, Flow> {
        let model = fold.model.clone();
        let (blocks, stop) = match fold.finish() {
            Ok(done) => done,
            Err(error) => {
                self.charge(run, usage);
                self.keep_partial_text(run);
                return Err(llm_flow(error));
            }
        };
        let Some(usage) = usage else {
            self.keep_partial_text(run);
            return Err(llm_flow(LlmError::Malformed("the turn stopped without reporting usage".into())));
        };
        self.charge(run, Some(usage));
        if mode == TurnMode::Summary {
            return Ok(TurnDone { blocks, stop });
        }
        self.measured = usage.prompt().saturating_add(usage.output);
        let message = Message::new(Role::Assistant, blocks.clone());
        self.ledger.observe(request, &Turn::new(model, message, usage, stop));
        self.commit_assistant(run, blocks.clone());
        Ok(TurnDone { blocks, stop })
    }

    /// Charges the session once for `usage` and tells the panel the new total (D2.3).
    pub(crate) fn charge(&mut self, run: &mut AnswerRun, usage: Option<Usage>) {
        let Some(usage) = usage else { return };
        let usd_micros = cost(&usage, &self.cfg.prices);
        run.counters.tokens(usage.tokens());
        self.shared.record(Entry::charge(usage, usd_micros));
        let spend = Spend { usd_micros, tokens: usage.tokens(), seconds: 0 };
        // A charge that fails means the session ended; the next gate finds out why.
        if let Ok(info) = self.sessions.charge(self.session, spend) {
            self.shared.emit(AssistantEvent::budget(info.spent, info.budget));
        }
    }

    /// Records the turn's blocks as the assistant's message and entry.
    pub(crate) fn commit_assistant(&mut self, run: &mut AnswerRun, blocks: Vec<ContentBlock>) {
        for block in &blocks {
            if let ContentBlock::Text(text) = block {
                run.text.push_str(text);
                run.text.push('\n');
            }
        }
        self.shared.record_turn(Entry::assistant(blocks.clone()));
        self.messages.push(Message::new(Role::Assistant, blocks));
    }

    /// A turn that did not finish keeps the text the person was shown, and nothing else: a half-built tool call was
    /// never run and a half-thought block cannot be sent back.
    pub(crate) fn keep_partial_text(&mut self, run: &mut AnswerRun) {
        let text = self.shared.lock().live_text.clone();
        if !text.is_empty() {
            self.commit_assistant(run, vec![ContentBlock::text(text)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_llm_api::usage::{PriceTier, Rates};

    use super::*;

    /// The design's section 4 prices for `claude-haiku-5-5`, in µ$ per million tokens.
    pub(crate) fn haiku_prices() -> PriceTable {
        let rates = Rates::new(100_000, 500_000).with_cache_read(10_000).with_cache_write(125_000);
        PriceTable::new("2026-10-01", "design section 4", vec![PriceTier::new(None, rates)])
    }

    #[test]
    fn the_design_example_charges_814_micro_dollars_and_6327_tokens() {
        let usage = Usage::new(4, 61).with_cache_write(6_262);
        assert_eq!(cost(&usage, &haiku_prices()), 814);
        assert_eq!(usage.tokens(), 6_327);
    }

    #[test]
    fn the_section_four_turns_charge_their_rounded_up_amounts() {
        let charged: Vec<u64> = [
            Usage::new(4, 48).with_cache_read(6_262).with_cache_write(447),
            Usage::new(4, 41).with_cache_read(6_709).with_cache_write(118),
            Usage::new(4, 96).with_cache_read(6_827).with_cache_write(1_891),
            Usage::new(4, 40).with_cache_read(8_718).with_cache_write(141),
            Usage::new(4, 18).with_cache_read(8_859).with_cache_write(426),
        ]
        .iter()
        .map(|u| cost(u, &haiku_prices()))
        .collect();
        assert_eq!(charged, [143, 103, 354, 126, 152]);
    }
}
