//! [`Show`]: the turn loop (`ai-performer.md` §5), one performer on one cast member.
//!
//! The loop is a state machine stepped on the runner clock ([`Show::step`]), so the fake harness drives it
//! deterministically and the App runs it on the performer thread every few milliseconds. Each step:
//!
//! 1. **hears** ([`Ears`]): the floor (the human always has it), addressing and the follow-up window, barge-in on
//!    `Held` with one resume after `Rejected` or a backchannel, echo, mute and private turns; and listens with its
//!    face while the person talks (#2453): a listening face from the decision layer on their partial words about once
//!    a second, and a nod on each backchannel cue of their voice;
//! 2. **thinks** ([`Brain`]): pulls the reply as it streams, chunks it into sentences and queues each one on the
//!    [`Voice`] at once, within the 20 s per-turn cap, holding them while the human talks (dropped after 3 s);
//! 3. **decides** (#2348): asks the decision layer which face fits the line or the person, and falls back to the
//!    rule floor when the provider is slow or absent, so the character reacts even without the LLM;
//! 4. **plays**: reports each chunk as it starts, times each intent at its word
//!    (`from + duration × offset / length`), and applies it through the [`Stage`] under the cooldowns.
//!
//! Everything it does is a [`TranscriptLine`], drained with [`Show::drain`]; private turns' lines carry no text.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use omuzaki_anim_api::score::ExpressionId;
use omuzaki_core::time::Seconds;
use omuzaki_llm_api::stream::Cancel;
use omuzaki_performer_api::brain::{
    Brain, BrainError, BrainEvent, BrainSource, PastTurn, Polled, TurnInput, TurnStream,
};
use omuzaki_performer_api::decide::{Ask, DECISION_TIMEOUT, Decider, DecisionTicket, Moment};
use omuzaki_performer_api::ears::{Ears, Heard};
use omuzaki_performer_api::event::{PerformerEvent, SafetyVerdict, TranscriptLine};
use omuzaki_performer_api::ids::{PersonaRevision, VoiceId};
use omuzaki_performer_api::intent::{Intent, StyleTag};
use omuzaki_performer_api::limits::{
    BACKCHANNEL_NOD, FLOOR_WAIT_S, FOLLOW_UP_WINDOW_S, INTENT_BURST, INTENTS_PER_S, LISTEN_FACE_INTERVAL_S,
    MOVE_INTERVAL_S, NOD_GAP_S, PROACTIVE_INTERVAL_S, PROACTIVE_SILENCE_S, REACTION_APEX, SAME_EMOTE_COOLDOWN_S,
    SPEAKING_DAMP, TALK_SHARE_MAX, TALK_SHARE_WINDOW_S, TURN_SPEECH_S,
};
use omuzaki_performer_api::speech::{SpeakHow, SpeechHandle, SpeechState, Voice};
use omuzaki_performer_api::stage::{Inventory, Stage, StageError};
use omuzaki_stage_api::ids::MemberId;

use crate::addressing::{Names, Respond, is_backchannel, is_echo};
use crate::chunker::{Chunk, Chunker};
use crate::decide::{Gates, RuleFloor, questions_listening, questions_speaking, with_floor};
use crate::parser::parse_reply;
use crate::persona::Persona;

/// Events pulled from the brain in one step at most, so a flood can't starve hearing.
const EVENTS_PER_STEP: usize = 512;
/// Seconds of audio a character is assumed to take before the voice reports the real length (a little slower than
/// most voices, so the 20 s cap errs short).
pub(crate) const EST_S_PER_CHAR: f64 = 0.07;
/// Lines of public conversation the decision layer sees.
pub(crate) const CONTEXT_LINES: usize = 6;
/// The performer's own speech is remembered this long for echo checks.
pub(crate) const ECHO_MEMORY_S: f64 = 30.0;

/// How the show is set up: who it plays, in which voice, and when it answers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ShowConfig {
    /// The cast member it plays (ST-12: one performer per member).
    pub member: MemberId,
    /// The voice it speaks in.
    pub voice: VoiceId,
    /// When it answers (§5.1).
    pub respond: Respond,
    /// Its share of speech in a rolling five minutes, above which only addressed turns run (§5.1).
    pub talk_share: f32,
    /// Proactive speech (§5.7), off by default.
    pub proactive: bool,
    /// The decision layer's reactions (#2348): a face for each line it speaks and for each line addressed to it.
    pub reactions: bool,
    /// The decision layer's thresholds.
    pub gates: Gates,
}

impl ShowConfig {
    /// The defaults for `member` speaking in `voice`: answers when addressed, reactions on, proactive off.
    pub fn new(member: MemberId, voice: VoiceId) -> Self {
        Self {
            member,
            voice,
            respond: Respond::Addressed,
            talk_share: TALK_SHARE_MAX,
            proactive: false,
            reactions: true,
            gates: Gates::default(),
        }
    }

    /// The same, answering as `respond` says.
    #[must_use]
    pub fn respond(mut self, respond: Respond) -> Self {
        self.respond = respond;
        self
    }

    /// The same, with reactions on or off.
    #[must_use]
    pub fn reactions(mut self, on: bool) -> Self {
        self.reactions = on;
        self
    }

    /// The same, with proactive speech on or off.
    #[must_use]
    pub fn proactive(mut self, on: bool) -> Self {
        self.proactive = on;
        self
    }
}

/// The parts a show runs over; the App wires real ones, tests wire the fakes.
#[non_exhaustive]
pub struct Parts {
    /// Writes the replies.
    pub brain: Box<dyn Brain>,
    /// Hears the person.
    pub ears: Box<dyn Ears>,
    /// Speaks the replies.
    pub voice: Box<dyn Voice>,
    /// Takes the intents.
    pub stage: Arc<dyn Stage>,
    /// Answers the decision layer's questions; `None` uses the rule floor alone.
    pub decider: Option<Box<dyn Decider>>,
}

impl Parts {
    /// The parts, with the rule floor as the only decider.
    pub fn new(brain: Box<dyn Brain>, ears: Box<dyn Ears>, voice: Box<dyn Voice>, stage: Arc<dyn Stage>) -> Self {
        Self { brain, ears, voice, stage, decider: None }
    }

    /// The same, with `decider` asked first.
    #[must_use]
    pub fn decider(mut self, decider: Box<dyn Decider>) -> Self {
        self.decider = Some(decider);
        self
    }
}

