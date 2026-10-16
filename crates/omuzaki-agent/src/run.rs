//! One answer: everything the host does for one user message (D2).

use omuzaki_agent_api::entry::{AnswerId, Entry, SessionNote};
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{ContentBlock, ToolChoice};
use omuzaki_llm_api::stream::StopReason;
use omuzaki_ops_api::{EndReason, PauseReason, SessionState, SessionTx, Spend};

use crate::calls::tool_calls;
use crate::caps::{Counters, limit_note};
use crate::context::person_text;
use crate::store::cut_rationale;
use crate::turn::TurnMode;
use crate::tx::{self, Close, Closed, TxState};
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
    /// The previous answer's transaction could still not be closed, so this message did not run (`tx.rs`).
    Unclosed,
}

/// The state of the answer in progress.
// The flags are independent facts of one answer (a transaction is open, the last turn is being made, a continue was
// sent, the history was compacted); none implies another.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct AnswerRun {
    /// Which answer of the conversation this is; its `User` and `Stopped` entries name it.
    pub answer: AnswerId,
    pub counters: Counters,
    /// The first line of the person's message: the transaction's label and cause.
    pub first_line: String,
    /// The message went to the model with its context block.
    pub admitted: bool,
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
    /// Bytes of the answer's text that compaction folded into the summary, so are no longer in the history.
    pub compacted_text: usize,
}

impl Worker {
    /// Answers one user message, start to its end (`Done`, or `Unclosed`).
    pub(crate) fn answer(&mut self, msg: &UserMessage) {
        self.answers += 1;
        let answer = AnswerId(self.answers);
        let mut run = AnswerRun {
            answer,
            counters: Counters::new(self.now()),
            first_line: tx::first_line(&msg.text).to_owned(),
            admitted: false,
            text: String::new(),
            final_turn: false,
            continued: false,
            msg_from: self.messages.len(),
            compacted: false,
            compacted_text: 0,
        };
        // First, so that everything this answer records (Stop's cutoff included, from whatever phase) follows the message
        // it answers and names it.
        self.shared.record(Entry::user(msg.text.clone(), msg.chips.clone()).in_answer(answer));
        let result = match self.drive(&mut run, msg) {
            // Stop pressed as the answer was finishing still stops it: the queue was already cut.
            Ok(()) | Err(Flow::Fail(_) | Flow::Unclosed) if self.shared.stopping() => Err(Flow::Stopped),
            other => other,
        };
        if !run.admitted {
            // The model's history holds the message as a continued conversation rebuilds it, so the two agree.
            self.push_user(vec![ContentBlock::text(person_text(&msg.text, &msg.chips))]);
        }
        self.conclude(&mut run, result);
    }

