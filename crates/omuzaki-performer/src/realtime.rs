//! The realtime turn loop (#2347, AP-02, AP-03): the performer over a full-duplex speech-to-speech provider
//! ([`Duplex`]), a bring-your-own-key option beside the cascaded [`Show`](crate::show::Show), which stays the default.
//!
//! The provider hears the microphone and decides when the person has finished; this loop keeps the rest under the
//! same rules as the cascaded path:
//!
//! - **The floor and barge-in.** When the person starts talking over a reply, the reply's speech is cut at once and
//!   the provider is told how much was heard (`cancel` with the played milliseconds), so its memory matches the
//!   room's.
//! - **When it answers.** `always` lets the provider answer by itself (the fastest path), unless the talk share is
//!   spent; `addressed` asks for a reply only when a line names the character or follows its last reply within the
//!   follow-up window; `listen_only` never answers.
//! - **Talk-time limits.** A reply stops at [`TURN_SPEECH_S`] of speech; over [`ShowConfig::talk_share`] only
//!   addressed lines get a reply.
//! - **Money.** Each reply reserves its bound on the [`SpendGate`] before a sample plays and settles with the
//!   provider's bill; a refused reservation cancels the reply and turns automatic replies off.
//! - **Face and body.** A speech-to-speech model writes no tags, so the decision layer reads the live transcript: a
//!   face for each line the person says and for each sentence the character says, through the same gates and limits.
//! - **Private turns.** A line heard in `CohostOnly` gets a reply on the private feed, and its transcript carries no
//!   text.

use std::collections::VecDeque;
use std::sync::Arc;

use omuzaki_core::time::Seconds;
use omuzaki_llm_api::usage::MicroUsd;
use omuzaki_ops_api::Spend;
use omuzaki_performer_api::decide::{Ask, DECISION_TIMEOUT, Decider, DecisionTicket, Moment};
use omuzaki_performer_api::duplex::{Duplex, DuplexEvent, DuplexUsage, PcmVoice};
use omuzaki_performer_api::event::{PerformerEvent, SafetyVerdict, TranscriptLine};
use omuzaki_performer_api::ids::PersonaRevision;
use omuzaki_performer_api::intent::Intent;
use omuzaki_performer_api::limits::{FOLLOW_UP_WINDOW_S, TALK_SHARE_WINDOW_S, TURN_SPEECH_S};
use omuzaki_performer_api::speech::{SpeechHandle, SpeechState};
use omuzaki_performer_api::spend::{Reservation, SpendGate};
use omuzaki_performer_api::stage::{Inventory, Stage, StageError};

use crate::addressing::{Names, Respond};
use crate::chunker::Chunker;
use crate::decide::{RuleFloor, questions_listening, questions_speaking, with_floor};
use crate::parser::parse_reply;
use crate::persona::Persona;
use crate::show::{
    CONTEXT_LINES, Decisions, EST_S_PER_CHAR, Limiter, Performer, Phase, ShowConfig, Source, Status, ms, tag_text,
};

/// Input tokens a reply's bound assumes when no bill has come yet: a minute of the person's speech.
const FIRST_CONTEXT_TOKENS: u64 = 600;
/// Ended replies whose bill hasn't come yet, kept to settle; an older one is charged its full bound.
const SETTLING_KEPT: usize = 4;

/// The parts the realtime loop runs over.
#[non_exhaustive]
pub struct RealtimeParts {
    /// The provider: ears, brain and voice in one.
    pub duplex: Box<dyn Duplex>,
    /// Plays the reply's audio (the TTS host's feeds, cut and mouth drive).
    pub voice: Box<dyn PcmVoice>,
    /// Takes the intents.
    pub stage: Arc<dyn Stage>,
    /// Answers the decision layer's questions; `None` uses the rule floor alone.
    pub decider: Option<Box<dyn Decider>>,
    /// Where the money is reserved and billed; `None` bills nothing (a provider on this machine).
    pub spend: Option<SpendGate>,
}