/// Where the show is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Waiting for something to answer.
    Idle,
    /// A turn is running and nothing plays yet.
    Thinking,
    /// A turn is speaking.
    Speaking,
    /// The human cut in; the turn waits to resume or end.
    Held,
}

/// What the show reports about itself.
#[derive(Clone, Debug, PartialEq, serde::Serialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct Status {
    /// Where it is.
    pub phase: Phase,
    /// The running turn.
    pub turn: Option<u64>,
    /// Turns run so far.
    pub turns: u64,
    /// Final transcripts dropped as its own voice coming back.
    pub echoes: u64,
    /// Its share of speech in the last five minutes, 0 to 1.
    pub share: f32,
    /// The mic is muted.
    pub muted: bool,
    /// How the decision layer is doing.
    pub decisions: DecisionStats,
    /// What this run has spent on a hosted provider, in micro-dollars (0 when everything runs on this machine).
    pub spent_micro_usd: u64,
}

/// How the decision layer has done this run: how often its provider answered in time, and how fast as the loop saw
/// it (to the loop's step).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct DecisionStats {
    /// The provider asked first (`rules` when the rule floor answers alone).
    pub provider: String,
    /// Asks made.
    pub asked: u64,
    /// Asks the provider answered in time.
    pub answered: u64,
    /// Asks it answered too late: the rule floor answered.
    pub timeouts: u64,
    /// Asks it failed: the rule floor answered.
    pub errors: u64,
    /// Why the last one failed.
    pub last_error: Option<String>,
    /// The median time to an answer in time, in milliseconds, over the last [`LATENCY_KEPT`] answers.
    pub p50_ms: Option<f64>,
    /// The 90th percentile of the same.
    pub p90_ms: Option<f64>,
}

/// How many answer times [`DecisionStats`] keeps.
pub const LATENCY_KEPT: usize = 128;

/// The running counts behind [`DecisionStats`].
#[derive(Default)]
pub(crate) struct Decisions {
    pub(crate) stats: DecisionStats,
    latencies: VecDeque<f64>,
}

impl Decisions {
    pub(crate) fn answered(&mut self, ms: f64) {
        self.stats.answered += 1;
        if self.latencies.len() == LATENCY_KEPT {
            self.latencies.pop_front();
        }
        self.latencies.push_back(ms);
    }

    pub(crate) fn report(&self) -> DecisionStats {
        let mut sorted: Vec<f64> = self.latencies.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let at = |p: f64| {
            // Small counts; the rank is in range.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
            let rank = ((p * sorted.len() as f64).ceil() as usize).max(1);
            sorted.get(rank - 1).copied()
        };
        DecisionStats { p50_ms: at(0.5), p90_ms: at(0.9), ..self.stats.clone() }
    }
}

/// One chunk handed to the voice.
struct Queued {
    chunk: Chunk,
    handle: SpeechHandle,
    /// Its audio length, once the voice says.
    duration: Option<Seconds>,
    /// When it started playing, once it has.
    from: Option<Seconds>,
    /// Its `speaking` line is out.
    reported: bool,
}

/// A running turn.
#[allow(clippy::struct_excessive_bools)] // independent facts about one turn
struct Turn {
    number: u64,
    private: bool,
    heard: Option<String>,
    cancel: Cancel,
    /// `None` once the reply is fully read.
    stream: Option<TurnStream>,
    chunker: Chunker,
    style: Vec<StyleTag>,
    /// The reply's speakable text so far.
    text: String,
    /// Chunks waiting for the floor (or for a resume).
    ready: VecDeque<Chunk>,
    /// Chunks handed to the voice, in order; a resume appends copies.
    queued: Vec<Queued>,
    /// Intents not timed yet: `(offset, intent)`.
    intents: Vec<(u32, Intent)>,
    /// Offsets of the chunks whose words carry the model's own emote or gesture, so a reaction never doubles one.
    tagged: Vec<(u32, &'static str)>,
    /// Since when a ready reply has waited for the human to finish.
    waiting_since: Option<Seconds>,
    /// The 20 s cap is reached: no more chunks.
    capped: bool,
    /// `[pass]`: it says nothing.
    passed: bool,
    /// Cut by `Held` at this time, waiting for the human's `Final` or `Rejected`.
    held: Option<Seconds>,
    /// It has resumed once already.
    resumed: bool,
    /// Characters of the text that played, as an offset (`start + played`), across attempts.
    played_upto: u32,
    /// When its audio last ended.
    ended: Option<Seconds>,
}

impl Turn {
    fn reading(&self) -> bool {
        self.stream.is_some()
    }

    fn playing(&self) -> bool {
        self.queued.iter().any(|q| !q.handle.state().is_final())
    }

    /// Speech queued so far: each chunk's length as the voice reports it, or an estimate until it does.
    fn estimated_s(&self) -> f64 {
        self.queued
            .iter()
            .map(|q| {
                let known = match q.handle.state() {
                    SpeechState::Ready { duration, .. } | SpeechState::Playing { duration, .. } => Some(duration),
                    _ => q.duration,
                };
                known.map_or_else(|| chars_s(q.chunk.text.chars().count()), |d| d.0)
            })
            .sum()
    }

    fn said(&self) -> String {
        self.text.chars().take(self.played_upto as usize).collect::<String>().trim_end().to_owned()
    }
}

/// An intent timed and waiting for its moment.
struct Scheduled {
    turn: u64,
    offset: u32,
    at: Seconds,
    intent: Intent,
}

/// A decision on its way.
struct Pending {
    ticket: DecisionTicket,
    ask: Ask,
    line: String,
    turn: u64,
    asked_at: Seconds,
    /// The chunk it is about (`Speaking`), so the model's own tags there win.
    chunk_start: Option<u32>,
}

/// Who asked for an intent: the same-emote cooldown is kept per source, so a quick reaction never blocks the
/// model's own tag on the next word.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    /// The model's tags, `performer.say` and `performer.intent`.
    Model,
    /// The decision layer.
    Reaction,
}

/// Rate limits on intents (§7.1).
#[derive(Default)]
pub(crate) struct Limiter {
    last_emote: HashMap<(Source, ExpressionId), f64>,
    last_move: Option<f64>,
    tokens: f64,
    filled_at: Option<f64>,
}

