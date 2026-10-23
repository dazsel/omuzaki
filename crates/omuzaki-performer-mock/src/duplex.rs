//! Realtime fakes (#2347): [`FakeDuplex`], a speech-to-speech provider that plays a script on a [`FakeClock`] and
//! answers with canned replies, and [`FakePcmVoice`], which plays PCM back to back on the same clock.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_performer_api::duplex::{Duplex, DuplexError, DuplexEvent, DuplexPrices, DuplexUsage, PcmVoice};
use omuzaki_performer_api::speech::{SpeechControl, SpeechHandle, SpeechState, VoiceError};

use crate::voice::{BLOCK_S, lock};

/// The rate the fakes stream at: a realtime provider's 24 kHz.
pub const DUPLEX_RATE: u32 = 24_000;
/// The length of one audio delta in [`reply_events`]: 100 ms.
pub const DELTA_S: f64 = 0.1;
/// How much faster than real time [`reply_events`] streams its audio, as a provider does.
pub const STREAM_SPEEDUP: f64 = 5.0;

/// gpt-realtime's prices (October 2026): audio $32 in, $0.40 cached, $64 out per million tokens; text $4 in, $16
/// out; 10 audio tokens a second heard and 20 spoken.
pub fn realtime_prices() -> DuplexPrices {
    DuplexPrices::new(32_000_000, 4_000_000, 400_000, 64_000_000, 16_000_000, 10.0, 20.0)
}

/// One call a [`FakeDuplex`] received.
#[derive(Clone, Debug, PartialEq)]
pub enum DuplexCall {
    /// `reply`, at a time, with its instructions.
    Reply(Seconds, Option<String>),
    /// `cancel`: when, the reply, the item and the milliseconds heard.
    Cancel(Seconds, String, Option<String>, u64),
    /// `auto_reply`.
    Auto(Seconds, bool),
    /// `close`.
    Close(Seconds),
}

/// A canned reply: its events, each at an offset in seconds from the moment the reply is asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct CannedReply {
    /// The reply's id.
    pub id: String,
    /// `(offset, event)`, in order.
    pub events: Vec<(f64, DuplexEvent)>,
    /// What it bills when it completes; a cancelled reply bills half.
    pub usage: DuplexUsage,
}

/// A reply of `text` with `seconds` of audio: it starts 200 ms after it is asked for (the provider's first-audio
/// latency), its transcript arrives word by word with its audio, its audio arrives in [`DELTA_S`] deltas
/// [`STREAM_SPEEDUP`] times faster than real time, and it ends with its usage.
pub fn reply_events(id: &str, text: &str, seconds: f64, usage: DuplexUsage) -> CannedReply {
    const FIRST: f64 = 0.2;
    let mut events = vec![(FIRST, DuplexEvent::reply_started(id))];
    // A delta count of a few seconds of speech.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let deltas = (seconds / DELTA_S).ceil().max(1.0) as usize;
    let words: Vec<String> = text.split_inclusive(' ').map(str::to_owned).collect();
    // Samples of one delta: 2400.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let samples = (DELTA_S * f64::from(DUPLEX_RATE)).round() as usize;
    let step = DELTA_S / STREAM_SPEEDUP;
    let item = format!("{id}-item");
    for i in 0..deltas.max(words.len()) {
        // Counts of a short reply.
        #[allow(clippy::cast_precision_loss)]
        let at = FIRST + 0.01 + i as f64 * step;
        if let Some(word) = words.get(i) {
            events.push((at, DuplexEvent::reply_text(id, word.clone())));
        }
        if i < deltas {
            let pcm = vec![0.1; samples];
            events.push((at, DuplexEvent::reply_audio(id, item.clone(), pcm, DUPLEX_RATE)));
        }
    }
    let end = events.last().map_or(FIRST, |(at, _)| *at) + 0.01;
    events.push((end, DuplexEvent::reply_done(id, false, Some(usage))));
    CannedReply { id: id.to_owned(), events, usage }
}

struct Inner {
    clock: Arc<FakeClock>,
    prices: DuplexPrices,
    /// Timed events still to release, in time order.
    script: VecDeque<(Seconds, DuplexEvent)>,
    /// Replies to give, in order, when asked or (in automatic mode) when a line is heard.
    replies: VecDeque<CannedReply>,
    /// The reply being given.
    current: Option<CannedReply>,
    calls: Vec<DuplexCall>,
    auto: bool,
    closed: bool,
}

