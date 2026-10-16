//! [`ScriptedAssistant`]: an `Assistant` that plays a JSON event script on a `Clock`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::{AssistantEvent, QuestionId};
use omuzaki_agent_api::hub::{EventHub, EventReceiver};
use omuzaki_agent_api::message::UserMessage;
use omuzaki_agent_api::prompts::{PromptInfo, PromptsState};
use omuzaki_core::time::{Clock, Seconds};
use serde::{Deserialize, Serialize};

/// One event of a script and when it is due.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimedEvent {
    /// Seconds after the `send` that started the answer, on the `Clock`.
    pub at: Seconds,
    /// What the panel receives.
    pub event: AssistantEvent,
}

impl TimedEvent {
    /// `event`, due `at` seconds after the send.
    pub fn new(at: Seconds, event: AssistantEvent) -> Self {
        Self { at, event }
    }
}

/// The events one `send` plays, in order. End each answer with its end, `Done` (or `Unclosed`, the state where the
/// answer's changes could not be closed): the assistant stays busy until it is delivered.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScriptedAnswer {
    /// The events, in delivery order.
    pub events: Vec<TimedEvent>,
}

impl ScriptedAnswer {
    /// An answer playing `events`.
    pub fn new(events: Vec<TimedEvent>) -> Self {
        Self { events }
    }
}

struct Playing {
    started: Seconds,
    events: VecDeque<TimedEvent>,
}

#[derive(Default)]
struct State {
    answers: Vec<ScriptedAnswer>,
    next: usize,
    playing: Option<Playing>,
    /// The answer is running (or stopping) and its end (`Done` or `Unclosed`) has not been delivered.
    busy: bool,
    /// After a stop with a delay: when the `Done` is due.
    done_due: Option<Seconds>,
    stop_delay: Seconds,
    transcript: Vec<Entry>,
    sent: Vec<UserMessage>,
    stop_count: usize,
    answered: Vec<(QuestionId, String)>,
    /// Questions delivered and not yet answered: `answer` refuses any other id, as the real host does.
    open_questions: Vec<QuestionId>,
    prompts: Vec<PromptInfo>,
    prompt_texts: BTreeMap<String, String>,
    prompt_failure: Option<HostError>,
    /// A `refresh_prompts` is waiting to be seen: the next `prompts()` reads `Loading`.
    refreshing: bool,
    refresh_count: usize,
    prompt_calls: Vec<(String, BTreeMap<String, String>)>,
}

/// An `Assistant` that plays a JSON event script and records what the panel sends. The n-th `send` plays the n-th
/// [`ScriptedAnswer`]; events reach the panel through a real `EventHub` when the test calls
/// [`tick`](Self::tick), so Stop's guarantee is the real one. No threads: nothing plays unless `tick` is called.
pub struct ScriptedAssistant {
    clock: Arc<dyn Clock>,
    hub: EventHub,
    state: Mutex<State>,
}

impl ScriptedAssistant {
    /// An assistant whose n-th `send` plays `answers[n]`. A `send` after the last answer is recorded and plays
    /// nothing.
    pub fn new(answers: Vec<ScriptedAnswer>, clock: Arc<dyn Clock>) -> Self {
        Self { clock, hub: EventHub::new(), state: Mutex::new(State { answers, ..State::default() }) }
    }

    /// An assistant from a script written as JSON: an array of answers (a `Vec<ScriptedAnswer>`), each
    /// `{"events": [{"at": 0.5, "event": ...}]}`, with `at` in seconds and `event` an `AssistantEvent`.
    ///
    /// # Errors
    /// The JSON is not such an array.
    pub fn from_json(json: &str, clock: Arc<dyn Clock>) -> Result<Self, serde_json::Error> {
        Ok(Self::new(serde_json::from_str(json)?, clock))
    }

    /// The conversation `transcript()` returns.
    #[must_use]
    pub fn with_transcript(self, entries: Vec<Entry>) -> Self {
        self.lock().transcript = entries;
        self
    }

    /// After `stop`: `Paused` at once, `Done` `delay` later on the `Clock` (a joined driver job reverting, D4),
    /// delivered by `tick`. Default 0: both at once.
    #[must_use]
    pub fn with_stop_delay(self, delay: Seconds) -> Self {
        self.lock().stop_delay = delay;
        self
    }

    /// The prompts `prompts()` offers, sorted by name (§7).
    #[must_use]
    pub fn with_prompts(self, mut prompts: Vec<PromptInfo>) -> Self {
        prompts.sort_by(|a, b| a.name.cmp(&b.name));
        self.lock().prompts = prompts;
        self
    }

    /// The text `prompt(name, ..)` returns, whatever the arguments.
    #[must_use]
    pub fn with_prompt_text(self, name: impl Into<String>, text: impl Into<String>) -> Self {
        self.lock().prompt_texts.insert(name.into(), text.into());
        self
    }