impl Limiter {
    /// Whether `intent` may run at `at`, spending a token if so; the refusal's reason if not.
    pub(crate) fn admit(&mut self, intent: &Intent, source: Source, at: f64) -> Result<(), &'static str> {
        match intent {
            Intent::Emote { expression, .. }
                if self
                    .last_emote
                    .get(&(source, expression.clone()))
                    .is_some_and(|t| at - t < SAME_EMOTE_COOLDOWN_S) =>
            {
                return Err("cooldown");
            }
            Intent::MoveTo { .. } if self.last_move.is_some_and(|t| at - t < MOVE_INTERVAL_S) => {
                return Err("cooldown");
            }
            _ => {}
        }
        let burst = f64::from(INTENT_BURST);
        let tokens = match self.filled_at {
            None => burst,
            Some(t) => (self.tokens + (at - t).max(0.0) * f64::from(INTENTS_PER_S)).min(burst),
        };
        if tokens < 1.0 {
            return Err("rate limit");
        }
        self.tokens = tokens - 1.0;
        self.filled_at = Some(at);
        match intent {
            Intent::Emote { expression, .. } => {
                self.last_emote.insert((source, expression.clone()), at);
            }
            Intent::MoveTo { .. } => self.last_move = Some(at),
            _ => {}
        }
        Ok(())
    }
}

/// What the co-host steps and steers: the cascaded [`Show`], or the realtime one
/// ([`RealtimeShow`](crate::realtime::RealtimeShow)).
pub trait Performer: Send + std::fmt::Debug {
    /// One step at program time `now`.
    fn step(&mut self, now: Seconds);
    /// Says `text` now (`performer.say`); inline tags act at once.
    fn say(&mut self, now: Seconds, text: &str);
    /// Applies one intent now (`performer.intent`), checked, clamped and limited.
    fn intent(&mut self, now: Seconds, intent: &Intent);
    /// When it answers.
    fn set_respond(&mut self, respond: Respond);
    /// Cuts what it is saying and drops what waits (`performer.stop`).
    fn stop(&mut self, now: Seconds);
    /// Where it is.
    fn status(&self, now: Seconds) -> Status;
    /// The transcript lines since the last drain.
    fn drain(&mut self) -> Vec<TranscriptLine>;
}

impl Performer for Show {
    fn step(&mut self, now: Seconds) {
        Show::step(self, now);
    }

    fn say(&mut self, now: Seconds, text: &str) {
        Show::say(self, now, text);
    }

    fn intent(&mut self, now: Seconds, intent: &Intent) {
        Show::intent(self, now, intent);
    }

    fn set_respond(&mut self, respond: Respond) {
        Show::set_respond(self, respond);
    }

    fn stop(&mut self, now: Seconds) {
        Show::stop(self, now);
    }

    fn status(&self, now: Seconds) -> Status {
        Show::status(self, now)
    }

    fn drain(&mut self) -> Vec<TranscriptLine> {
        Show::drain(self)
    }
}

/// The turn loop.
pub struct Show {
    parts: Parts,
    config: ShowConfig,
    persona_name: String,
    names: Names,
    revision: PersonaRevision,
    floor: RuleFloor,
    inventory: Inventory,
    out: Vec<TranscriptLine>,
    next_turn: u64,
    turn: Option<Turn>,
    scheduled: Vec<Scheduled>,
    pending: Option<Pending>,
    decisions: Decisions,
    limiter: Limiter,
    /// The person is talking since then (`Onset` to `Final` or `Rejected`).
    talking_since: Option<Seconds>,
    muted: bool,
    /// The last sign of the person: an onset or the end of a final.
    last_heard: Option<Seconds>,
    /// When the last reply ended (or was cut), and its turn.
    last_reply: Option<(Seconds, u64)>,
    /// Every turn, public and private, in order; a public prompt never sees a private one.
    history: Vec<PastTurn>,
    /// The last cut turn, for the next prompt's note.
    interrupted: Option<PastTurn>,
    /// Public lines for the decision layer.
    context: VecDeque<String>,
    /// The performer's speech: `(from, end, text)`, for the talk share and echo.
    spoken: VecDeque<(f64, f64, String)>,
    echoes: u64,
    last_proactive: Option<Seconds>,
    /// When the last listening face (from a partial) and the last listener's nod were asked for.
    last_listen_face: Option<Seconds>,
    last_nod: Option<Seconds>,
}

impl Show {
    /// A show of `persona` over `parts`.
    pub fn new(persona: &Persona, parts: Parts, config: ShowConfig) -> Self {
        let inventory = parts.stage.inventory(&config.member).unwrap_or_default();
        let mut decisions = Decisions::default();
        parts.decider.as_ref().map_or("rules", |d| d.kind()).clone_into(&mut decisions.stats.provider);
        Self {
            parts,
            config,
            persona_name: persona.name.clone(),
            names: Names::new(&persona.names()),
            revision: persona.revision(),
            floor: RuleFloor,
            inventory,
            out: Vec::new(),
            next_turn: 1,
            turn: None,
            scheduled: Vec::new(),
            pending: None,
            decisions,
            limiter: Limiter::default(),
            talking_since: None,
            muted: false,
            last_heard: None,
            last_reply: None,
            history: Vec::new(),
            interrupted: None,
            context: VecDeque::new(),
            spoken: VecDeque::new(),
            echoes: 0,
            last_proactive: None,
            last_listen_face: None,
            last_nod: None,
        }
    }

    /// The setup.
    pub fn config(&self) -> &ShowConfig {
        &self.config
    }

    /// Changes when it answers, from the next utterance.
    pub fn set_respond(&mut self, respond: Respond) {
        self.config.respond = respond;
    }

    /// Changes the persona from the next turn (the brain is told separately).
    pub fn set_persona(&mut self, persona: &Persona) {
        self.persona_name.clone_from(&persona.name);
        self.names = Names::new(&persona.names());
        self.revision = persona.revision();
    }

    /// Where it is now.
    pub fn status(&self, now: Seconds) -> Status {
        let phase = match &self.turn {
            None => Phase::Idle,
            Some(t) if t.held.is_some() => Phase::Held,
            Some(t) if t.queued.iter().any(|q| q.from.is_some()) => Phase::Speaking,
            Some(_) => Phase::Thinking,
        };
        Status {
            phase,
            turn: self.turn.as_ref().map(|t| t.number),
            turns: self.next_turn - 1,
            echoes: self.echoes,
            share: self.share(now),
            muted: self.muted,
            decisions: self.decisions.report(),
            spent_micro_usd: 0,
        }
    }

