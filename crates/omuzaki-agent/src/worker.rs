//! The host thread's state and the checks it makes between steps.

use std::sync::{Arc, Mutex};

use omuzaki_agent_api::event::{AssistantEvent, PlanItem};
use omuzaki_core::time::Seconds;
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::message::{ContentBlock, Message, Role};
use omuzaki_llm_api::usage::Ledger;
use omuzaki_ops_api::{EndReason, SessionId, SessionInfo, SessionState, Sessions};

use crate::config::HostConfig;
use crate::link::{McpClientLink, ToolInfo};
use crate::run::{AnswerRun, Flow};
use crate::shared::Shared;
use crate::wait;

/// How often a paused session is looked at (D2.6).
const PAUSE_POLL: f64 = 0.25;

/// A call's result that arrived after Stop (D4): shown to the model in the next message's context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LateResult {
    pub tool: String,
    pub outcome: String,
}

/// Everything the host thread owns.
pub(crate) struct Worker {
    pub shared: Arc<Shared>,
    pub link: McpClientLink,
    pub llm: Arc<dyn LlmClient>,
    pub sessions: Arc<dyn Sessions>,
    pub session: SessionId,
    pub cfg: HostConfig,
    /// The model's history, append-only (a stopped answer is cut once, at Stop).
    pub messages: Vec<Message>,
    pub ledger: Ledger,
    /// The server's `tools/list`, fetched once.
    pub tools: Option<Vec<ToolInfo>>,
    pub late: Arc<Mutex<Vec<LateResult>>>,
    pub plan: Vec<PlanItem>,
    /// Tokens of the last turn's prompt and answer: a lower bound of the next prompt, for the 75 % compaction trigger.
    pub measured: u64,
    /// The last `project.describe` text the model was given, so it is repeated only when it changed.
    pub last_describe: Option<String>,
    /// The model was checked for the assistant (window and prefix).
    pub model_checked: bool,
    pub prompt_fetch: crate::prompts::PromptFetch,
    /// An answer just ended: the prompt list is refetched (§7).
    pub refetch_after_answer: bool,
    /// An answer ended without the executor confirming its transaction closed (a refused commit, a failed Stop). The
    /// transaction is still the host's to close before the next message is admitted; this is what that answer's model
    /// wrote, the rationale of the step it will become.
    pub tx_unconfirmed: Option<String>,
}

impl Worker {
    /// Appends a user message; consecutive user messages are one message, because providers want the roles to alternate.
    pub(crate) fn push_user(&mut self, mut blocks: Vec<ContentBlock>) {
        match self.messages.last_mut() {
            Some(last) if last.role == Role::User => last.content.append(&mut blocks),
            _ => self.messages.push(Message::new(Role::User, blocks)),
        }
    }

    pub(crate) fn now(&self) -> Seconds {
        self.cfg.clock.now()
    }

    /// The session as the executor sees it now.
    pub(crate) fn session_info(&self) -> Option<SessionInfo> {
        self.sessions.session(self.session)
    }

    /// Why the session ended, if it has.
    pub(crate) fn ended(&self) -> Option<EndReason> {
        match self.session_info() {
            Some(SessionInfo { state: SessionState::Ended { reason, .. }, .. }) => Some(reason),
            Some(_) => None,
            None => Some(EndReason::Closed),
        }
    }

    /// Fails with [`Flow::Stopped`] once Stop has been pressed.
    pub(crate) fn check_stop(&self) -> Result<(), Flow> {
        if self.shared.stopping() { Err(Flow::Stopped) } else { Ok(()) }
    }

    /// Checked before each turn and call: Stop, a revoked session, and a pause (D4). A pause finishes nothing and keeps
    /// the transaction open; the wait ends when the session is active again.
    pub(crate) fn gate(&mut self, run: &mut AnswerRun) -> Result<(), Flow> {
        self.check_stop()?;
        match self.session_info().map(|i| i.state) {
            None => Err(Flow::Revoked(EndReason::Closed)),
            Some(SessionState::Ended { reason, .. }) => Err(Flow::Revoked(reason)),
            Some(SessionState::Active) => Ok(()),
            Some(SessionState::Paused { .. }) => {
                self.shared.emit(AssistantEvent::Paused);
                self.wait_until_active(run)?;
                self.shared.emit(AssistantEvent::Resumed);
                Ok(())
            }
        }
    }

    /// Polls a paused session every 250 ms until it is active; the wait is not wall time.
    pub(crate) fn wait_until_active(&mut self, run: &mut AnswerRun) -> Result<(), Flow> {
        let started = self.now();
        let result = loop {
            self.check_stop()?;
            match self.session_info().map(|i| i.state) {
                Some(SessionState::Active) => break Ok(()),
                Some(SessionState::Ended { reason, .. }) => break Err(Flow::Revoked(reason)),
                None => break Err(Flow::Revoked(EndReason::Closed)),
                Some(SessionState::Paused { .. }) => {
                    wait::for_seconds(self.cfg.clock.as_ref(), PAUSE_POLL, || self.shared.stopping());
                }
            }
        };
        run.counters.exclude_seconds(self.now().0 - started.0);
        result
    }
}
