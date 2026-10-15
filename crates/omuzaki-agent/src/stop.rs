//! Stop, as the host thread finishes it (D4). The synchronous half (the queue's withdrawal and the cancel flag) is in
//! [`AssistantHost::stop`](crate::AssistantHost); this is the rest, in the design's order.

use omuzaki_agent_api::entry::Entry;
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
        let turns = self.messages[run.msg_from..].iter_mut().filter(|m| m.role == Role::Assistant).map(|m| &mut m.content);
        cut_text_blocks(turns, read);
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
        self.shared.record(Entry::Stopped);
        let step = match self.sessions.stop(self.session, &self.cfg.principal) {
            Ok(stopped) => Some(stopped.step),
            Err(_) => None,
        };
        self.shared.emit(AssistantEvent::Paused);
        run.tx_open = false;
        if step == Some(StopStep::Pending) {
            self.wait_step_closed();
        }
        if let Some(tx) = &run.tx {
            let committed = match step {
                Some(StopStep::Committed(id)) => Some(id),
                Some(StopStep::Pending) => Some(tx.tx),
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
                Some(info) if info.open_tx.is_some() && !matches!(info.state, omuzaki_ops_api::SessionState::Ended { .. }) => {
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