impl RealtimeParts {
    /// The parts, with the rule floor and no spend gate.
    pub fn new(duplex: Box<dyn Duplex>, voice: Box<dyn PcmVoice>, stage: Arc<dyn Stage>) -> Self {
        Self { duplex, voice, stage, decider: None, spend: None }
    }

    /// The same, with `decider` asked first.
    #[must_use]
    pub fn decider(mut self, decider: Box<dyn Decider>) -> Self {
        self.decider = Some(decider);
        self
    }

    /// The same, billing through `gate`.
    #[must_use]
    pub fn spend(mut self, gate: SpendGate) -> Self {
        self.spend = Some(gate);
        self
    }
}

/// One reply as it streams.
struct Reply {
    id: String,
    number: u64,
    private: bool,
    /// The output item its audio belongs to, for a truncation.
    item: Option<String>,
    handles: Vec<SpeechHandle>,
    /// Seconds of audio received.
    audio_s: f64,
    /// When its first sample entered the bus.
    from: Option<Seconds>,
    /// The transcript so far, and the sentences found in it.
    text: String,
    chunker: Chunker,
    chunks: u32,
    reservation: Option<Reservation>,
    /// Takes no more audio: the provider finished it, or it reached the talk-time limit. It still plays out.
    finished: bool,
    /// Cut: by the person, `performer.stop` or a newer reply.
    cut: bool,
}

impl Reply {
    fn playing(&self) -> bool {
        self.handles.iter().any(|h| !h.state().is_final())
    }

    /// Milliseconds the person heard by `now`.
    fn played_ms(&self, now: Seconds) -> u64 {
        let heard = self.from.map_or(0.0, |from| (now.0 - from.0).clamp(0.0, self.audio_s));
        ms(Seconds(heard))
    }
}

/// A decision on its way.
struct Pending {
    ticket: DecisionTicket,
    ask: Ask,
    line: String,
    turn: u64,
    asked_at: Seconds,
}

/// The realtime turn loop: see the module docs.
pub struct RealtimeShow {
    parts: RealtimeParts,
    config: ShowConfig,
    persona_name: String,
    names: Names,
    revision: PersonaRevision,
    floor: RuleFloor,
    inventory: Inventory,
    out: Vec<TranscriptLine>,
    next_turn: u64,
    next_utterance: u64,
    reply: Option<Reply>,
    /// The id of the last reply cut or refused: its late audio and text are dropped.
    dropped: Option<String>,
    pending: Option<Pending>,
    /// Replies cut before their bill came: settled when it does.
    settling: VecDeque<(String, Reservation)>,
    decisions: Decisions,
    limiter: Limiter,
    /// The next reply goes to the private feed.
    private_next: bool,
    last_reply: Option<(Seconds, u64)>,
    context: VecDeque<String>,
    /// Its speech: `(from, end)`, for the talk share.
    spoken: VecDeque<(f64, f64)>,
    /// Whether the provider answers by itself now.
    auto: Option<bool>,
    /// The gate refused a reply: no more replies this run.
    broke: bool,
    /// The provider's session ended.
    closed: bool,
    last_input_tokens: u64,
    spent: MicroUsd,
    /// The time of the last step.
    now: Seconds,
}

impl RealtimeShow {
    /// A realtime show of `persona` over `parts`.
    pub fn new(persona: &Persona, parts: RealtimeParts, config: ShowConfig) -> Self {
        let inventory = parts.stage.inventory(&config.member).unwrap_or_default();
        let mut decisions = Decisions::default();
        parts.decider.as_ref().map_or("rules", |d| d.kind()).clone_into(&mut decisions.stats.provider);
        let mut show = Self {
            parts,
            config,
            persona_name: persona.name.clone(),
            names: Names::new(&persona.names()),
            revision: persona.revision(),
            floor: RuleFloor,
            inventory,
            out: Vec::new(),
            next_turn: 1,
            next_utterance: 0,
            reply: None,
            dropped: None,
            pending: None,
            settling: VecDeque::new(),
            decisions,
            limiter: Limiter::default(),
            private_next: false,
            last_reply: None,
            context: VecDeque::new(),
            spoken: VecDeque::new(),
            auto: None,
            broke: false,
            closed: false,
            last_input_tokens: FIRST_CONTEXT_TOKENS,
            spent: MicroUsd(0),
            now: Seconds(0.0),
        };
        show.sync_auto(Seconds(0.0));
        show
    }