    /// The transcript lines since the last drain, in the order they were written.
    pub fn drain(&mut self) -> Vec<TranscriptLine> {
        std::mem::take(&mut self.out)
    }

    /// One step at `now`: hear, read the reply, decide, play.
    pub fn step(&mut self, now: Seconds) {
        self.hear(now);
        self.read(now);
        self.release_ready(now);
        self.decide(now);
        self.play(now);
        self.proact(now);
    }

    /// Speaks `text` (inline tags allowed) as its own turn, without the LLM: `performer.say`. A running turn is cut.
    pub fn say(&mut self, now: Seconds, text: &str) {
        self.end_turn(now, true);
        let (_, events) = parse_reply(text);
        let number = self.begin(now, false);
        let stream = TurnStream::new(Box::new(Fixed(events.into())), &Cancel::new());
        self.turn = Some(new_turn(number, None, false, stream, Cancel::new()));
    }

    /// Applies one intent at once: `performer.intent`. It is checked, clamped and limited like any other.
    pub fn intent(&mut self, now: Seconds, intent: &Intent) {
        let turn = self.turn.as_ref().map_or(0, |t| t.number);
        let mut lines = Vec::new();
        let checked = self.check(turn, intent, &mut lines);
        for line in lines {
            self.push(now, line);
        }
        if let Some(intent) = checked.and_then(|i| self.admit_at(turn, &i, Source::Model, now)) {
            self.apply(turn, now, intent);
        }
    }

    /// Stops: cuts the turn and drops its cues (`performer.stop`, pause, revoke).
    pub fn stop(&mut self, now: Seconds) {
        self.end_turn(now, true);
        self.pending = None;
    }

    fn push(&mut self, at: Seconds, event: PerformerEvent) {
        self.out.push(TranscriptLine::new(ms(at), self.revision.clone(), event));
    }

    // ---- hearing ----

    fn hear(&mut self, now: Seconds) {
        while let Some(heard) = self.parts.ears.next() {
            match heard {
                Heard::Muted(on) => {
                    self.muted = on;
                    if on {
                        self.talking_since = None;
                    }
                }
                _ if self.muted => {}
                Heard::Onset { at, .. } => {
                    self.talking_since = Some(at);
                    self.last_heard = Some(at);
                }
                Heard::Held { .. } => self.barge_in(now),
                Heard::Rejected { .. } => {
                    self.talking_since = None;
                    self.resume_or_end(now);
                }
                Heard::Final { utterance, text, start, end, private, .. } => {
                    self.talking_since = None;
                    self.last_heard = Some(end);
                    self.heard_final(now, utterance, &text, start, end, private);
                }
                Heard::Partial { text, private, .. } => self.heard_partial(now, &text, private),
                Heard::Backchannel { .. } => self.listener_nod(now),
                _ => {}
            }
        }
    }

    /// The person's words so far, while they talk: a listening face from the decision layer, at most once per
    /// [`LISTEN_FACE_INTERVAL_S`]. Private words never reach the decision layer, and the face stays still while the
    /// performer itself is speaking.
    fn heard_partial(&mut self, now: Seconds, text: &str, private: bool) {
        if private || !self.config.reactions || self.talking_since.is_none() || self.speaking() {
            return;
        }
        if self.last_listen_face.is_some_and(|t| now.0 - t.0 < LISTEN_FACE_INTERVAL_S) || self.pending.is_some() {
            return;
        }
        self.last_listen_face = Some(now);
        self.ask(now, Moment::Listening, text.to_owned(), None);
    }

    /// A backchannel cue in the person's voice: a nod, while they talk and the performer doesn't, at most once per
    /// [`NOD_GAP_S`].
    fn listener_nod(&mut self, now: Seconds) {
        if !self.config.reactions || self.talking_since.is_none() || self.speaking() {
            return;
        }
        if self.last_nod.is_some_and(|t| now.0 - t.0 < NOD_GAP_S) || self.turn.as_ref().is_some_and(|t| t.private) {
            return;
        }
        self.last_nod = Some(now);
        let turn = self.turn.as_ref().map_or(0, |t| t.number);
        let nod = Intent::nod(BACKCHANNEL_NOD);
        let mut lines = Vec::new();
        let checked = self.check(turn, &nod, &mut lines);
        for line in lines {
            self.push(now, line);
        }
        if let Some(intent) = checked.and_then(|i| self.admit_at(turn, &i, Source::Reaction, now)) {
            self.apply(turn, now, intent);
        }
    }

    /// Whether the performer's own voice is playing now (not cut by the person).
    fn speaking(&self) -> bool {
        self.turn.as_ref().is_some_and(|t| t.held.is_none() && t.playing())
    }

    fn heard_final(&mut self, now: Seconds, utterance: u64, text: &str, start: Seconds, end: Seconds, private: bool) {
        if !private && self.is_own_echo(text, start, end) {
            self.echoes += 1;
            // What cut it in was its own voice: carry on.
            self.resume_or_end(now);
            return;
        }
        if self.turn.as_ref().is_some_and(|t| t.held.is_some()) {
            if is_backchannel(text) {
                let event = if private {
                    PerformerEvent::heard_private(utterance)
                } else {
                    PerformerEvent::heard(utterance, text, Some("backchannel".to_owned()))
                };
                self.push(now, event);
                self.resume_or_end(now);
                return;
            }
            self.end_turn(now, false);
        }
        let named = self.names.in_text(text);
        let follow = self.last_reply.and_then(|(ended, turn)| {
            let gap = start.0 - ended.0;
            (gap <= FOLLOW_UP_WINDOW_S).then(|| (gap.max(0.0), turn))
        });
        let shared_out = self.share(now) > self.config.talk_share;
        let (addressed, why) = match (self.config.respond, named, follow) {
            (Respond::ListenOnly, ..) => (false, Some("no: listen only".to_owned())),
            (_, true, _) => (true, None),
            (_, false, Some((gap, turn))) => {
                (true, Some(format!("follow-up, {} ms after turn {turn}", ms(Seconds(gap)))))
            }
            (Respond::Always, false, None) if !shared_out => (true, Some("always".to_owned())),
            (Respond::Always, false, None) => (false, Some("no: over its talk share".to_owned())),
            (_, false, None) => (
                false,
                Some(self.last_reply.map_or_else(
                    || "no: no name".to_owned(),
                    |(ended, turn)| format!("no: {} ms after turn {turn}, no name", ms(Seconds(now.0 - ended.0))),
                )),
            ),
        };
        let event = if private {
            PerformerEvent::heard_private(utterance)
        } else {
            PerformerEvent::heard(utterance, text, why)
        };
        self.push(now, event);
        if !private {
            self.remember(format!("person: {text}"));
        }
        if !addressed {
            return;
        }
        // A newer addressed input cancels the running turn (§5.6).
        self.end_turn(now, false);
        if !private {
            self.ask(now, Moment::Listening, text.to_owned(), None);
        }
        let number = self.begin(now, private);
        self.think(number, Some(text.to_owned()), private, false);
    }

