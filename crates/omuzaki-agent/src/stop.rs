//! Stop, as the host thread finishes it (D4). The synchronous half (the queue's withdrawal and the cancel flag) is in
//! [`AssistantHost::stop`](crate::AssistantHost); this is the rest, in the design's order.
//!
//! Stop's ending moves the tracked transaction through the transitions of `tx.rs` and nothing else: the executor's
//! answer to `Sessions::stop` confirms a commit (`Committed(id)`), or the host waits until the executor no longer holds
//! the transaction (`Pending`, D4's 50 ms poll) and retains it for the log; a Stop the executor refuses leaves it
//! Unclosed, for the next message to close. Stop's ending makes no MCP call at all, so `Done` follows the executor's
//! answer, and the step a lagging log confirms is recorded later.

use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_ops_api::StopStep;

use crate::cut::cut_history;
use crate::run::AnswerRun;
use crate::tx::{STOPPED_SUFFIX, TxState};
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

    /// After Stop: the stream and the call are down. The cutoff is recorded for this answer, the executor is asked once
    /// to stop (it pauses the session, cancels its jobs, reverts what they did and commits what is left), and the
    /// transaction is settled (D4 step 6). Then the answer ends.
    pub(crate) fn finish_stopped(&mut self, run: &mut AnswerRun) {
        let read = self.shared.lock().stop_text_read.unwrap_or(0);
        // Its own answer: the `User` entry of this answer was recorded first, and the cutoff names it. Stored before the
        // history is cut, and also when the host is being dropped: `stop()` has cut the transcript already, and a
        // reopened host reads the cut from here.
        self.shared.record(Entry::stopped(Some(read)).in_answer(run.answer));
        if self.shared.lock().shutdown {
            self.shut_down_tx();
            self.finish();
            return;
        }
        self.cut_history(run, read);
        self.adopt_open_tx(TxState::Closing);
        if let Some(t) = self.tx.as_mut() {
            // Whatever it was (the answer's, or an earlier one's still Unclosed), the Stop now closes it.
            t.state = TxState::Closing;
            t.suffix = STOPPED_SUFFIX;
            if t.answer == run.answer {
                t.rationale.clone_from(&run.text);
            }
        }
        match self.sessions.stop(self.session, &self.cfg.principal) {
            Ok(stopped) => {
                self.shared.emit(AssistantEvent::Paused);
                self.settle_stopped(stopped.step);
            }
            Err(error) => {
                // Never ignored: the person is told, and the host closes the transaction itself before the answer ends.
                self.shared.emit(AssistantEvent::Error(HostError::Mcp(format!("Stop was refused: {}", error.message))));
                self.close_after_refused_stop();
            }
        }
        self.finish();
    }

    /// The host is being dropped: ending the session rolls an open transaction back, which is what was asked. One whose
    /// close was sent may have committed first; it is retained (stored), so the host that reopens this conversation
    /// reads its outcome.
    fn shut_down_tx(&mut self) {
        match self.tx.as_ref().map(|t| t.state) {
            Some(TxState::Closing) => self.retain_tx(),
            _ => self.tx = None,
        }
    }

    /// Settles the tracked transaction by what the executor's Stop answered.
    fn settle_stopped(&mut self, step: StopStep) {
        let Some(t) = self.tx.as_ref() else { return };
        match step {
            StopStep::Committed(id) if id == t.tx.tx => self.tx_committed_now(),
            StopStep::Pending => {
                self.wait_step_closed();
                self.settle_or_close_self();
            }
            _ => self.settle_or_close_self(),
        }
    }

    /// The executor has closed it (Retained, for the log to tell its outcome), or says it closed nothing yet still holds
    /// it: then the host closes it as after a refused Stop.
    fn settle_or_close_self(&mut self) {
        if self.shared.lock().shutdown {
            self.shut_down_tx();
        } else if self.executor_holds_tx() {
            self.close_after_refused_stop();
        } else {
            self.retain_tx();
        }
    }

    /// The executor refused the Stop (or says it closed nothing, yet still holds the transaction): the transaction is
    /// still the host's, and Stop's ending makes no call that could wait on the server. It is Unclosed at once: the person
    /// is told, the answer ends `Unclosed`, and the next message closes it before anything else. A session that has
    /// ended rolled it back already.
    fn close_after_refused_stop(&mut self) {
        if self.tx.is_none() {
            return;
        }
        if self.ended().is_some() {
            self.settle_ended_session();
            return;
        }
        if let Some(t) = self.tx.as_mut() {
            t.state = TxState::Unclosed;
        }
        self.shared.emit(AssistantEvent::Error(HostError::Mcp(
            "the changes of this answer are still open; your next message closes them first".into(),
        )));
    }

    /// Polls the session every 50 ms until it no longer holds the tracked transaction: a joined driver job is still
    /// reverting its edits and the executor commits the rest when it has ended. No give-up (D4).
    fn wait_step_closed(&self) {
        let Some(tracked) = self.tx.as_ref().map(|t| t.tx.tx) else { return };
        loop {
            // One look at the session per poll.
            let held = self.session_info().is_some_and(|info| {
                !matches!(info.state, omuzaki_ops_api::SessionState::Ended { .. })
                    && info.open_tx.is_some_and(|open| open.tx == tracked)
            });
            if !held || self.shared.lock().shutdown {
                return;
            }
            wait::for_seconds(self.cfg.clock.as_ref(), CLOSE_POLL, || false);
        }
    }
}