    /// Makes `prompts()` read `Failed(error)` and `prompt` return `Err(error)`, until `refresh_prompts` clears it
    /// (after one `Loading`).
    #[must_use]
    pub fn fail_prompts(self, error: HostError) -> Self {
        self.lock().prompt_failure = Some(error);
        self
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Delivers, in order and through the `EventHub`, every event now due: those whose `at` has passed since the
    /// `send` that started the answer, and the `Done` that follows a delayed stop.
    pub fn tick(&self) {
        let now = self.clock.now();
        let mut state = self.lock();
        if let Some(due) = state.done_due {
            if now >= due {
                state.done_due = None;
                state.busy = false;
                self.hub.send(AssistantEvent::Done);
            }
            return;
        }
        let mut finished = false;
        let mut asked = Vec::new();
        if let Some(playing) = state.playing.as_mut() {
            while let Some(front) = playing.events.front() {
                if now.0 < playing.started.0 + front.at.0 {
                    break;
                }
                let Some(timed) = playing.events.pop_front() else { break };
                finished = timed.event.ends_answer();
                if let AssistantEvent::Question { id, .. } = &timed.event {
                    asked.push(*id);
                }
                self.hub.send(timed.event);
                if finished {
                    break;
                }
            }
        }
        state.open_questions.extend(asked);
        if finished {
            state.open_questions.clear();
            state.playing = None;
            state.busy = false;
        }
    }

    /// Every message sent, in order (rejected `Busy` sends are not recorded).
    pub fn sent(&self) -> Vec<UserMessage> {
        self.lock().sent.clone()
    }

    /// How many times `stop` was called.
    pub fn stop_count(&self) -> usize {
        self.lock().stop_count
    }

    /// Every `(question, text)` passed to `answer`, in order.
    pub fn answered(&self) -> Vec<(QuestionId, String)> {
        self.lock().answered.clone()
    }

    /// How many times `refresh_prompts` was called.
    pub fn refresh_count(&self) -> usize {
        self.lock().refresh_count
    }

    /// Every `(name, args)` passed to `prompt`, in order, beside the `sent` log.
    pub fn prompt_calls(&self) -> Vec<(String, BTreeMap<String, String>)> {
        self.lock().prompt_calls.clone()
    }
}

impl Assistant for ScriptedAssistant {
    /// Records the message, resets the hub and starts the next answer. `Err(Busy)` until the previous answer's end
    /// (`Done` or `Unclosed`) has been delivered.
    fn send(&self, msg: UserMessage) -> Result<(), HostError> {
        let mut state = self.lock();
        if state.busy {
            return Err(HostError::Busy);
        }
        state.sent.push(msg);
        self.hub.reset();
        if let Some(answer) = state.answers.get(state.next).cloned() {
            state.next += 1;
            state.playing = Some(Playing { started: self.clock.now(), events: answer.events.into() });
            state.busy = true;
        }
        Ok(())
    }

    /// Calls `EventHub::stop`, drops every undelivered scripted event, queues `Paused` and then `Done` (after the stop
    /// delay), and counts the call. With no answer running, or one already stopping, it only counts.
    fn stop(&self) {
        let mut state = self.lock();
        state.stop_count += 1;
        if !state.busy || state.done_due.is_some() {
            return;
        }
        self.hub.stop();
        state.open_questions.clear();
        state.playing = None;
        self.hub.send(AssistantEvent::Paused);
        let delay = state.stop_delay;
        if delay.0 > 0.0 {
            state.done_due = Some(Seconds(self.clock.now().0 + delay.0));
        } else {
            state.busy = false;
            self.hub.send(AssistantEvent::Done);
        }
    }

    /// Records the answer to a question that was delivered and is still open.
    ///
    /// # Errors
    /// [`HostError::Mcp`] for any other id, as the real host answers a stale card.
    fn answer(&self, q: QuestionId, text: String) -> Result<(), HostError> {
        let mut state = self.lock();
        let Some(at) = state.open_questions.iter().position(|open| *open == q) else {
            return Err(HostError::Mcp(format!("no open question {}", q.0)));
        };
        state.open_questions.remove(at);
        state.answered.push((q, text));
        Ok(())
    }

    fn events(&self) -> EventReceiver {
        self.hub.subscribe()
    }

    fn transcript(&self) -> Vec<Entry> {
        self.lock().transcript.clone()
    }

    /// `Failed` while `fail_prompts` holds; `Loading` once after `refresh_prompts` (which also clears the failure);
    /// otherwise `Ready` with the prompts given, sorted by name.
    fn prompts(&self) -> PromptsState {
        let mut state = self.lock();
        if state.refreshing {
            state.refreshing = false;
            state.prompt_failure = None;
            return PromptsState::Loading;
        }
        match &state.prompt_failure {
            Some(error) => PromptsState::Failed(error.clone()),
            None => PromptsState::Ready(state.prompts.clone()),
        }
    }

    fn refresh_prompts(&self) {
        let mut state = self.lock();
        state.refresh_count += 1;
        state.refreshing = true;
    }

    /// Records the call, then returns the failure if one is set, the text given to `with_prompt_text`, or
    /// `Err(Mcp(..))` for an unknown name. Sends nothing.
    fn prompt(&self, name: &str, args: BTreeMap<String, String>) -> Result<String, HostError> {
        let mut state = self.lock();
        state.prompt_calls.push((name.to_owned(), args));
        if let Some(error) = &state.prompt_failure {
            return Err(error.clone());
        }
        state.prompt_texts.get(name).cloned().ok_or_else(|| HostError::Mcp(format!("unknown prompt: {name}")))
    }
}