    /// Whether a final is the performer's own voice: it overlaps the performer's speech and matches what it said.
    fn is_own_echo(&self, text: &str, start: Seconds, end: Seconds) -> bool {
        let said: Vec<&str> = self
            .spoken
            .iter()
            .filter(|(from, until, _)| *from <= end.0 + 0.25 && *until >= start.0 - 0.25)
            .map(|(.., t)| t.as_str())
            .collect();
        !said.is_empty() && is_echo(text, &said.join(" "))
    }

    fn barge_in(&mut self, now: Seconds) {
        let member = self.config.member.clone();
        let Some(turn) = self.turn.as_mut() else { return };
        if turn.held.is_some() || (!turn.playing() && turn.ready.is_empty()) {
            return;
        }
        for q in &turn.queued {
            q.handle.cut();
        }
        turn.held = Some(now);
        let number = turn.number;
        let private = turn.private;
        // Cues not yet played go; the ones of this turn wait to be timed again if it resumes.
        let mut back = Vec::new();
        self.scheduled.retain(|s| {
            let keep = s.turn != number || s.at.0 <= now.0;
            if !keep {
                back.push((s.offset, s.intent.clone()));
            }
            keep
        });
        if let Some(turn) = self.turn.as_mut() {
            turn.intents.extend(back);
            turn.intents.sort_by_key(|(o, _)| *o);
        }
        if !private {
            self.parts.stage.release(&member, now);
        }
        self.pending = None;
        self.push(now, PerformerEvent::held(number));
    }

    /// After a cut: resume once from the start of the cut sentence (`Rejected`, a backchannel, echo), or end the turn
    /// interrupted if it has resumed already. Nothing happens when no turn is held.
    fn resume_or_end(&mut self, now: Seconds) {
        let Some(turn) = self.turn.as_mut() else { return };
        if turn.held.is_none() {
            return;
        }
        if turn.resumed {
            self.end_turn(now, false);
            return;
        }
        self.settle_cut();
        let Some(turn) = self.turn.as_mut() else { return };
        turn.held = None;
        turn.resumed = true;
        // Every chunk from the first that didn't finish is spoken again, ahead of what still waits.
        let mut again: Vec<Chunk> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for q in &turn.queued {
            if !matches!(q.handle.state(), SpeechState::Done { .. }) && seen.insert(q.chunk.start) {
                again.push(q.chunk.clone());
            }
        }
        let done: std::collections::HashSet<u32> = turn
            .queued
            .iter()
            .filter(|q| matches!(q.handle.state(), SpeechState::Done { .. }))
            .map(|q| q.chunk.start)
            .collect();
        again.retain(|c| !done.contains(&c.start));
        for c in again.into_iter().rev() {
            turn.ready.push_front(c);
        }
        turn.waiting_since = None;
    }

    /// Folds the cut chunks' played characters into the turn.
    fn settle_cut(&mut self) {
        let Some(turn) = self.turn.as_mut() else { return };
        for q in &turn.queued {
            let played = match q.handle.state() {
                SpeechState::Done { .. } => q.chunk.text.chars().count(),
                SpeechState::Cut { spoken_chars, .. } => spoken_chars as usize,
                _ => 0,
            };
            if played > 0 {
                let upto = q.chunk.start.saturating_add(u32::try_from(played).unwrap_or(u32::MAX));
                turn.played_upto = turn.played_upto.max(upto);
            }
        }
    }

    // ---- thinking ----

    /// Opens turn `n`: refreshes the inventory and writes `thinking`.
    fn begin(&mut self, now: Seconds, private: bool) -> u64 {
        let number = self.next_turn;
        self.next_turn += 1;
        match self.parts.stage.inventory(&self.config.member) {
            Ok(inventory) => self.inventory = inventory,
            Err(e) => self.push(now, PerformerEvent::error(Some(number), e.to_string())),
        }
        self.push(now, PerformerEvent::thinking(number, None, Some(SafetyVerdict::Pass), private));
        number
    }

    fn think(&mut self, number: u64, heard: Option<String>, private: bool, proactive: bool) {
        let history: Vec<PastTurn> = self.history.iter().filter(|t| private || !t.private).cloned().collect();
        let mut input = TurnInput::new(number, self.inventory.clone()).history(history);
        if let Some(h) = &heard {
            input = input.heard(h.clone());
        }
        if proactive {
            input = input.proactive();
        }
        if private {
            input = input.private();
        }
        if let Some(cut) = self.interrupted.take()
            && (private || !cut.private)
        {
            input = input.interrupted(cut);
        }
        let cancel = Cancel::new();
        match self.parts.brain.turn(input, &cancel) {
            Ok(stream) => self.turn = Some(new_turn(number, heard, private, stream, cancel)),
            Err(e) => {
                let at = self.last_heard.unwrap_or_default();
                self.push(at, PerformerEvent::error(Some(number), e.to_string()));
            }
        }
    }

    /// Pulls what the brain has written: speech into chunks, intents checked, tags dropped with a reason.
    fn read(&mut self, now: Seconds) {
        let Some(mut turn) = self.turn.take() else { return };
        let mut lines = Vec::new();
        for _ in 0..EVENTS_PER_STEP {
            let Some(stream) = turn.stream.as_mut() else { break };
            match stream.try_next() {
                Ok(Polled::Event(event)) => self.take_event(&mut turn, event, &mut lines),
                Ok(Polled::End) => {
                    let rest = turn.chunker.finish();
                    turn.ready.extend(rest);
                    turn.stream = None;
                }
                Ok(_) => break,
                Err(BrainError::Cancelled) => turn.stream = None,
                Err(e) => {
                    lines.push(PerformerEvent::error(Some(turn.number), e.to_string()));
                    // What was complete still plays; the half sentence goes.
                    let _ = turn.chunker.finish();
                    turn.stream = None;
                }
            }
        }
        for line in lines {
            self.push(now, line);
        }
        self.turn = Some(turn);
    }