    fn push(&mut self, at: Seconds, event: PerformerEvent) {
        self.out.push(TranscriptLine::new(ms(at), self.revision.clone(), event));
    }

    /// Whether the provider should answer by itself: `always`, within the talk share, with money left.
    fn wants_auto(&self, now: Seconds) -> bool {
        self.config.respond == Respond::Always && !self.broke && self.share(now) <= self.config.talk_share
    }

    fn sync_auto(&mut self, now: Seconds) {
        let want = self.wants_auto(now);
        if self.auto != Some(want) && !self.closed {
            match self.parts.duplex.auto_reply(want) {
                Ok(()) => self.auto = Some(want),
                Err(e) => self.push(now, PerformerEvent::error(None, e.to_string())),
            }
        }
    }

    // ---- hearing ----

    fn take(&mut self, now: Seconds, event: DuplexEvent) {
        match event {
            DuplexEvent::SpeechStarted => self.barge_in(now),
            DuplexEvent::Heard { text, private, .. } => self.heard(now, &text, private),
            DuplexEvent::ReplyStarted { reply, .. } => self.begin(now, reply),
            DuplexEvent::ReplyAudio { reply, item, pcm, rate, .. } => self.audio(now, &reply, item, &pcm, rate),
            DuplexEvent::ReplyText { reply, delta, .. } => self.text(now, &reply, &delta),
            DuplexEvent::ReplyDone { reply, cancelled, usage, .. } => self.done(now, &reply, cancelled, usage),
            DuplexEvent::Error { message, fatal, .. } => {
                let turn = self.reply.as_ref().map(|r| r.number);
                self.push(now, PerformerEvent::error(turn, message));
                if fatal {
                    self.closed = true;
                    self.end_reply(now, true);
                }
            }
            _ => {}
        }
    }