impl Inner {
    fn schedule(&mut self, at: Seconds, event: DuplexEvent) {
        let i = self.script.iter().position(|(t, _)| t.0 > at.0).unwrap_or(self.script.len());
        self.script.insert(i, (at, event));
    }

    fn start_reply(&mut self, at: Seconds) {
        if self.current.is_some() {
            return;
        }
        let Some(reply) = self.replies.pop_front() else { return };
        for (offset, event) in &reply.events {
            self.schedule(Seconds(at.0 + offset), event.clone());
        }
        self.current = Some(reply);
    }
}

/// A [`Duplex`] on a [`FakeClock`]. The test scripts what the person does ([`script`](Self::script)) and what the
/// model answers ([`answer`](Self::answer)); a reply is given when `reply` is called, or by itself after a
/// `Heard` when automatic replies are on, as a provider with server-side turn detection does. `cancel` drops the
/// reply's undelivered events and reports it done, cancelled, at half its usage. Clones share one session.
#[derive(Clone)]
pub struct FakeDuplex(Arc<Mutex<Inner>>);

impl FakeDuplex {
    /// A session on `clock` billing at [`realtime_prices`].
    pub fn new(clock: Arc<FakeClock>) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            clock,
            prices: realtime_prices(),
            script: VecDeque::new(),
            replies: VecDeque::new(),
            current: None,
            calls: Vec::new(),
            auto: false,
            closed: false,
        })))
    }

    /// Releases `event` at `at`.
    #[must_use]
    pub fn script(self, at: Seconds, event: DuplexEvent) -> Self {
        lock(&self.0).schedule(at, event);
        self
    }

    /// The person says `text` from `from` to `to`: speech starts, stops, and its transcript follows 100 ms later.
    #[must_use]
    pub fn says(self, from: Seconds, to: Seconds, text: &str) -> Self {
        self.script(from, DuplexEvent::SpeechStarted)
            .script(to, DuplexEvent::SpeechStopped)
            .script(Seconds(to.0 + 0.1), DuplexEvent::heard(text))
    }

    /// Queues a reply to give.
    #[must_use]
    pub fn answer(self, reply: CannedReply) -> Self {
        lock(&self.0).replies.push_back(reply);
        self
    }

    /// Every call so far.
    pub fn calls(&self) -> Vec<DuplexCall> {
        lock(&self.0).calls.clone()
    }

    /// Whether automatic replies are on.
    pub fn auto(&self) -> bool {
        lock(&self.0).auto
    }
}

impl Duplex for FakeDuplex {
    fn kind(&self) -> &'static str {
        "fake-realtime"
    }

    fn prices(&self) -> DuplexPrices {
        lock(&self.0).prices
    }

    fn poll(&mut self) -> Vec<DuplexEvent> {
        let mut inner = lock(&self.0);
        if inner.closed {
            return Vec::new();
        }
        let now = inner.clock.now();
        let mut out = Vec::new();
        while inner.script.front().is_some_and(|(at, _)| at.0 <= now.0) {
            let Some((at, event)) = inner.script.pop_front() else { break };
            if let DuplexEvent::ReplyDone { reply, .. } = &event
                && inner.current.as_ref().is_some_and(|c| &c.id == reply)
            {
                inner.current = None;
            }
            let heard = matches!(event, DuplexEvent::Heard { .. });
            out.push(event);
            if heard && inner.auto {
                inner.start_reply(at);
            }
        }
        out
    }

    fn reply(&mut self, instructions: Option<&str>) -> Result<(), DuplexError> {
        let mut inner = lock(&self.0);
        if inner.closed {
            return Err(DuplexError::Closed);
        }
        let now = inner.clock.now();
        inner.calls.push(DuplexCall::Reply(now, instructions.map(str::to_owned)));
        inner.start_reply(now);
        Ok(())
    }

    fn cancel(&mut self, reply: &str, item: Option<&str>, played_ms: u64) -> Result<(), DuplexError> {
        let mut inner = lock(&self.0);
        if inner.closed {
            return Err(DuplexError::Closed);
        }
        let now = inner.clock.now();
        inner.calls.push(DuplexCall::Cancel(now, reply.to_owned(), item.map(str::to_owned), played_ms));
        if let Some(current) = inner.current.take_if(|c| c.id == reply) {
            inner.script.retain(|(_, e)| !belongs(e, reply));
            let u = current.usage;
            let half =
                DuplexUsage::new(u.input_audio, u.input_text, u.cached_input, u.output_audio / 2, u.output_text / 2);
            inner.schedule(now, DuplexEvent::reply_done(reply, true, Some(half)));
        }
        Ok(())
    }

    fn auto_reply(&mut self, on: bool) -> Result<(), DuplexError> {
        let mut inner = lock(&self.0);
        if inner.closed {
            return Err(DuplexError::Closed);
        }
        let now = inner.clock.now();
        inner.calls.push(DuplexCall::Auto(now, on));
        inner.auto = on;
        Ok(())
    }

    fn close(&mut self) {
        let mut inner = lock(&self.0);
        let now = inner.clock.now();
        inner.calls.push(DuplexCall::Close(now));
        inner.closed = true;
    }
}