    fn take_event(&mut self, turn: &mut Turn, event: BrainEvent, lines: &mut Vec<PerformerEvent>) {
        match event {
            BrainEvent::Text(text) if !turn.passed && !turn.capped => {
                turn.text.push_str(&text);
                turn.ready.extend(turn.chunker.push(&text));
            }
            BrainEvent::Intent { intent, at_char, .. } if !turn.private && !turn.passed => {
                if let Some(intent) = self.check(turn.number, &intent, lines) {
                    if let Some(verb) = intent.verb().filter(|v| matches!(*v, "emote" | "gesture" | "clip")) {
                        turn.tagged.push((at_char, verb));
                    }
                    turn.intents.push((at_char, intent));
                }
            }
            BrainEvent::Style(style) => turn.style = style,
            BrainEvent::Pass => {
                turn.passed = true;
                turn.ready.clear();
                turn.intents.clear();
                turn.cancel.cancel();
                turn.stream = None;
            }
            BrainEvent::Issue(issue) if !turn.private => {
                let what = issue.run.trim_start_matches('[').trim_end_matches(']').to_owned();
                lines.push(PerformerEvent::filtered(turn.number, what, issue_why(issue.kind)));
            }
            _ => {}
        }
    }

    /// An intent checked against the inventory and clamped; a refusal is written and gives `None`.
    fn check(&self, turn: u64, intent: &Intent, lines: &mut Vec<PerformerEvent>) -> Option<Intent> {
        if let Err(why) = self.inventory.check(intent) {
            lines.push(PerformerEvent::filtered(turn, tag_text(intent), why.to_string()));
            return None;
        }
        Some(intent.clamped().0)
    }

    /// Hands ready chunks to the voice when the floor is free; drops a reply that waited 3 s.
    fn release_ready(&mut self, now: Seconds) {
        let talking = self.talking_since.is_some();
        let voice_id = self.config.voice.clone();
        let Some(turn) = self.turn.as_mut() else { return };
        if turn.held.is_some() || turn.ready.is_empty() {
            return;
        }
        if talking {
            let since = *turn.waiting_since.get_or_insert(now);
            if now.0 - since.0 > FLOOR_WAIT_S {
                let number = turn.number;
                self.push(now, PerformerEvent::filtered(number, "reply", "the person kept the floor"));
                self.end_turn(now, true);
            }
            return;
        }
        turn.waiting_since = None;
        let mut errors = Vec::new();
        while let Some(chunk) = turn.ready.pop_front() {
            if turn.capped || turn.estimated_s() >= TURN_SPEECH_S {
                // The sentence that crossed the cap was the last one (§5.1).
                turn.capped = true;
                turn.ready.clear();
                turn.cancel.cancel();
                turn.stream = None;
                break;
            }
            let how = if turn.private {
                SpeakHow::new(turn.style.clone()).private()
            } else {
                SpeakHow::new(turn.style.clone())
            };
            match self.parts.voice.speak(&chunk.text, &voice_id, &how) {
                Ok(handle) => turn.queued.push(Queued { chunk, handle, duration: None, from: None, reported: false }),
                Err(e) => errors.push(e.to_string()),
            }
        }
        let number = turn.number;
        for e in errors {
            self.push(now, PerformerEvent::error(Some(number), e));
        }
    }

    // ---- deciding ----

    /// Asks the decision layer about `line`, unless a decision is already on its way.
    fn ask(&mut self, now: Seconds, moment: Moment, line: String, chunk_start: Option<u32>) {
        if !self.config.reactions || self.pending.is_some() {
            return;
        }
        let questions = match moment {
            Moment::Listening => questions_listening(&self.inventory),
            _ => questions_speaking(&self.inventory),
        };
        let mut state: Vec<String> = self.context.iter().cloned().collect();
        if moment == Moment::Speaking {
            state.push(format!("{}: {line}", self.persona_name));
        } else {
            // The person's line, unless it is already the last one remembered (a final is remembered before it asks).
            let heard = format!("person: {line}");
            if state.last() != Some(&heard) {
                state.push(heard);
            }
        }
        let ask = Ask::new(moment, state.join("\n"), questions);
        let ticket = match self.parts.decider.as_mut() {
            Some(d) => d.ask(ask.clone()),
            None => DecisionTicket::ready(Ok(self.floor.answer(&ask))),
        };
        self.decisions.stats.asked += 1;
        let turn = self.turn.as_ref().map_or(self.next_turn, |t| t.number);
        self.pending = Some(Pending { ticket, ask, line, turn, asked_at: now, chunk_start });
    }

    /// Collects a decision: the provider's answers with the rule floor's filling any gap, or the floor alone when the
    /// provider failed or ran out of time.
    fn decide(&mut self, now: Seconds) {
        let Some(pending) = self.pending.as_ref() else { return };
        let floor = self.floor.answer(&pending.ask);
        let waited = now.0 - pending.asked_at.0;
        let decision = match pending.ticket.poll() {
            Some(Ok(d)) => {
                self.decisions.answered(waited.max(0.0) * 1000.0);
                with_floor(&pending.ask, d, &floor)
            }
            Some(Err(e)) => {
                self.decisions.stats.errors += 1;
                self.decisions.stats.last_error = Some(e.to_string());
                floor
            }
            // A hair under the timeout, so float rounding of step times never adds a step.
            None if waited >= DECISION_TIMEOUT.as_secs_f64() - 1e-6 => {
                self.decisions.stats.timeouts += 1;
                floor
            }
            None => return,
        };
        let Some(pending) = self.pending.take() else { return };
        let mut intents = self.config.gates.intents(&pending.ask, &decision, &pending.line);
        // The model's own tags on this line win over a reaction.
        if let (Some(start), Some(turn)) = (pending.chunk_start, self.turn.as_ref()) {
            let end =
                turn.queued.iter().find(|q| q.chunk.start == start).map_or(start, |q| q.chunk.start + q.chunk.len());
            let tagged: Vec<&str> =
                turn.tagged.iter().filter(|(o, _)| *o >= start && *o <= end).map(|(_, v)| *v).collect();
            intents.retain(|i| match i.verb() {
                Some("emote") => !tagged.contains(&"emote"),
                Some("gesture") => !tagged.contains(&"gesture") && !tagged.contains(&"clip"),
                _ => true,
            });
        }
        if self.turn.as_ref().is_some_and(|t| t.private && t.number == pending.turn) {
            return;
        }
        let mut lines = Vec::new();
        let checked: Vec<Intent> = intents.iter().filter_map(|i| self.check(pending.turn, i, &mut lines)).collect();
        for line in lines {
            self.push(now, line);
        }
        let damp = if self.speaking() { SPEAKING_DAMP } else { 1.0 };
        for intent in checked {
            let intent = match intent {
                Intent::Emote { expression, intensity, hold_s, .. } => {
                    Intent::emote(expression, intensity.min(REACTION_APEX) * damp, hold_s)
                }
                other => other,
            };
            if let Some(intent) = self.admit_at(pending.turn, &intent, Source::Reaction, now) {
                self.apply(pending.turn, now, intent);
            }
        }
    }