    fn heard(&mut self, now: Seconds, text: &str, private: bool) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let utterance = self.next_utterance;
        self.next_utterance += 1;
        let named = self.names.in_text(text);
        let follow = self.last_reply.and_then(|(ended, turn)| {
            let gap = now.0 - ended.0;
            (gap <= FOLLOW_UP_WINDOW_S).then(|| (gap.max(0.0), turn))
        });
        let auto = self.auto == Some(true);
        let (addressed, why) = match (self.config.respond, named, follow) {
            (Respond::ListenOnly, ..) => (false, Some("no: listen only".to_owned())),
            _ if self.broke => (false, Some("no: the spending cap is reached".to_owned())),
            (_, true, _) => (true, None),
            (_, false, Some((gap, turn))) => {
                (true, Some(format!("follow-up, {} ms after turn {turn}", ms(Seconds(gap)))))
            }
            (Respond::Always, false, None) if auto => (true, Some("always".to_owned())),
            (Respond::Always, false, None) => (false, Some("no: over its talk share".to_owned())),
            (_, false, None) => (false, Some("no: no name".to_owned())),
        };
        let event = if private {
            PerformerEvent::heard_private(utterance)
        } else {
            PerformerEvent::heard(utterance, text, why)
        };
        self.push(now, event);
        self.private_next = private;
        if !private {
            self.remember(format!("person: {text}"));
            self.ask(now, Moment::Listening, text.to_owned());
        }
        // The provider answers by itself in `always`; otherwise this loop asks once it knows who was addressed.
        if addressed
            && !auto
            && self.reply.is_none()
            && let Err(e) = self.parts.duplex.reply(None)
        {
            self.push(now, PerformerEvent::error(None, e.to_string()));
        }
    }

    fn barge_in(&mut self, now: Seconds) {
        let Some(reply) = self.reply.as_mut() else { return };
        if reply.cut || (reply.finished && !reply.playing()) {
            return;
        }
        let played = reply.played_ms(now);
        for h in &reply.handles {
            h.cut();
        }
        reply.cut = true;
        let (id, item, number, private) = (reply.id.clone(), reply.item.clone(), reply.number, reply.private);
        let spoken = Self::spoken_chars(reply, played);
        if let Err(e) = self.parts.duplex.cancel(&id, item.as_deref(), played) {
            self.push(now, PerformerEvent::error(Some(number), e.to_string()));
        }
        if !private {
            self.parts.stage.release(&self.config.member, now);
        }
        self.pending = None;
        self.push(now, PerformerEvent::held(number));
        self.push(now, PerformerEvent::interrupted(number, spoken));
        self.dropped = Some(id);
        self.end_reply(now, false);
    }

    /// The characters of the transcript heard by `played` ms, in proportion to the audio.
    fn spoken_chars(reply: &Reply, played: u64) -> u32 {
        if reply.audio_s <= 0.0 {
            return 0;
        }
        // Milliseconds of a reply at most a few minutes long, and its character count.
        #[allow(clippy::cast_precision_loss)]
        let share = (played as f64 / 1000.0 / reply.audio_s).clamp(0.0, 1.0);
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let chars = (reply.text.chars().count() as f64 * share).floor() as u32;
        chars
    }

    // ---- replying ----

    fn begin(&mut self, now: Seconds, id: String) {
        if self.reply.as_ref().is_some_and(|r| r.id == id) {
            return;
        }
        // A new reply ends any other.
        self.end_reply(now, true);
        let number = self.next_turn;
        self.next_turn += 1;
        let private = std::mem::take(&mut self.private_next);
        let reservation = match &self.parts.spend {
            None => None,
            Some(gate) => {
                let bound = self.parts.duplex.prices().reply_bound(TURN_SPEECH_S, self.last_input_tokens);
                match gate.reserve(bound) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        self.broke = true;
                        let _ = self.parts.duplex.cancel(&id, None, 0);
                        self.push(now, PerformerEvent::filtered(number, "reply", e.to_string()));
                        self.dropped = Some(id);
                        self.sync_auto(now);
                        return;
                    }
                }
            }
        };
        let thinking = PerformerEvent::thinking(number, None, Some(SafetyVerdict::Pass), private);
        self.push(now, thinking);
        self.reply = Some(Reply {
            id,
            number,
            private,
            item: None,
            handles: Vec::new(),
            audio_s: 0.0,
            from: None,
            text: String::new(),
            chunker: Chunker::new(),
            chunks: 0,
            reservation,
            finished: false,
            cut: false,
        });
    }

    fn audio(&mut self, now: Seconds, id: &str, item: String, pcm: &[f32], rate: u32) {
        if self.dropped.as_deref() == Some(id) || pcm.is_empty() {
            return;
        }
        if self.reply.as_ref().is_none_or(|r| r.id != id) {
            // Audio before (or without) its start: the reply started.
            self.begin(now, id.to_owned());
        }
        let voice_rate = self.parts.voice.rate();
        let Some(reply) = self.reply.as_mut() else { return };
        if reply.cut || reply.finished {
            return;
        }
        reply.item = Some(item);
        let pcm = resample(pcm, rate, voice_rate);
        // Sample counts of one delta.
        #[allow(clippy::cast_precision_loss)]
        let seconds = pcm.len() as f64 / f64::from(voice_rate.max(1));
        if reply.audio_s + seconds > TURN_SPEECH_S {
            // The sentence that crossed the limit is the last one (§5.1).
            reply.finished = true;
            let (rid, item, number) = (reply.id.clone(), reply.item.clone(), reply.number);
            let played = ms(Seconds(reply.audio_s));
            if let Err(e) = self.parts.duplex.cancel(&rid, item.as_deref(), played) {
                self.push(now, PerformerEvent::error(Some(number), e.to_string()));
            }
            self.push(now, PerformerEvent::filtered(number, "reply", "over the talk-time limit"));
            self.dropped = Some(rid);
            return;
        }
        match self.parts.voice.play(pcm, reply.private) {
            Ok(handle) => {
                reply.handles.push(handle);
                reply.audio_s += seconds;
            }
            Err(e) => {
                let number = reply.number;
                self.push(now, PerformerEvent::error(Some(number), e.to_string()));
            }
        }
    }

    fn text(&mut self, now: Seconds, id: &str, delta: &str) {
        if self.dropped.as_deref() == Some(id) {
            return;
        }
        let Some(reply) = self.reply.as_mut().filter(|r| r.id == id) else { return };
        reply.text.push_str(delta);
        let chunks = reply.chunker.push(delta);
        for chunk in chunks {
            self.sentence(now, &chunk.text);
        }
    }

    /// One sentence of the reply's transcript: its `speaking` line and a face for it.
    fn sentence(&mut self, now: Seconds, text: &str) {
        let Some(reply) = self.reply.as_mut() else { return };
        let (number, chunk, private) = (reply.number, reply.chunks, reply.private);
        reply.chunks += 1;
        // An estimate: the provider sends text and audio separately.
        // A sentence's character count.
        #[allow(clippy::cast_precision_loss)]
        let estimate = ms(Seconds(text.chars().count() as f64 * EST_S_PER_CHAR));
        if private {
            self.push(now, PerformerEvent::speaking_private(number, chunk));
            return;
        }
        self.push(now, PerformerEvent::speaking(number, chunk, text.to_owned(), estimate));
        self.ask(now, Moment::Speaking, text.to_owned());
        self.remember(format!("{}: {text}", self.persona_name));
    }

    fn done(&mut self, now: Seconds, id: &str, cancelled: bool, usage: Option<DuplexUsage>) {
        if let Some(usage) = usage {
            self.last_input_tokens = usage.input_audio + usage.input_text + usage.cached_input;
        }
        let ours = self.reply.as_ref().is_some_and(|r| r.id == id);
        if !ours {
            // A cut or refused reply's bill still counts.
            let reservation = self.settling.iter().position(|(r, _)| r == id).and_then(|i| self.settling.remove(i));
            match reservation {
                Some((_, reservation)) => self.settle(now, None, reservation, usage),
                None => {
                    if let Some(usage) = usage {
                        self.spent = MicroUsd(self.spent.0.saturating_add(self.parts.duplex.prices().cost(&usage).0));
                    }
                }
            }
            return;
        }
        let Some(reply) = self.reply.as_mut() else { return };
        let (reservation, rest, number) = (reply.reservation.take(), reply.chunker.finish(), reply.number);
        // It is over once its audio has played; until then it still holds the floor.
        reply.finished = true;
        if let Some(reservation) = reservation {
            self.settle(now, Some(number), reservation, usage);
        }
        if !cancelled {
            for chunk in rest {
                self.sentence(now, &chunk.text);
            }
        }
    }

    /// Bills a reply: its usage when the provider said, else its whole bound.
    fn settle(&mut self, now: Seconds, turn: Option<u64>, reservation: Reservation, usage: Option<DuplexUsage>) {
        let settled = match usage {
            Some(usage) => {
                let cost = self.parts.duplex.prices().cost(&usage);
                self.spent = MicroUsd(self.spent.0.saturating_add(cost.0));
                reservation.settle(Spend { usd_micros: cost.0, tokens: usage.tokens(), seconds: 0 })
            }
            None => self.forfeit(reservation),
        };
        if let Err(e) = settled {
            self.push(now, PerformerEvent::error(turn, e.to_string()));
        }
    }

    fn forfeit(&mut self, reservation: Reservation) -> Result<(), omuzaki_performer_api::spend::SpendError> {
        self.spent = MicroUsd(self.spent.0.saturating_add(reservation.bound().0));
        reservation.forfeit()
    }

    /// Ends the reply: its audio is cut when `cut`, its speech counted, and the floor freed. Its bill, if it hasn't
    /// come, settles when it does.
    fn end_reply(&mut self, now: Seconds, cut: bool) {
        let Some(reply) = self.reply.take() else { return };
        if cut {
            for h in &reply.handles {
                h.cut();
            }
        }
        if let Some(from) = reply.from {
            self.spoken.push_back((from.0, now.0.min(from.0 + reply.audio_s)));
        }
        if let Some(reservation) = reply.reservation {
            self.settling.push_back((reply.id, reservation));
            while self.settling.len() > SETTLING_KEPT {
                if let Some((_, old)) = self.settling.pop_front() {
                    let _ = self.forfeit(old);
                }
            }
        }
        self.last_reply = Some((now, reply.number));
    }

    /// Notes when the reply started playing, and ends it once its audio has played out.
    fn watch(&mut self, now: Seconds) {
        let Some(reply) = self.reply.as_mut() else { return };
        if reply.from.is_none() {
            reply.from = reply.handles.iter().find_map(|h| match h.state() {
                SpeechState::Playing { from, .. } => Some(from),
                _ => None,
            });
        }
        if (reply.finished || reply.cut) && !reply.playing() {
            self.end_reply(now, false);
        }
    }

    // ---- deciding ----

    fn ask(&mut self, now: Seconds, moment: Moment, line: String) {
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
        }
        let ask = Ask::new(moment, state.join("\n"), questions);
        let ticket = match self.parts.decider.as_mut() {
            Some(d) => d.ask(ask.clone()),
            None => DecisionTicket::ready(Ok(self.floor.answer(&ask))),
        };
        self.decisions.stats.asked += 1;
        let turn = self.reply.as_ref().map_or(self.next_turn, |r| r.number);
        self.pending = Some(Pending { ticket, ask, line, turn, asked_at: now });
    }

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
            None if waited >= DECISION_TIMEOUT.as_secs_f64() - 1e-6 => {
                self.decisions.stats.timeouts += 1;
                floor
            }
            None => return,
        };
        let Some(pending) = self.pending.take() else { return };
        if self.reply.as_ref().is_some_and(|r| r.private && r.number == pending.turn) {
            return;
        }
        let intents = self.config.gates.intents(&pending.ask, &decision, &pending.line);
        for intent in intents {
            self.act(now, pending.turn, &intent, Source::Reaction);
        }
    }

    /// Checks, limits and applies one intent.
    fn act(&mut self, now: Seconds, turn: u64, intent: &Intent, source: Source) {
        if let Err(why) = self.inventory.check(intent) {
            self.push(now, PerformerEvent::filtered(turn, tag_text(intent), why.to_string()));
            return;
        }
        let intent = intent.clamped().0;
        if let Err(why) = self.limiter.admit(&intent, source, now.0) {
            self.push(now, PerformerEvent::filtered(turn, tag_text(&intent), why));
            return;
        }
        match self.parts.stage.apply(&self.config.member, &intent, now) {
            Ok(()) => self.push(now, PerformerEvent::intent(turn, intent)),
            Err(StageError::Refused(why)) => {
                self.push(now, PerformerEvent::filtered(turn, tag_text(&intent), why.to_string()));
            }
            Err(e) => self.push(now, PerformerEvent::filtered(turn, tag_text(&intent), e.to_string())),
        }
    }

    // ---- bookkeeping ----

    fn remember(&mut self, line: String) {
        self.context.push_back(line);
        while self.context.len() > CONTEXT_LINES {
            self.context.pop_front();
        }
    }

    fn share(&self, now: Seconds) -> f32 {
        let lo = now.0 - TALK_SHARE_WINDOW_S;
        let mut talked: f64 = self.spoken.iter().map(|(from, end)| (end.min(now.0) - from.max(lo)).max(0.0)).sum();
        if let Some(from) = self.reply.as_ref().and_then(|r| r.from) {
            talked += (now.0 - from.0.max(lo)).max(0.0);
        }
        // A ratio in 0..=1.
        #[allow(clippy::cast_possible_truncation)]
        let share = (talked / TALK_SHARE_WINDOW_S) as f32;
        share
    }

    /// What it has spent so far.
    pub fn spent(&self) -> MicroUsd {
        self.spent
    }
}

