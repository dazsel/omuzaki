//! Stop, as the host thread finishes it (D4). The synchronous half (the queue's withdrawal and the cancel flag) is in
//! [`AssistantHost::stop`](crate::AssistantHost); this is the rest, in the design's order.

use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_llm_api::message::Role;
use omuzaki_ops_api::StopStep;

use crate::run::AnswerRun;
use crate::shared::cut_text_blocks;
use crate::wait;
use crate::worker::Worker;

/// How often the executor is asked whether the stopped step has closed (D4).
const CLOSE_POLL: f64 = 0.05;

impl Worker {
    /// Cuts the answer's text in the model's history to what the person had read, so the next request agrees with the
    /// panel and the transcript.
    fn cut_history(&mut self, run: &AnswerRun, read: usize) {
        // `msg_from` is rebased when the history is compacted, but a stale one must never index past the end.
        let turns =
            self.messages.iter_mut().skip(run.msg_from).filter(|m| m.role == Role::Assistant).map(|m| &mut m.content);
        // Stop counts the answer's text from its first byte; what compaction folded into the summary is not in `turns`.
        cut_text_blocks(turns, read.saturating_sub(run.compacted_text));
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
        let step = match self.sessions.stop(self.session, &self.cfg.principal) {
            Ok(stopped) => {
                self.shared.emit(AssistantEvent::Paused);
                Some(stopped.step)
            }
            Err(error) => {
                // Never ignored: the person is told. A session that has ended rolled its transaction back; otherwise
                // the transaction is still open and the host closes it before the next message (`close_prior_tx`).
                self.shared.emit(AssistantEvent::Error(HostError::Mcp(format!("Stop was refused: {}", error.message))));
                if self.ended().is_some() {
                    run.tx_open = false;
                }
                None
            }
        };
        // The executor owns the transaction once it has accepted the Stop. If it did not, the transaction is still
        // ours and stays tracked for the next answer.
        if step.is_some() {
            run.tx_open = false;
        }
        if step == Some(StopStep::Pending) {
            self.wait_step_closed();
        }
        if let Some(tx) = &run.tx {
            let committed = match step {
                Some(StopStep::Committed(id)) => Some(id),
                // The executor closed it by itself: only a transaction it committed made an undo step. One it reverted
                // to nothing (every edit undone) did not. The server's history says which, by the transaction's id.
                Some(StopStep::Pending) => self.tx_in_history(tx.tx).then_some(tx.tx),
                // The commit was already on its way when Stop cancelled the call, and the executor applied it first:
                // the transaction is closed as a real undo step the host never saw confirmed.
                _ if run.commit_sent => self.tx_in_history(tx.tx).then_some(tx.tx),
                _ => None,
            };
            if committed.is_some() {
                self.write_step(run, " (stopped)");
            }
        }
        self.done();
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