    // ---- playing ----

    fn play(&mut self, now: Seconds) {
        self.watch_chunks(now);
        self.fire(now);
        self.finish(now);
    }

    /// Reports chunks as they start, and times the intents on their words.
    fn watch_chunks(&mut self, now: Seconds) {
        let Some(mut turn) = self.turn.take() else { return };
        let mut started = Vec::new();
        let mut lines_said = Vec::new();
        for (i, q) in turn.queued.iter_mut().enumerate() {
            let state = q.handle.state();
            match state {
                SpeechState::Ready { duration, .. } | SpeechState::Playing { duration, .. } => {
                    q.duration = Some(duration);
                }
                _ => {}
            }
            let from = match state {
                SpeechState::Playing { from, .. } => Some(from),
                SpeechState::Done { at, .. } => Some(Seconds(at.0 - q.duration.map_or(0.0, |d| d.0))),
                SpeechState::Cut { at, spoken_chars, .. } if spoken_chars > 0 => {
                    Some(Seconds(at.0 - chars_s(spoken_chars as usize)))
                }
                _ => None,
            };
            if q.from.is_none() {
                q.from = from;
            }
            if let (Some(from), false) = (q.from, q.reported) {
                q.reported = true;
                started.push((i, from));
            }
        }
        for (i, from) in started {
            let q = &turn.queued[i];
            let duration = q.duration.unwrap_or_else(|| Seconds(chars_s(q.chunk.text.chars().count())));
            let index = u32::try_from(i).unwrap_or(u32::MAX);
            let event = if turn.private {
                PerformerEvent::speaking_private(turn.number, index)
            } else {
                PerformerEvent::speaking(turn.number, index, q.chunk.text.clone(), ms(duration))
            };
            self.push(from, event);
            let (start, len, text) = (q.chunk.start, q.chunk.len(), q.chunk.text.clone());
            self.spoken.push_back((from.0, from.0 + duration.0, text.clone()));
            // Intents up to this chunk's end, timed on their word.
            let mut keep = Vec::new();
            for (offset, intent) in turn.intents.drain(..) {
                if offset < start + len {
                    let frac = if len == 0 { 0.0 } else { f64::from(offset.saturating_sub(start)) / f64::from(len) };
                    let at = Seconds(from.0 + duration.0 * frac);
                    self.scheduled.push(Scheduled { turn: turn.number, offset, at, intent });
                } else {
                    keep.push((offset, intent));
                }
            }
            turn.intents = keep;
            if !turn.private {
                lines_said.push((text, start));
            }
        }
        // Tags after the last word play as it ends; tags in a reply with no words play now.
        if turn.stream.is_none() && turn.ready.is_empty() && turn.held.is_none() && !turn.intents.is_empty() {
            let all_started = turn.queued.iter().all(|q| q.reported);
            if all_started {
                let at = turn
                    .queued
                    .last()
                    .and_then(|q| q.from.map(|f| Seconds(f.0 + q.duration.map_or(0.0, |d| d.0))))
                    .unwrap_or(now);
                for (offset, intent) in turn.intents.drain(..) {
                    self.scheduled.push(Scheduled { turn: turn.number, offset, at, intent });
                }
            }
        }
        self.turn = Some(turn);
        for (text, start) in lines_said {
            self.remember(format!("{}: {text}", self.persona_name));
            self.ask(now, Moment::Speaking, text, Some(start));
        }
        self.trim_spoken(now);
    }

    /// Applies the intents whose moment has come.
    fn fire(&mut self, now: Seconds) {
        let mut due = Vec::new();
        self.scheduled.retain(|s| {
            let ready = s.at.0 <= now.0;
            if ready {
                due.push((s.turn, s.at, s.intent.clone()));
            }
            !ready
        });
        due.sort_by(|a, b| a.1.0.total_cmp(&b.1.0));
        for (turn, at, intent) in due {
            if let Some(intent) = self.admit_at(turn, &intent, Source::Model, at) {
                self.apply(turn, at, intent);
            }
        }
    }

    fn admit_at(&mut self, turn: u64, intent: &Intent, source: Source, at: Seconds) -> Option<Intent> {
        match self.limiter.admit(intent, source, at.0) {
            Ok(()) => Some(intent.clone()),
            Err(why) => {
                self.push(at, PerformerEvent::filtered(turn, tag_text(intent), why));
                None
            }
        }
    }

    fn apply(&mut self, turn: u64, at: Seconds, intent: Intent) {
        match self.parts.stage.apply(&self.config.member, &intent, at) {
            Ok(()) => self.push(at, PerformerEvent::intent(turn, intent)),
            Err(StageError::Refused(why)) => {
                self.push(at, PerformerEvent::filtered(turn, tag_text(&intent), why.to_string()));
            }
            Err(e) => self.push(at, PerformerEvent::filtered(turn, tag_text(&intent), e.to_string())),
        }
    }

