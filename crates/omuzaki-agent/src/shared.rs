//! The state the panel-facing handle and the host thread share.
//!
//! One lock, [`Control`], guards everything Stop must change atomically with the event queue: the stopped flag, the
//! text of the turn in flight, the transcript and the open question. The queue ([`EventHub`]) has its own lock, always
//! taken inside this one, never the other way round.
//!
//! Every entry is appended to the store and to the transcript under one more lock, `records`, taken outside the other
//! two. So the store and the transcript hold the same entries in the same order, also when the host thread and a
//! late-result callback record at once, and what is owed to the model (`owed_since_context`) is decided against the
//! same order a reopened host reads.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::{AssistantEvent, QuestionId};
use omuzaki_agent_api::hub::EventHub;
use omuzaki_agent_api::prompts::PromptsState;
use omuzaki_core::time::Clock;
use omuzaki_llm_api::message::ContentBlock;
use omuzaki_llm_api::stream::Cancel;
use omuzaki_ops_api::{SessionId, SessionState, Sessions};

use crate::cut::{assistant_blocks_mut, assistant_text_bytes, cut_text_blocks};
use crate::sink::EntrySink;

/// An `ask_person` question waiting for the person.
pub(crate) struct OpenQuestion {
    pub id: QuestionId,
    pub reply: Option<String>,
}

/// What the lock guards.
// The flags are independent facts about one answer and one session, read together under the lock; a state enum would
// need a variant for each combination.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct Control {
    /// An answer is in progress, from an accepted `send` until just before its end (`Done` or `Unclosed`).
    pub running: bool,
    /// Stop was pressed for the answer in progress.
    pub stopping: bool,
    /// The session is over (revoked, closed or ended): every later `send` is refused.
    pub ended: bool,
    /// The host is being dropped.
    pub shutdown: bool,
    /// The cancel flag of the turn or call in flight; Stop sets it.
    pub cancel: Cancel,
    /// Every entry so far, in order.
    pub transcript: Vec<Entry>,
    /// Where the answer in progress starts in `transcript`.
    pub answer_from: usize,
    /// The text of the turn in flight that has been sent to the panel.
    pub live_text: String,
    /// Bytes of the answer's text the panel had read when Stop was pressed.
    pub stop_text_read: Option<usize>,
    pub question: Option<OpenQuestion>,
    pub next_question: u64,
    pub prompts: PromptsState,
    /// `refresh_prompts` was called and the host thread has not refetched yet.
    pub refresh_prompts: bool,
}

/// Shared between the handle, the host thread and the late-result callbacks.
pub(crate) struct Shared {
    pub hub: EventHub,
    pub ctl: Mutex<Control>,
    /// Held while an entry is appended to the store and the transcript (see the module docs).
    records: Mutex<()>,
    /// Mirrors `Control::stopping` for loops that poll it without taking the lock.
    stop_flag: AtomicBool,
    pub clock: Arc<dyn Clock>,
    pub sink: Arc<dyn EntrySink>,
    pub sessions: Arc<dyn Sessions>,
    pub session: SessionId,
}