    fn drive(&mut self, run: &mut AnswerRun, msg: &UserMessage) -> Result<(), Flow> {
        self.close_prior_tx()?;
        // Outcomes the log has shown since the last answer; what others changed meanwhile joins this message's context.
        self.reconcile_now();
        self.plan.clear();
        self.ensure_tools()?;
        self.admit(run, msg);
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

    /// D2.1: a session paused by the person is resumed by their next message. Gives the model the message with its
    /// context block.
    fn admit(&mut self, run: &mut AnswerRun, msg: &UserMessage) {
        if let Some(info) = self.session_info()
            && matches!(info.state, SessionState::Paused { reason: PauseReason::Person, .. })
        {
            // If the budget still blocks it, the gate waits for the person instead.
            let _ = self.sessions.resume(self.session, &self.cfg.principal);
        }
        let blocks = self.user_blocks(msg, run.answer);
        self.push_user(blocks);
        run.admitted = true;
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

    /// One message, one undo step: a transaction an earlier answer left Unclosed is closed here, before this message
    /// runs, so this answer never joins it: committed as its own step with its own rationale, else discarded (the person
    /// is told). If the executor refuses both, this message does not run and ends `Unclosed` too. Stop cuts these
    /// closes short: the transaction stays Closing with its own rationale, and Stop's ending closes it.
    fn close_prior_tx(&mut self) -> Result<(), Flow> {
        if !self.tx.as_ref().is_some_and(|t| t.state == TxState::Unclosed) {
            return Ok(());
        }
        if self.ended().is_some() {
            self.settle_ended_session();
            return Ok(());
        }
        match self.close_in_order(&[Close::Commit, Close::Rollback]) {
            Closed::Committed | Closed::Retained => Ok(()),
            Closed::Interrupted => Err(Flow::Stopped),
            Closed::RolledBack => {
                self.shared.emit(AssistantEvent::Error(HostError::Mcp(
                    "the changes of the previous answer could not be kept as an undo step and were discarded".into(),
                )));
                Ok(())
            }
            Closed::Unclosed => {
                self.shared.emit(AssistantEvent::Error(HostError::Mcp(
                    "the changes of the previous answer could neither be committed nor discarded; they are still open"
                        .into(),
                )));
                Err(Flow::Unclosed)
            }
        }
    }

    /// How an answer ends (D2.8): commit and say `Done`; or hand over to Stop's ending, or the session's end.
    fn conclude(&mut self, run: &mut AnswerRun, result: Result<(), Flow>) {
        self.settle_pending();
        // What the model wrote is the rationale of the answer's step, however the transaction closes.
        if let Some(t) = self.tx.as_mut()
            && t.answer == run.answer
        {
            t.rationale.clone_from(&run.text);
        }
        match result {
            Ok(()) => self.finish_committed(run),
            Err(Flow::Fail(error)) => {
                self.shared.emit(AssistantEvent::Error(error));
                self.finish_committed(run);
            }
            Err(Flow::Stopped) => self.finish_stopped(run),
            Err(Flow::Revoked(reason)) => self.finish_revoked(reason),
            Err(Flow::Unclosed) => self.end(run),
        }
    }

    fn finish_committed(&mut self, run: &mut AnswerRun) {
        // Stop pressed while the answer was concluding still stops it.
        if self.shared.stopping() {
            return self.finish_stopped(run);
        }
        match self.commit_at_end() {
            Ok(()) | Err(Flow::Unclosed) => self.end(run),
            Err(Flow::Stopped) => self.finish_stopped(run),
            Err(Flow::Revoked(reason)) => self.finish_revoked(reason),
            Err(Flow::Fail(error)) => {
                self.shared.emit(AssistantEvent::Error(error));
                self.settle_after_failed_close();
                self.end(run);
            }
        }
    }

    pub(crate) fn finish_revoked(&mut self, reason: EndReason) {
        self.settle_ended_session();
        self.shared.record(Entry::session(self.session, SessionNote::Ended(reason)));
        self.shared.mark_ended();
        self.shared.emit(AssistantEvent::Error(HostError::SessionEnded));
        self.finish();
    }

    /// Records the step of transaction `tx`, with `rationale` as the text of "Explain this".
    pub(crate) fn record_step(&mut self, tx: &SessionTx, rationale: &str, suffix: &str) {
        let label = format!("{}{suffix}", tx.label);
        self.shared.record(Entry::step(tx.tx, label, cut_rationale(rationale.trim())));
    }

    /// The answer is over, unless Stop was pressed since the answer last looked: then Stop's ending runs first (the
    /// executor is asked to stop, the cutoff is recorded). Deciding and ending the answer happen under one lock, so a
    /// Stop either lands before this and is honoured, or finds the answer over.
    fn end(&mut self, run: &mut AnswerRun) {
        self.settle_pending();
        let ending = self.ending_event();
        {
            let mut ctl = self.shared.lock();
            if !ctl.stopping {
                ctl.running = false;
                // Lock order: control, then the queue's.
                self.shared.hub.send(ending);
                return;
            }
        }
        self.finish_stopped(run);
    }

    /// The answer is over: new messages are accepted again, and its end is sent, in one lock section.
    pub(crate) fn finish(&mut self) {
        self.settle_pending();
        let ending = self.ending_event();
        let mut ctl = self.shared.lock();
        ctl.running = false;
        self.shared.hub.send(ending);
    }

    /// How the answer ends: `Done` when it leaves no transaction the executor may still hold (none, or one the
    /// executor confirmed closed); otherwise `Unclosed`, with the transaction marked so the next message closes it
    /// first. The executor is asked too, so a transaction no path tracked can never pass as closed; and one still Open
    /// or Closing here would be a path that forgot to settle it. Both are reported the same way, never as `Done`.
    fn ending_event(&mut self) -> AssistantEvent {
        if !self.shared.lock().shutdown {
            self.adopt_open_tx(TxState::Unclosed);
        }
        match self.tx.as_mut() {
            None => AssistantEvent::Done,
            Some(t) => {
                t.state = TxState::Unclosed;
                AssistantEvent::Unclosed
            }
        }
    }
}