    /// Ends the turn once it is read, played and nothing waits.
    fn finish(&mut self, now: Seconds) {
        let Some(turn) = self.turn.as_ref() else { return };
        if turn.reading() || !turn.ready.is_empty() || turn.held.is_some() || turn.playing() {
            return;
        }
        if !turn.intents.is_empty() {
            return;
        }
        self.settle_cut();
        let Some(mut turn) = self.turn.take() else { return };
        turn.ended = turn
            .queued
            .iter()
            .filter_map(|q| match q.handle.state() {
                SpeechState::Done { at, .. } | SpeechState::Cut { at, .. } => Some(at),
                _ => None,
            })
            .max_by(|a, b| a.0.total_cmp(&b.0));
        let ended = turn.ended.unwrap_or(now);
        if !turn.queued.is_empty() {
            self.last_reply = Some((ended, turn.number));
        }
        let mut past = PastTurn::new(turn.heard.clone(), turn.said());
        if turn.private {
            past = past.private();
        }
        self.history.push(past);
    }

    /// Ends the running turn now: cuts its speech, drops its cues and, if any of it played, writes `interrupted`.
    /// `quiet` skips the line (a stop, a dropped reply).
    fn end_turn(&mut self, now: Seconds, quiet: bool) {
        let Some(turn) = self.turn.as_ref() else { return };
        turn.cancel.cancel();
        for q in &turn.queued {
            q.handle.cut();
        }
        let (number, private) = (turn.number, turn.private);
        self.scheduled.retain(|s| s.turn != number || s.at.0 <= now.0);
        if !private {
            self.parts.stage.release(&self.config.member, now);
        }
        self.settle_cut();
        let Some(turn) = self.turn.take() else { return };
        let spoken = turn.played_upto;
        if !quiet {
            self.push(now, PerformerEvent::interrupted(number, spoken));
        }
        let cut_at = turn.held.unwrap_or(now);
        if !turn.queued.is_empty() {
            self.last_reply = Some((cut_at, number));
        }
        let mut past = PastTurn::new(turn.heard.clone(), turn.said()).interrupted(spoken);
        if private {
            past = past.private();
        }
        self.history.push(past.clone());
        self.interrupted = Some(past);
    }

    // ---- proactive ----

    fn proact(&mut self, now: Seconds) {
        if !self.config.proactive
            || self.config.respond == Respond::ListenOnly
            || self.turn.is_some()
            || self.muted
            || self.talking_since.is_some()
            || self.share(now) > self.config.talk_share
        {
            return;
        }
        let quiet_since = [self.last_heard, self.last_reply.map(|(t, _)| t)]
            .into_iter()
            .flatten()
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .unwrap_or_default();
        if now.0 - quiet_since.0 < PROACTIVE_SILENCE_S
            || self.last_proactive.is_some_and(|t| now.0 - t.0 < PROACTIVE_INTERVAL_S)
        {
            return;
        }
        self.last_proactive = Some(now);
        let number = self.begin(now, false);
        self.think(number, None, false, true);
    }

    // ---- bookkeeping ----

    fn remember(&mut self, line: String) {
        self.context.push_back(line);
        while self.context.len() > CONTEXT_LINES {
            self.context.pop_front();
        }
    }

    fn trim_spoken(&mut self, now: Seconds) {
        let keep = TALK_SHARE_WINDOW_S.max(ECHO_MEMORY_S);
        while self.spoken.front().is_some_and(|(_, end, _)| now.0 - end > keep) {
            self.spoken.pop_front();
        }
    }

    /// The performer's share of speech in the last five minutes.
    fn share(&self, now: Seconds) -> f32 {
        let lo = now.0 - TALK_SHARE_WINDOW_S;
        let talked: f64 = self.spoken.iter().map(|(from, end, _)| (end.min(now.0) - from.max(lo)).max(0.0)).sum();
        // A ratio in 0..=1.
        #[allow(clippy::cast_possible_truncation)]
        let share = (talked / TALK_SHARE_WINDOW_S) as f32;
        share
    }
}

impl std::fmt::Debug for Show {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Show")
            .field("persona", &self.persona_name)
            .field("member", &self.config.member)
            .field("turn", &self.turn.as_ref().map(|t| t.number))
            .finish_non_exhaustive()
    }
}

fn new_turn(number: u64, heard: Option<String>, private: bool, stream: TurnStream, cancel: Cancel) -> Turn {
    Turn {
        number,
        private,
        heard,
        cancel,
        stream: Some(stream),
        chunker: Chunker::new(),
        style: Vec::new(),
        text: String::new(),
        ready: VecDeque::new(),
        queued: Vec::new(),
        intents: Vec::new(),
        tagged: Vec::new(),
        waiting_since: None,
        capped: false,
        passed: false,
        held: None,
        resumed: false,
        played_upto: 0,
        ended: None,
    }
}

/// A reply already written (`performer.say`).
struct Fixed(VecDeque<BrainEvent>);

impl BrainSource for Fixed {
    fn poll(&mut self) -> Result<Polled, BrainError> {
        Ok(self.0.pop_front().map_or(Polled::End, Polled::Event))
    }
}

/// The tag that writes `intent`, without brackets, as `filtered` lines show it ("clip backflip").
pub fn tag_text(intent: &Intent) -> String {
    match intent {
        Intent::Emote { expression, .. } => format!("emote {expression}"),
        Intent::Gesture { clip, .. } => format!("gesture {clip}"),
        Intent::PlayClip { clip, .. } => format!("clip {clip}"),
        Intent::LookAt { target, .. } => format!("look {}", target.as_text()),
        Intent::MoveTo { place, .. } => format!("move {}", place.as_text()),
        _ => intent.verb().unwrap_or("speak").to_owned(),
    }
}

fn issue_why(kind: omuzaki_performer_api::brain::ParseIssueKind) -> &'static str {
    use omuzaki_performer_api::brain::ParseIssueKind as K;
    match kind {
        K::UnknownVerb => "unknown verb",
        K::BadArguments => "bad arguments",
        K::TooLong => "too long",
        K::Unclosed => "unclosed",
        _ => "malformed",
    }
}

fn chars_s(chars: usize) -> f64 {
    // Chunk lengths are at most a few hundred characters.
    #[allow(clippy::cast_precision_loss)]
    let s = chars as f64 * EST_S_PER_CHAR;
    s
}

/// Seconds to whole milliseconds, for transcript lines.
pub fn ms(s: Seconds) -> u64 {
    // Runner times are non-negative and far below 2^53 ms.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = (s.0.max(0.0) * 1000.0).round() as u64;
    out
}
