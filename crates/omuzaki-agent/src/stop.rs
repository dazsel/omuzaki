//! Stop, as the host thread finishes it (D4). The synchronous half (the queue's withdrawal and the cancel flag) is in
//! [`AssistantHost::stop`](crate::AssistantHost); this is the rest, in the design's order.

use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_ops_api::StopStep;

use crate::cut::cut_history;
use crate::history::Unresolved;
use crate::run::AnswerRun;
use crate::wait;
use crate::worker::Worker;

/// How often the executor is asked whether the stopped step has closed (D4).
const CLOSE_POLL: f64 = 0.05;

impl Worker {
    /// Cuts the answer's text in the model's history to what the person had read, so the next request agrees with the
    /// panel and the transcript.
    fn cut_history(&mut self, run: &AnswerRun, read: usize) {
        cut_history(&mut self.messages, run.msg_from, read, run.compacted_text);
    }

    /// After Stop: charge is done, the stream and the call are down. The executor pauses the session, cancels its jobs,
    /// reverts what they did and commits what is left; the host only asks, once, and reports (D4 steps 6).
    pub(crate) fn finish_stopped(&mut self, run: &mut AnswerRun) {
        if self.shared.lock().shutdown {
            // The host is being dropped: ending the session rolls the transaction back, which is what was asked.
            self.done();
            return;
        }
        let read = self.shared.lock().stop_text_read.unwrap_or(0);
        self.cut_history(run, read);
        self.shared.record(Entry::stopped(Some(read)));
        match self.sessions.stop(self.session, &self.cfg.principal) {
            Ok(stopped) => {
                self.shared.emit(AssistantEvent::Paused);
                // The executor owns the transaction once it has accepted the Stop.
                run.tx_open = false;
                if stopped.step == StopStep::Pending {
                    self.wait_step_closed();
                }
                self.record_stopped_step(run, stopped.step);
            }
            Err(error) => {
                // Never ignored: the person is told, and the host closes the transaction itself before `Done`.
                self.shared.emit(AssistantEvent::Error(HostError::Mcp(format!("Stop was refused: {}", error.message))));
                self.close_after_refused_stop(run);
            }
        }
        self.done();
    }

    /// Records the undo step of the stopped answer's transaction, if the executor made one.
    fn record_stopped_step(&mut self, run: &mut AnswerRun, step: StopStep) {
        let Some(tx) = run.tx.clone() else { return };
        let confirmed = match step {
            StopStep::Committed(_) => true,
            // The executor closed it by itself, or the commit was already on its way when Stop cancelled the call and
            // the executor applied it first. Only a transaction it committed made an undo step; one reverted to
            // nothing did not. The server's log says which, by the transaction's id.
            StopStep::Pending => self.tx_committed(tx.tx),
            _ if run.commit_sent => self.tx_committed(tx.tx),
            _ => return,
        };
        if confirmed {
            self.write_step(run, " (stopped)");
        } else {
            // The log may only be late: the outcome is asked again before the next message.
            self.unresolved.push(Unresolved { tx, rationale: run.text.clone(), suffix: " (stopped)" });
        }
    }

    /// The executor refused the Stop, so the transaction is still the host's. It is closed here, before `Done`, the way
    /// the executor's Stop would have: committed as a step, else discarded. If neither works the person is told and the
    /// next message tries again (`close_prior_tx`).
    fn close_after_refused_stop(&mut self, run: &mut AnswerRun) {
        if self.ended().is_some() {
            // A session that has ended rolled its transaction back.
            run.tx_open = false;
            return;
        }
        if !run.tx_open {
            return;
        }
        if run.tx.is_none() {
            return;
        }
        if self.session_info().is_some_and(|info| info.open_tx.is_none()) {
            // Closed meanwhile (a commit that was already on its way): the log says whether it made a step.
            run.tx_open = false;
            if run.commit_sent {
                self.record_stopped_step(run, StopStep::None);
            }
            return;
        }
        let closed = |reply: Option<crate::link::ToolReply>| reply.is_some_and(|r| !r.is_error);
        if closed(self.call_after_stop("tx_commit")) {
            run.tx_open = false;
            self.write_step(run, " (stopped)");
        } else if closed(self.call_after_stop("tx_rollback")) {
            run.tx_open = false;
            run.tx = None;
            self.shared.emit(AssistantEvent::Error(HostError::Mcp(
                "the changes of this answer could not be kept as an undo step and were discarded".into(),
            )));
        } else {
            self.shared.emit(AssistantEvent::Error(HostError::Mcp(
                "the changes of this answer are still open; they will be closed with your next message".into(),
            )));
        }
    }

    /// Polls the session every 50 ms until its open transaction is closed: a joined driver job is still reverting its
    /// edits and the executor commits the rest when it has ended.
    fn wait_step_closed(&self) {
        loop {
            match self.session_info() {
                Some(info)
                    if info.open_tx.is_some() && !matches!(info.state, omuzaki_ops_api::SessionState::Ended { .. }) =>
                {
                    wait::for_seconds(self.cfg.clock.as_ref(), CLOSE_POLL, || false);
                }
                _ => return,
            }
            if self.shared.lock().shutdown {
                return;
            }
        }
    }
}