fn belongs(event: &DuplexEvent, id: &str) -> bool {
    match event {
        DuplexEvent::ReplyStarted { reply, .. }
        | DuplexEvent::ReplyAudio { reply, .. }
        | DuplexEvent::ReplyText { reply, .. }
        | DuplexEvent::ReplyDone { reply, .. } => reply == id,
        _ => false,
    }
}

/// One stretch of PCM a [`FakePcmVoice`] played.
#[derive(Clone, Debug, PartialEq)]
pub struct Played {
    /// When it starts (or started).
    pub start: Seconds,
    /// Its length.
    pub duration: Seconds,
    /// On the monitor only.
    pub private: bool,
    /// When it went silent, if it was cut.
    pub cut: Option<Seconds>,
}

impl Played {
    /// When it ends (or ended).
    pub fn end(&self) -> Seconds {
        self.cut.unwrap_or(Seconds(self.start.0 + self.duration.0))
    }
}

struct Stretch {
    clock: Arc<FakeClock>,
    played: Mutex<Played>,
}

impl SpeechControl for Stretch {
    fn state(&self) -> SpeechState {
        let now = self.clock.now().0;
        let p = lock(&self.played);
        let end = p.start.0 + p.duration.0;
        match p.cut {
            Some(at) if now >= at.0 => SpeechState::cut(at, 0),
            _ if now < p.start.0 => SpeechState::ready(p.duration),
            _ if now < end => SpeechState::playing(p.start, p.duration),
            Some(at) => SpeechState::cut(at, 0),
            None => SpeechState::done(Seconds(end)),
        }
    }

    fn cut(&self) {
        let now = self.clock.now().0;
        let mut p = lock(&self.played);
        if p.cut.is_none() && now < p.start.0 + p.duration.0 {
            p.cut = Some(Seconds((now + BLOCK_S).max(p.start.0).min(p.start.0 + p.duration.0)));
        }
    }
}

/// A [`PcmVoice`] on a [`FakeClock`] at [`DUPLEX_RATE`]: a stretch starts `lead` after it is queued and never
/// before the previous one ends, and a cut lands one [`BLOCK_S`] later. Clones share one record.
#[derive(Clone)]
pub struct FakePcmVoice {
    clock: Arc<FakeClock>,
    lead: Seconds,
    stretches: Arc<Mutex<Vec<Arc<Stretch>>>>,
}

impl FakePcmVoice {
    /// A voice on `clock` whose audio starts `lead` after it is queued.
    pub fn new(clock: Arc<FakeClock>, lead: Seconds) -> Self {
        Self { clock, lead, stretches: Arc::default() }
    }

    /// Every stretch played, in order.
    pub fn log(&self) -> Vec<Played> {
        lock(&self.stretches).iter().map(|s| lock(&s.played).clone()).collect()
    }
}

impl PcmVoice for FakePcmVoice {
    fn rate(&self) -> u32 {
        DUPLEX_RATE
    }

    fn play(&mut self, pcm: Vec<f32>, private: bool) -> Result<SpeechHandle, VoiceError> {
        if pcm.is_empty() {
            return Err(VoiceError::bad_chunk(0));
        }
        let mut stretches = lock(&self.stretches);
        let queued = self.clock.now().0 + self.lead.0;
        // A cut stretch frees the feed: what follows starts at once.
        let after = stretches.last().map_or(queued, |s| lock(&s.played).end().0);
        // Sample counts of one delta.
        #[allow(clippy::cast_precision_loss)]
        let duration = Seconds(pcm.len() as f64 / f64::from(DUPLEX_RATE));
        let played = Played { start: Seconds(queued.max(after)), duration, private, cut: None };
        let stretch = Arc::new(Stretch { clock: Arc::clone(&self.clock), played: Mutex::new(played) });
        stretches.push(Arc::clone(&stretch));
        Ok(SpeechHandle::new(stretch))
    }
}
