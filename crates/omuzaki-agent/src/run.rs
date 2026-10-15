//! One answer: everything the host does for one user message (D2).

use omuzaki_agent_api::entry::{Entry, SessionNote};
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{ContentBlock, ToolChoice};
use omuzaki_llm_api::stream::StopReason;
use omuzaki_ops_api::{EndReason, PauseReason, SessionState, SessionTx, Spend};

use crate::calls::tool_calls;
use crate::caps::{Counters, limit_note};
use crate::store::cut_rationale;
use crate::turn::TurnMode;
use crate::tx;
use crate::worker::Worker;

/// Why an answer ends other than by finishing (the error side of the loop's `?`).
#[derive(Debug)]
pub(crate) enum Flow {
    /// The person pressed Stop (D4).
    Stopped,
    /// The session ended: revoked, closed or over.
    Revoked(EndReason),
    /// Something failed; the person sees the error and the answer ends (D3).
    Fail(HostError),
}

/// The state of the answer in progress.
// The flags are independent facts of one answer (a transaction is open, the last turn is being made, a continue was
// sent, the history was compacted); none implies another.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct AnswerRun {
    pub counters: Counters,
    /// The first line of the person's message: the transaction's label and cause.
    pub first_line: String,
    /// The answer's transaction, once opened.
    pub tx: Option<SessionTx>,
    pub tx_open: bool,
    /// What the model wrote during the answer: the rationale for Explain this.
    pub text: String,
    /// A cap was reached: the last turn is being made.
    pub final_turn: bool,
    /// The turn ended at `max_output` and was continued once.
    pub continued: bool,
    /// Where the answer starts in the model's history.
    pub msg_from: usize,
    /// The conversation was compacted during this answer.
    pub compacted: bool,
}

impl Worker {
    /// Answers one user message, start to `Done`.
    pub(crate) fn answer(&mut self, msg: &UserMessage) {
        let mut run = AnswerRun {
            counters: Counters::new(self.now()),
            first_line: tx::first_line(&msg.text).to_owned(),
            tx: None,
            tx_open: false,
            text: String::new(),
            final_turn: false,
            continued: false,
            msg_from: self.messages.len(),
            compacted: false,
        };
        let result = match self.drive(&mut run, msg) {
            // Stop pressed as the answer was finishing still stops it: the queue was already cut.
            Ok(()) | Err(Flow::Fail(_)) if self.shared.stopping() => Err(Flow::Stopped),
            other => other,
        };
        self.conclude(&mut run, result);
    }

    fn drive(&mut self, run: &mut AnswerRun, msg: &UserMessage) -> Result<(), Flow> {
        self.ensure_tools()?;
        self.admit(msg);
        loop {
            self.gate(run)?;
            if !run.final_turn
                && let Some(cap) = run.counters.reached(&self.cfg.caps, self.now())
            {
                self.shared.emit(AssistantEvent::Limit(cap));
                self.push_user(vec![ContentBlock::text(limit_note(cap, &self.cfg.caps))]);
                run.final_turn = true;
            }
            let choice = if run.final_turn { ToolChoice::None } else { ToolChoice::Auto };
            self.ensure_tools()?;
            self.check_model()?;
            if self.wants_compaction() {
                self.compact(run)?;
            }
            let request = self.build_request(choice.clone());
            self.reserve_budget(run, &request)?;
            let done = match self.run_turn(run, &request, TurnMode::Answer) {
                // The prompt did not fit after all: compact and try the turn once more (D3).
                Err(Flow::Fail(HostError::Llm(LlmError::ContextTooLong { .. }))) if !run.compacted => {
                    self.compact(run)?;
                    continue;
                }
                Err(Flow::Fail(HostError::Llm(LlmError::ContextTooLong { .. }))) => {
                    return Err(Flow::Fail(HostError::ContextFull));
                }
                other => other?,
            };
            let ended_by_cap = run.final_turn;
            match done.stop {
                StopReason::ToolUse if !ended_by_cap => {
                    run.continued = false;
                    self.run_calls(run, &done.blocks)?;
                }
                StopReason::ToolUse => {
                    // The last turn was told not to call tools; answer any it called so the history stays valid.
                    let unanswered = tool_calls(&done.blocks)
                        .into_iter()
                        .map(|c| {
                            ContentBlock::tool_error(
                                c.id,
                                vec![ContentBlock::text("not run: the limit for this message was reached")],
                            )
                        })
                        .collect();
                    self.push_user(unanswered);
                    return Ok(());
                }
                StopReason::Paused if !ended_by_cap => run.continued = false,
                StopReason::MaxOutput if !run.continued && !ended_by_cap => {
                    run.continued = true;
                    self.push_user(vec![ContentBlock::text("continue")]);
                }
                _ => return Ok(()),
            }
        }
    }