impl Shared {
    pub(crate) fn new(
        clock: Arc<dyn Clock>,
        sink: Arc<dyn EntrySink>,
        sessions: Arc<dyn Sessions>,
        session: SessionId,
        transcript: Vec<Entry>,
    ) -> Self {
        // Question ids are the conversation's: a continued one goes on from the highest already used.
        let next_question = transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Question { id, .. } | Entry::Answer { id, .. } => Some(id.0),
                _ => None,
            })
            .max()
            .map_or(1, |highest| highest + 1);
        let ctl = Control {
            running: false,
            stopping: false,
            ended: false,
            shutdown: false,
            cancel: Cancel::new(),
            answer_from: transcript.len(),
            transcript,
            live_text: String::new(),
            stop_text_read: None,
            question: None,
            next_question,
            prompts: PromptsState::Loading,
            refresh_prompts: false,
        };
        Self {
            hub: EventHub::new(),
            ctl: Mutex::new(ctl),
            records: Mutex::new(()),
            stop_flag: AtomicBool::new(false),
            clock,
            sink,
            sessions,
            session,
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Control> {
        // A poisoned lock means a thread panicked mid-update; the state is plain data and Stop must still work.
        self.ctl.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stop was pressed (or the host is being dropped) for the answer in progress.
    pub(crate) fn stopping(&self) -> bool {
        self.stop_flag.load(Ordering::Acquire)
    }

    /// Marks Stop (or a shutdown) as requested and cancels what is in flight. Returns false if it already was.
    pub(crate) fn request_stop(&self, ctl: &mut Control) -> bool {
        if ctl.stopping {
            return false;
        }
        ctl.stopping = true;
        self.stop_flag.store(true, Ordering::Release);
        ctl.cancel.cancel();
        true
    }

    /// A fresh cancel flag for the next turn or call, already set if Stop was pressed.
    pub(crate) fn fresh_cancel(&self) -> Cancel {
        let mut ctl = self.lock();
        let cancel = Cancel::new();
        if ctl.stopping {
            cancel.cancel();
        }
        ctl.cancel = cancel.clone();
        cancel
    }

    /// Starts an answer: clears the stop state and the queue's stopped flag.
    pub(crate) fn begin_answer(&self, ctl: &mut Control) {
        ctl.running = true;
        ctl.stopping = false;
        self.stop_flag.store(false, Ordering::Release);
        ctl.stop_text_read = None;
        ctl.live_text.clear();
        ctl.answer_from = ctl.transcript.len();
        ctl.cancel = Cancel::new();
        self.hub.reset();
    }

    /// Queues an event for the panel. False once Stop has been pressed, except for `Paused`, `Error` and the answer's
    /// end.
    pub(crate) fn emit(&self, event: AssistantEvent) -> bool {
        let _guard = self.lock();
        self.hub.send(event)
    }

    /// Sends text to the panel and keeps it as the turn's live text, atomically with Stop.
    pub(crate) fn emit_text(&self, text: &str) {
        let mut ctl = self.lock();
        if self.hub.send(AssistantEvent::TextDelta(text.to_owned())) {
            ctl.live_text.push_str(text);
        }
    }

    fn records(&self) -> MutexGuard<'_, ()> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records an entry in the store and the transcript. A store failure is shown to the person and does not stop the
    /// answer: losing the log must not lose the work.
    pub(crate) fn record(&self, entry: Entry) {
        self.record_all(vec![entry]);
    }

    /// Records `entries` with nothing recorded between them.
    pub(crate) fn record_all(&self, entries: Vec<Entry>) {
        let _records = self.records();
        self.append_unlocked(entries);
    }

    fn append_unlocked(&self, entries: Vec<Entry>) {
        for entry in entries {
            if let Err(error) = self.sink.append(&entry) {
                self.emit(AssistantEvent::Error(error));
            }
            self.lock().transcript.push(entry);
        }
    }

    /// What the model is owed: the `Notice` entries and the late results (`<call>:late`) recorded since the last
    /// `Context` entry.
    pub(crate) fn owed(&self) -> Vec<Entry> {
        Self::owed_in(&self.lock().transcript)
    }

    fn owed_in(transcript: &[Entry]) -> Vec<Entry> {
        let since = transcript.iter().rposition(|e| matches!(e, Entry::Context { .. })).map_or(0, |at| at + 1);
        transcript[since..].iter().filter(|e| matches!(e, Entry::Notice { .. }) || is_late(e)).cloned().collect()
    }

    /// Records a message's `Context` entry with what is owed in it. Under the record lock, `build` is given the owed
    /// entries and a lookup of the tool each call id belongs to, and returns the entry to record (or `None` for an empty
    /// context, recording nothing) with whatever the caller wants back. Nothing can be recorded between reading what is
    /// owed and recording the context, so an owed entry is either in this context or recorded after it and owed to the
    /// next one, in the store as in the transcript.
    pub(crate) fn record_context<T>(
        &self,
        build: impl FnOnce(&[Entry], &dyn Fn(&str) -> Option<String>) -> Option<(Entry, T)>,
    ) -> Option<T> {
        let _records = self.records();
        let (owed, calls) = {
            let ctl = self.lock();
            let calls: Vec<(String, String)> = ctl
                .transcript
                .iter()
                .filter_map(|e| match e {
                    Entry::ToolCall { id, tool, .. } => Some((id.clone(), tool.clone())),
                    _ => None,
                })
                .collect();
            (Self::owed_in(&ctl.transcript), calls)
        };
        let tool_of = |id: &str| calls.iter().find(|(call, _)| call == id).map(|(_, tool)| tool.clone());
        let (context, back) = build(&owed, &tool_of)?;
        self.append_unlocked(vec![context]);
        Some(back)
    }

    /// Records the finished turn's entry and drops the live text it replaces in one step, so a panel reading the
    /// transcript never sees the turn twice or not at all. If Stop was pressed meanwhile, the text is cut to what the
    /// person had been shown before it is recorded or stored, so a turn that finishes after Stop cannot bring unread
    /// text back. Returns the blocks as recorded.
    pub(crate) fn record_turn(&self, mut blocks: Vec<ContentBlock>) -> Vec<ContentBlock> {
        let _records = self.records();
        let entry = {
            let mut ctl = self.lock();
            if let Some(read) = ctl.stop_text_read {
                let shown_before = assistant_text_bytes(&ctl.transcript[ctl.answer_from..]);
                cut_text_blocks([&mut blocks], read.saturating_sub(shown_before));
            }
            ctl.live_text.clear();
            let entry = Entry::assistant(blocks.clone());
            ctl.transcript.push(entry.clone());
            entry
        };
        if let Err(error) = self.sink.append(&entry) {
            self.emit(AssistantEvent::Error(error));
        }
        blocks
    }

    /// The transcript for a panel opened late, with the turn in flight as it has been shown.
    pub(crate) fn transcript(&self) -> Vec<Entry> {
        // Text is sent to the panel under this same lock, so nothing can slip between the snapshot and the watermark:
        // what is marked displayed is exactly what the snapshot holds. (Lock order: control, then the queue's.)
        let ctl = self.lock();
        let entries = Self::entries_of(&ctl);
        // What this hands out is what the person sees, so Stop must not cut it even if the queue never delivered it.
        self.hub.mark_sent_as_displayed();
        entries
    }

    /// The transcript for the host's own summary of the conversation: the answer in progress counts only the text the
    /// person has been shown. What they have not read is still Stop's to withdraw, and a summary built from it would
    /// keep it in the model's history, in the log and after a restart. Read text can no longer be withdrawn, and the
    /// display watermark does not move.
    pub(crate) fn snapshot_read(&self) -> Vec<Entry> {
        let ctl = self.lock();
        let mut entries = Self::entries_of(&ctl);
        let from = ctl.answer_from.min(entries.len());
        cut_text_blocks(assistant_blocks_mut(&mut entries[from..]), self.hub.text_read());
        entries
    }

    fn entries_of(ctl: &Control) -> Vec<Entry> {
        let mut entries = ctl.transcript.clone();
        if !ctl.live_text.is_empty() {
            entries.push(Entry::assistant(vec![ContentBlock::text(ctl.live_text.clone())]));
        }
        entries
    }

    /// Bytes of the text the answer in progress has written so far, in the turns already recorded. Stop counts the
    /// answer's text from its first byte, so this is the offset to subtract once the history it came from is gone.
    ///
    /// `None` if Stop has been pressed: Stop cut that text to what was read, so the count no longer says what the
    /// history held, and the caller must not build anything on it. Read under the lock Stop takes, so a Stop either
    /// came first (`None`) or comes after and finds the count it is judged against.
    pub(crate) fn answer_text_unless_stopped(&self) -> Option<usize> {
        let ctl = self.lock();
        if ctl.stopping {
            return None;
        }
        Some(Self::text_bytes(&ctl))
    }

    fn text_bytes(ctl: &Control) -> usize {
        assistant_text_bytes(&ctl.transcript[ctl.answer_from..])
    }

    /// Marks the session over so later `send`s are refused.
    pub(crate) fn mark_ended(&self) {
        self.lock().ended = true;
    }

    /// Whether the session is over: the host knows it ended, or the executor says so.
    pub(crate) fn session_over(&self) -> bool {
        if self.lock().ended {
            return true;
        }
        match self.sessions.session(self.session) {
            Some(info) => matches!(info.state, SessionState::Ended { .. }),
            None => true,
        }
    }

    /// Hands the person's reply to the open question `q` to the call that asked. Nothing is written here: the caller
    /// is the panel's thread, and the call records the answer from the host thread.
    pub(crate) fn answer(&self, q: QuestionId, text: String) -> Result<(), HostError> {
        let mut ctl = self.lock();
        match ctl.question.as_mut() {
            Some(open) if open.id == q && open.reply.is_none() => {
                open.reply = Some(text);
                Ok(())
            }
            _ => Err(HostError::Mcp(format!("no open question {}", q.0))),
        }
    }

    /// The error a refused `send` returns once the session has ended.
    pub(crate) fn ended_error() -> HostError {
        HostError::SessionEnded
    }
}

/// Whether `entry` is a result that arrived after Stop: its id is the call's, suffixed `:late`.
pub(crate) fn is_late(entry: &Entry) -> bool {
    matches!(entry, Entry::ToolResult { id, .. } if id.ends_with(":late"))
}
