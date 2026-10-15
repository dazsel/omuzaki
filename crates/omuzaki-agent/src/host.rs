//! [`AssistantHost`]: the assistant's host thread and the panel-facing handle (D1).

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::entry::{Entry, SessionNote};
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::QuestionId;
use omuzaki_agent_api::hub::EventReceiver;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_agent_api::prompts::PromptsState;
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::usage::Ledger;
use omuzaki_ops_api::{EndReason, SessionId, SessionState, Sessions};

use crate::config::HostConfig;
use crate::link::{LinkError, McpClientLink};
use crate::prompts::{PROMPT_TIMEOUT_S, PromptFetch, check_prompt_text, prompt_arguments};
use crate::resume::messages_from_entries_with;
use crate::shared::{Shared, cut_text_blocks};
use crate::worker::Worker;

/// How long the host thread waits for a command before it looks at the prompt cache again (real time).
const TICK: Duration = Duration::from_millis(2);

enum Cmd {
    Send(UserMessage),
    Shutdown,
}

/// The in-app assistant: one host thread per conversation, started by [`start`](AssistantHost::start). The panel sees it
/// as `dyn Assistant`. Dropping it ends the session with [`EndReason::Closed`] if it is still active.
pub struct AssistantHost {
    shared: Arc<Shared>,
    link: McpClientLink,
    commands: Mutex<Option<Sender<Cmd>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl AssistantHost {
    /// Starts the host thread. Does no I/O on the caller. `link` is the in-process MCP link the app built (its session is
    /// `session`, opened with `role = "assistant"` and the studio's person as principal); `llm` is the model;
    /// `sessions` charges and controls the session.
    pub fn start(
        link: McpClientLink,
        llm: Arc<dyn LlmClient>,
        sessions: Arc<dyn Sessions>,
        session: SessionId,
        cfg: HostConfig,
    ) -> Self {
        let shared = Arc::new(Shared::new(
            Arc::clone(&cfg.clock),
            Arc::clone(&cfg.sink),
            Arc::clone(&sessions),
            session,
            cfg.history.clone(),
        ));
        let messages = messages_from_entries_with(&cfg.history, cfg.sink.as_ref());
        let mut worker = Worker {
            shared: Arc::clone(&shared),
            link: link.clone(),
            llm,
            sessions,
            session,
            cfg,
            messages,
            ledger: Ledger::new(),
            tools: None,
            late: Arc::default(),
            plan: Vec::new(),
            measured: 0,
            last_describe: None,
            model_checked: false,
            prompt_fetch: PromptFetch::default(),
            refetch_after_answer: false,
        };
        let (tx, rx) = channel();
        worker.start_prompt_fetch();
        let spawned = std::thread::Builder::new().name("omuzaki-assistant".into()).spawn(move || worker.run(&rx));
        let thread = match spawned {
            Ok(handle) => Some(handle),
            Err(error) => {
                let mut ctl = shared.lock();
                ctl.ended = true;
                ctl.prompts = PromptsState::Failed(HostError::Mcp(format!("could not start the host thread: {error}")));
                None
            }
        };
        Self { shared, link, commands: Mutex::new(Some(tx)), thread: Mutex::new(thread) }
    }
}

impl Worker {
    /// The host thread: records the session, then serves commands until told to stop.
    fn run(&mut self, commands: &Receiver<Cmd>) {
        self.shared.record(Entry::session(self.session, SessionNote::Opened));
        loop {
            self.service_prompts();
            match commands.recv_timeout(TICK) {
                Ok(Cmd::Send(msg)) => {
                    self.answer(&msg);
                    self.refetch_after_answer = true;
                }
                Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        self.close_session();
    }

    /// Ends the session if it is still live, once, and closes the link.
    fn close_session(&mut self) {
        if let Some(info) = self.session_info()
            && !matches!(info.state, SessionState::Ended { .. })
        {
            // Ending a session that ended meanwhile is a harmless conflict.
            let _ = self.sessions.end(self.session, EndReason::Closed);
        }
        self.link.close();
    }
}

impl AssistantHost {
    fn session_ended(&self) -> bool {
        self.shared.session_over()
    }
}

impl Assistant for AssistantHost {
    fn send(&self, msg: UserMessage) -> Result<(), HostError> {
        if self.session_ended() {
            self.shared.mark_ended();
            return Err(Shared::ended_error());
        }
        {
            let mut ctl = self.shared.lock();
            if ctl.running {
                return Err(HostError::Busy);
            }
            self.shared.begin_answer(&mut ctl);
        }
        let sent =
            self.commands.lock().unwrap_or_else(PoisonError::into_inner).as_ref().map(|tx| tx.send(Cmd::Send(msg)));
        if let Some(Ok(())) = sent {
            return Ok(());
        }
        self.shared.lock().running = false;
        Err(HostError::SessionEnded)
    }

    fn stop(&self) {
        let mut ctl = self.shared.lock();
        if !ctl.running || !self.shared.request_stop(&mut ctl) {
            return;
        }
        let report = self.shared.hub.stop();
        ctl.stop_text_read = Some(report.text_read);
        // Cut the answer's text to what was read: earlier turns of the answer first, then the turn in flight.
        let from = ctl.answer_from;
        let ctl = &mut *ctl;
        let turns = ctl.transcript[from..].iter_mut().filter_map(|e| match e {
            Entry::Assistant { blocks, .. } => Some(blocks),
            _ => None,
        });
        let left = cut_text_blocks(turns, report.text_read);
        let mut live = vec![omuzaki_llm_api::message::ContentBlock::text(std::mem::take(&mut ctl.live_text))];
        cut_text_blocks([&mut live], left);
        ctl.live_text = live
            .into_iter()
            .find_map(|b| if let omuzaki_llm_api::message::ContentBlock::Text(t) = b { Some(t) } else { None })
            .unwrap_or_default();
        ctl.question = None;
    }

    fn answer(&self, q: QuestionId, text: String) -> Result<(), HostError> {
        self.shared.answer(q, text)
    }

    fn events(&self) -> EventReceiver {
        self.shared.hub.subscribe()
    }

    fn transcript(&self) -> Vec<Entry> {
        self.shared.transcript()
    }

    fn prompts(&self) -> PromptsState {
        self.shared.prompts_state()
    }

    fn refresh_prompts(&self) {
        self.shared.mark_prompts_stale();
    }

    fn prompt(&self, name: &str, args: BTreeMap<String, String>) -> Result<String, HostError> {
        if self.session_ended() {
            return Err(HostError::SessionEnded);
        }
        let pending = self.link.get_prompt(name, prompt_arguments(&args));
        let clock = &self.shared.clock;
        let deadline = clock.now().0 + PROMPT_TIMEOUT_S;
        loop {
            match pending.poll(Duration::from_millis(1)) {
                Some(Ok(Ok(text))) => return check_prompt_text(text),
                Some(Ok(Err(LinkError::Protocol(text)))) => return Err(HostError::Mcp(text)),
                Some(Ok(Err(_)) | Err(_)) => return Err(HostError::SessionEnded),
                None if clock.now().0 >= deadline => return Err(HostError::Mcp("timed out".into())),
                None => {}
            }
        }
    }
}

impl Drop for AssistantHost {
    fn drop(&mut self) {
        {
            let mut ctl = self.shared.lock();
            ctl.shutdown = true;
            self.shared.request_stop(&mut ctl);
        }
        if let Some(tx) = self.commands.lock().unwrap_or_else(PoisonError::into_inner).take() {
            // The thread may already have ended; then there is nothing to tell.
            let _ = tx.send(Cmd::Shutdown);
        }
        if let Some(thread) = self.thread.lock().unwrap_or_else(PoisonError::into_inner).take() {
            // A panicked host thread has already said what it had to; the drop must still finish.
            let _ = thread.join();
        }
    }
}