    /// D2.1: a session paused by the person is resumed by their next message. Records the message.
    fn admit(&mut self, msg: &UserMessage) {
        if let Some(info) = self.session_info()
            && matches!(info.state, SessionState::Paused { reason: PauseReason::Person, .. })
        {
            // If the budget still blocks it, the gate waits for the person instead.
            let _ = self.sessions.resume(self.session, &self.cfg.principal);
        }
        self.shared.record(Entry::user(msg.text.clone(), msg.chips.clone()));
        let blocks = self.user_blocks(msg);
        self.push_user(blocks);
    }

    /// Fetches `tools/list` the first time.
    fn ensure_tools(&mut self) -> Result<(), Flow> {
        self.tool_list().map(|_| ()).map_err(|e| if self.shared.stopping() { Flow::Stopped } else { Flow::Fail(e) })
    }

    /// Asks the executor whether the turn's bound fits the budget; if not, waits for the person (D2.2).
    pub(crate) fn reserve_budget(
        &mut self,
        run: &mut AnswerRun,
        request: &omuzaki_llm_api::message::TurnRequest,
    ) -> Result<(), Flow> {
        loop {
            let bound = self.ledger.bound(request, self.llm.info(), &self.cfg.prices);
            let prompt = self.ledger.prompt_estimate(request, self.llm.info());
            let needed =
                Spend { usd_micros: bound.0, tokens: prompt.saturating_add(u64::from(request.max_output)), seconds: 0 };
            match self.sessions.request_budget(self.session, needed) {
                Ok(()) => return Ok(()),
                Err(error) => match error.detail.as_deref() {
                    Some(omuzaki_ops_api::ErrorDetail::Queued { approval, need }) => {
                        let approval = *approval;
                        self.shared.record(Entry::approval(approval, need.clone(), None));
                        self.shared.emit(AssistantEvent::waiting(approval));
                        let reply = self.wait_approval(run, approval)?;
                        if reply.is_error {
                            return Err(Flow::Fail(HostError::Mcp(crate::content::plain_text(&reply.blocks))));
                        }
                        self.shared.emit(AssistantEvent::Resumed);
                    }
                    // An executor with no budgets lets the turn go.
                    _ if error.code == omuzaki_ops_api::ErrorCode::Unsupported => return Ok(()),
                    _ => {
                        return Err(self
                            .ended()
                            .map_or_else(|| Flow::Fail(HostError::Mcp(error.message.clone())), Flow::Revoked));
                    }
                },
            }
        }
    }

    /// How an answer ends (D2.8): commit, write the rationale, say `Done`; or hand over to Stop's ending.
    fn conclude(&mut self, run: &mut AnswerRun, result: Result<(), Flow>) {
        match result {
            Ok(()) => self.finish_committed(run),
            Err(Flow::Fail(error)) => {
                self.shared.emit(AssistantEvent::Error(error));
                self.finish_committed(run);
            }
            Err(Flow::Stopped) => self.finish_stopped(run),
            Err(Flow::Revoked(reason)) => self.finish_revoked(run, reason),
        }
    }

    fn finish_committed(&mut self, run: &mut AnswerRun) {
        self.commit_tx(run);
        self.write_step(run, "");
        self.done();
    }

    pub(crate) fn finish_revoked(&mut self, run: &mut AnswerRun, reason: EndReason) {
        // The executor rolled the open transaction back when it ended the session.
        run.tx_open = false;
        self.shared.record(Entry::session(self.session, SessionNote::Ended(reason)));
        self.shared.mark_ended();
        self.shared.emit(AssistantEvent::Error(HostError::SessionEnded));
        self.done();
    }

    /// Records the answer's step with the model's text as its rationale (D7).
    pub(crate) fn write_step(&mut self, run: &AnswerRun, suffix: &str) {
        let Some(tx) = &run.tx else { return };
        let label = format!("{}{suffix}", tx.label);
        self.shared.record(Entry::step(tx.tx, label, cut_rationale(run.text.trim())));
    }

    /// The answer is over: new messages are accepted again, then `Done`.
    pub(crate) fn done(&mut self) {
        self.shared.lock().running = false;
        self.shared.emit(AssistantEvent::Done);
    }
}