/// `pcm` at `from` Hz as `to` Hz, by linear interpolation (the provider and the feed share 24 kHz, so usually a copy).
fn resample(pcm: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || from == 0 || to == 0 || pcm.is_empty() {
        return pcm.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    // Sample counts of one delta.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (pcm.len() as f64 / ratio).floor() as usize;
    (0..n)
        .map(|i| {
            // Positions inside one delta.
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let (at, frac) = {
                let x = i as f64 * ratio;
                (x.floor() as usize, (x - x.floor()) as f32)
            };
            let a = pcm.get(at).copied().unwrap_or(0.0);
            let b = pcm.get(at + 1).copied().unwrap_or(a);
            a + (b - a) * frac
        })
        .collect()
}

impl Performer for RealtimeShow {
    fn step(&mut self, now: Seconds) {
        self.now = now;
        let events = if self.closed { Vec::new() } else { self.parts.duplex.poll() };
        for event in events {
            self.take(now, event);
        }
        self.watch(now);
        self.decide(now);
        while self.spoken.front().is_some_and(|(_, end)| now.0 - end > TALK_SHARE_WINDOW_S) {
            self.spoken.pop_front();
        }
        self.sync_auto(now);
    }

    fn say(&mut self, now: Seconds, text: &str) {
        let (speech, events) = parse_reply(text);
        let turn = self.reply.as_ref().map_or(self.next_turn, |r| r.number);
        for event in events {
            if let omuzaki_performer_api::brain::BrainEvent::Intent { intent, .. } = event {
                self.act(now, turn, &intent, Source::Model);
            }
        }
        let speech = speech.trim();
        if speech.is_empty() || self.closed {
            return;
        }
        self.end_reply(now, true);
        let line = format!("Say exactly this, in your own voice, and nothing else: {speech}");
        if let Err(e) = self.parts.duplex.reply(Some(&line)) {
            self.push(now, PerformerEvent::error(None, e.to_string()));
        }
    }

    fn intent(&mut self, now: Seconds, intent: &Intent) {
        let turn = self.reply.as_ref().map_or(0, |r| r.number);
        self.act(now, turn, intent, Source::Model);
    }

    fn set_respond(&mut self, respond: Respond) {
        self.config.respond = respond;
        self.sync_auto(self.now);
    }

    fn stop(&mut self, now: Seconds) {
        if let Some(reply) = self.reply.as_ref() {
            let (id, item, played) = (reply.id.clone(), reply.item.clone(), reply.played_ms(now));
            let _ = self.parts.duplex.cancel(&id, item.as_deref(), played);
            self.dropped = Some(id);
        }
        self.end_reply(now, true);
        self.pending = None;
        // The session closes: no bill will come for what is outstanding.
        while let Some((_, reservation)) = self.settling.pop_front() {
            let _ = self.forfeit(reservation);
        }
        let _ = self.parts.duplex.auto_reply(false);
        self.auto = Some(false);
        self.parts.duplex.close();
        self.closed = true;
    }

    fn status(&self, now: Seconds) -> Status {
        let phase = match &self.reply {
            None => Phase::Idle,
            Some(r) if r.from.is_some() && r.playing() => Phase::Speaking,
            Some(_) => Phase::Thinking,
        };
        Status {
            phase,
            turn: self.reply.as_ref().map(|r| r.number),
            turns: self.next_turn - 1,
            echoes: 0,
            share: self.share(now),
            muted: false,
            decisions: self.decisions.report(),
            spent_micro_usd: self.spent.0,
        }
    }

    fn drain(&mut self) -> Vec<TranscriptLine> {
        std::mem::take(&mut self.out)
    }
}

impl std::fmt::Debug for RealtimeShow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeShow")
            .field("persona", &self.persona_name)
            .field("provider", &self.parts.duplex.kind())
            .field("turn", &self.reply.as_ref().map(|r| r.number))
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_the_duration() {
        let pcm: Vec<f32> = (0..2_400u16).map(|i| (f32::from(i) * 0.01).sin()).collect();
        assert_eq!(resample(&pcm, 24_000, 24_000).len(), 2_400);
        assert_eq!(resample(&pcm, 24_000, 48_000).len(), 4_800);
        assert_eq!(resample(&pcm, 24_000, 16_000).len(), 1_600);
    }
}
