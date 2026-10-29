//! The TTS host (AP-02, `ai-performer-voice-face.md` §3.1–§3.3, §3.6): the performer's [`Voice`], speaking chunks
//! through a `tts@1` session into the `ai_voice` feeds.
//!
//! ```text
//!  speak() ──▶ queue ──▶ synth thread (tts@1, ≤ 2 chunks ahead) ──▶ trimmed, gained PCM
//!                                                                         │
//!            pump (every 10 ms): tops the feed up to 500 ms of lead ◀─────┘ ──▶ ai_voice_a | ai_voice_b | _private
//!                   reads the playhead: Ready ▶ Playing { from } ▶ Done { at }; pushes the VoiceLead
//! ```
//!
//! - **Two threads,** so a slow synthesis never starves the feed: the synth thread runs one call per chunk, at most two
//!   chunks ahead of the one playing; the pump ([`TtsHost::pump`], every 10 ms on its own thread in the app, driven by
//!   hand in tests) writes PCM ahead of playback and moves every handle's state from the feed's playhead, so `from` and
//!   `at` are what the audio thread measured, never a guess.
//! - **Gapless,** with pauses where a person makes them: 180 ms after `. ! ? …`, 80 ms after a comma. A chunk that isn't
//!   synthesised when the one before it has played lengthens the pause and counts in [`TtsStats::late_chunks`].
//! - **Cut at once:** [`SpeechHandle::cut`] raises the feed's closer from the calling thread; the audio thread fades out
//!   within one block plus 2.5 ms. The next speech goes to the **other** feed while the pump re-opens the cut one, and
//!   chunks written behind the cut that hadn't started are written again there. The cut chunk's PCM is kept: speaking
//!   the same chunk again (the resume after a cough) re-uses it, with no second synthesis.
//! - **Pitch is the declared rate:** `p` semitones synthesise at speed `s / r` and declare `24 kHz × r`
//!   (`r = 2^(p/12)`), so the engine's edge resampler moves the pitch and gives back the tempo.
//! - **Style tags** set the model's controls and a gain, from the §3.6 table.
//! - **Private** chunks go to `ai_voice_private`, a feed whose bus reaches only `monitor_out`; the lead is marked
//!   private so the face does not move.
//!
//! Hosted voices (CN-10) and their `SpendGate` reservations come with the connectors; a local model costs nothing.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_ai_api::Session;
use omuzaki_audio_api::feed::Missing;
use omuzaki_audio_api::tap::FeedRequest;
use omuzaki_audio_api::{AudioEngine, EngineClock, FeedCloser, FeedId, FeedPlayhead, FeedWriter, SAMPLE_RATE};
use omuzaki_core::time::Seconds;
use omuzaki_performer_api::duplex::PcmVoice;
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::intent::StyleTag;
use omuzaki_performer_api::limits::CHUNK_MAX_CHARS;
use omuzaki_performer_api::speech::{SpeakHow, SpeechControl, SpeechHandle, SpeechState, Voice, VoiceError};
use omuzaki_performer_api::voice::{
    CutProbe, CutSource, CutState, PlayGap, Played, SPEED_MAX, SPEED_MIN, VoiceAssignment, VoiceLead,
};

use crate::ears::EchoLog;
use crate::slots::tts::{TTS_RATE, TtsCall, Utterance};

/// How far ahead of playback the pump keeps the feed (§3.1).
pub const LEAD: Seconds = Seconds(0.5);
/// How often the pump runs.
pub const PUMP_EVERY: Duration = Duration::from_millis(10);
/// The pause after a sentence.
pub const SENTENCE_PAD: Seconds = Seconds(0.18);
/// The pause after a chunk that ends at a comma.
pub const COMMA_PAD: Seconds = Seconds(0.08);
/// Chunks synthesised ahead of the one playing.
pub const AHEAD: usize = 2;
/// Each feed's ring: the lead plus a 500 ms stall of the pump.
pub const FEED_CAPACITY: Seconds = Seconds(1.0);
/// Leading and trailing audio quieter than this (−50 dBFS) is trimmed...
pub const TRIM_FLOOR: f32 = 0.003_162_3;
/// ... down to this much.
pub const TRIM_KEEP: Seconds = Seconds(0.04);

/// The feeds, in slot order: the two public ones that alternate after a cut (both sources of the `ai_voice` bus), and
/// the private one (the only source of the `ai_voice_private` bus, whose one output is `monitor_out`). Feed ids are
/// slugs, so the design's `ai_voice.a` is written `ai_voice_a`.
pub const FEEDS: [&str; 3] = ["ai_voice_a", "ai_voice_b", "ai_voice_private"];
const PRIVATE: usize = 2;

/// What a chunk's style asks of the model and the mix (§3.6).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StyleControls {
    /// Multiplies the voice's speed.
    pub speed: f32,
    /// 0 flat to 1 lively; 0.5 neutral.
    pub expressiveness: f32,
    /// Added to the speech, in dB.
    pub gain_db: f32,
}

impl StyleControls {
    /// The controls `tags` set, applied in order: speeds multiply, gains add, the last expressiveness wins.
    #[must_use]
    pub fn of(tags: &[StyleTag]) -> Self {
        let mut out = Self { speed: 1.0, expressiveness: 0.5, gain_db: 0.0 };
        for tag in tags {
            let (speed, expressiveness, gain_db) = match tag {
                StyleTag::Calm => (0.95, Some(0.35), 0.0),
                StyleTag::Excited => (1.08, Some(0.80), 1.0),
                StyleTag::Sad => (0.90, Some(0.30), -2.0),
                StyleTag::Whisper => (0.90, Some(0.20), -8.0),
                StyleTag::Laugh => (1.00, Some(0.75), 0.0),
                StyleTag::Fast => (1.15, None, 0.0),
                StyleTag::Slow => (0.87, None, 0.0),
            };
            out.speed *= speed;
            out.expressiveness = expressiveness.unwrap_or(out.expressiveness);
            out.gain_db += gain_db;
        }
        out
    }
}

/// The voice a host speaks in, from the character's assignment.
#[derive(Clone, Debug, PartialEq)]
pub struct TtsVoice {
    /// `<model>/<voice>`: the model's own voice name is the part after the slash.
    pub id: VoiceId,
    /// Tempo, [`SPEED_MIN`]`..=`[`SPEED_MAX`].
    pub speed: f32,
    /// Pitch shift in semitones, made by the declared rate.
    pub pitch_semitones: f32,
}

impl TtsVoice {
    /// `id` at speed 1, no pitch shift.
    #[must_use]
    pub fn new(id: VoiceId) -> Self {
        Self { id, speed: 1.0, pitch_semitones: 0.0 }
    }

    /// The voice an assignment names, with its speed and pitch.
    #[must_use]
    pub fn of(assignment: &VoiceAssignment) -> Self {
        Self { id: assignment.voice.clone(), speed: assignment.speed, pitch_semitones: assignment.pitch_semitones }
    }

    /// `r = 2^(p/12)`.
    #[must_use]
    pub fn ratio(&self) -> f64 {
        2f64.powf(f64::from(self.pitch_semitones) / 12.0)
    }

    /// The feeds' declared rate: `24 kHz × r`, rounded to whole hertz (30,238 Hz at +4).
    #[must_use]
    pub fn feed_rate(&self) -> u32 {
        (f64::from(TTS_RATE) * self.ratio()).round() as u32
    }

    /// The speed the model is asked for with `style`: the voice's speed times the style's, kept in the assignment's
    /// range, divided by `r` so the declared rate gives the tempo back.
    #[must_use]
    pub fn model_speed(&self, style: &StyleControls) -> f32 {
        (f64::from((self.speed * style.speed).clamp(SPEED_MIN, SPEED_MAX)) / self.ratio()) as f32
    }
}

/// `pcm` with leading and trailing audio under [`TRIM_FLOOR`] cut down to [`TRIM_KEEP`].
#[must_use]
pub fn trim(pcm: &[f32], rate: u32) -> &[f32] {
    let keep = (TRIM_KEEP.0 * f64::from(rate)).round() as usize;
    let loud = |s: &f32| s.abs() >= TRIM_FLOOR;
    match (pcm.iter().position(loud), pcm.iter().rposition(loud)) {
        (Some(first), Some(last)) => &pcm[first.saturating_sub(keep)..(last + 1 + keep).min(pcm.len())],
        _ => &[],
    }
}

/// The pause a chunk ends with, from its last character.
#[must_use]
pub fn pad_after(text: &str) -> Seconds {
    match text.trim_end().chars().last() {
        Some('.' | '!' | '?' | '…') => SENTENCE_PAD,
        Some(',') => COMMA_PAD,
        _ => Seconds(0.0),
    }
}

/// What the host has done, for the health meter.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TtsStats {
    /// Chunks synthesised.
    pub synthesised: u64,
    /// Chunks spoken from the resume cache, with no synthesis.
    pub resumed: u64,
    /// Chunks that weren't ready when the one before them had played: the pause grew.
    pub late_chunks: u64,
    /// Syntheses that failed (the chunk is cut, never played).
    pub failures: u64,
    /// Feed cuts.
    pub cuts: u64,
}

/// The feed a chunk was written to, as the chunk's handle needs it without the host's lock.
#[derive(Clone)]
struct OnFeed {
    slot: usize,
    closer: FeedCloser,
    playhead: FeedPlayhead,
    /// Feed sample of the chunk's first sample.
    start: u64,
}

struct ChunkInner {
    state: SpeechState,
    pcm: Option<Arc<Vec<f32>>>,
    /// The phonemes the model spoke the PCM from, when it reports them: the mouth lines its lips up with them.
    phonemes: Option<Arc<str>>,
    feed: Option<OnFeed>,
    /// PCM samples written, then pad samples written.
    written: usize,
    pad_written: usize,
    cut: bool,
    /// The cut's state is known for good (the audio thread published where it went silent, or it never played).
    settled: bool,
    late: bool,
}

/// One queued chunk: the host's record, which every handle on it reads.
struct Chunk {
    text: String,
    key: String,
    private: bool,
    controls: StyleControls,
    model_speed: f32,
    pad: usize,
    rate: u32,
    clock: Arc<EngineClock>,
    inner: Mutex<ChunkInner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere can't leave these records half-written in a way that matters more than stopping speech.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Chunk {
    fn len(inner: &ChunkInner) -> usize {
        inner.pcm.as_ref().map_or(0, |p| p.len())
    }

    fn seconds(&self, samples: usize) -> Seconds {
        Seconds(samples as f64 / f64::from(self.rate))
    }

    /// The program time now, as the audio thread last published it.
    fn now(&self) -> Seconds {
        self.clock.read().time
    }

    /// Settles a requested cut once where it went silent is known. Returns whether it is settled.
    fn settle(&self, inner: &mut ChunkInner) -> bool {
        if inner.settled {
            return true;
        }
        let chars = self.text.chars().count();
        let (at, spoken) = match &inner.feed {
            Some(on) if inner.written > 0 => {
                let cut_at = on.playhead.cut_at_sample();
                if cut_at == u64::MAX {
                    // The audio thread hasn't finished the fade: report the cut, settle it later.
                    let played = on.playhead.latest().map_or(0, |r| r.consumed + u64::from(r.n));
                    inner.state = SpeechState::cut(self.now(), spoken_chars(chars, played, on.start, Self::len(inner)));
                    return false;
                }
                let played = on.playhead.latest().map_or(0, |r| r.consumed + u64::from(r.n));
                (self.clock.time_of(cut_at), spoken_chars(chars, played, on.start, Self::len(inner)))
            }
            _ => (self.now(), 0),
        };
        inner.state = SpeechState::cut(at, spoken);
        inner.settled = true;
        true
    }
}

/// Characters spoken when the feed had played up to `played`, for a chunk of `len` samples from `start`.
fn spoken_chars(chars: usize, played: u64, start: u64, len: usize) -> u32 {
    if len == 0 || played <= start {
        return 0;
    }
    let fraction = ((played - start) as f64 / len as f64).min(1.0);
    (chars as f64 * fraction).floor() as u32
}

struct Handle(Arc<Chunk>);

impl SpeechControl for Handle {
    fn state(&self) -> SpeechState {
        let mut inner = lock(&self.0.inner);
        if inner.cut && !inner.settled {
            self.0.settle(&mut inner);
        }
        inner.state
    }

    fn cut(&self) {
        let mut inner = lock(&self.0.inner);
        if inner.cut || inner.state.is_final() {
            return;
        }
        inner.cut = true;
        match &inner.feed {
            Some(on) if inner.written > 0 => on.closer.close(),
            _ => {}
        }
        self.0.settle(&mut inner);
    }
}

/// The audio thread's view of one feed, for the mouth drive.
struct FeedCut {
    closer: FeedCloser,
    playhead: FeedPlayhead,
    clock: Arc<EngineClock>,
}

impl CutSource for FeedCut {
    fn state(&self) -> CutState {
        match self.playhead.cut_at_sample() {
            u64::MAX if self.closer.is_raised() => CutState::Raised,
            u64::MAX => CutState::Live,
            at => CutState::cut(self.clock.time_of(at)),
        }
    }

    fn head(&self) -> u64 {
        self.playhead.head()
    }

    fn record(&self, index: u64) -> Result<Played, PlayGap> {
        let r = self.playhead.get(index).map_err(|missing| match missing {
            Missing::NotYet => PlayGap::NotYet,
            Missing::Lapped => PlayGap::Lapped,
        })?;
        // The engine clock's own sample length, so a record's span is exactly the engine samples it played over.
        let clock = self.clock.read();
        let per_feed_sample =
            f64::from(SAMPLE_RATE) / f64::from(self.playhead.rate().max(1)) * clock.seconds_per_sample;
        let start = clock.time_of(r.at_sample);
        Ok(Played {
            index: r.index,
            consumed: r.consumed,
            n: r.n,
            start,
            end: Seconds(start.0 + f64::from(r.n) * per_feed_sample),
        })
    }
}

struct Feed {
    id: FeedId,
    writer: Option<FeedWriter>,
    /// The lead was attached to this opening of the feed.
    attached: bool,
    /// Its closer was seen raised; it re-opens once the fade has ended.
    closing: bool,
}

struct State {
    queue: VecDeque<Arc<Chunk>>,
    feeds: [Feed; 3],
    /// The public feed in use: 0 or 1.
    active: usize,
    /// The last cut chunk's PCM, by its key.
    cache: Option<(String, Arc<Vec<f32>>, Option<Arc<str>>)>,
    synthesising: bool,
    /// The last chunk written has played to its end, and nothing was written after it.
    drained: bool,
    stats: TtsStats,
    lead: Option<Box<dyn VoiceLead>>,
}

struct Shared {
    engine: Arc<dyn AudioEngine>,
    clock: Arc<EngineClock>,
    voice: TtsVoice,
    rate: u32,
    state: Mutex<State>,
    /// The synth thread waits here for work or room.
    work: Condvar,
    /// A synthesis finished.
    progress: Condvar,
    stop: AtomicBool,
    /// Where each chunk's text and play times go as it starts playing, so the ears can drop the performer's own echo.
    echo: Mutex<Option<EchoLog>>,
}

/// The TTS host: see the module docs.
pub struct TtsHost {
    shared: Arc<Shared>,
    synth: Option<JoinHandle<()>>,
    pump: Option<JoinHandle<()>>,
}

impl TtsHost {
    /// Opens the three feeds on `engine` at the voice's declared rate and starts the synth thread over `session` (a
    /// `tts@1` session). The pump is not started: call [`TtsHost::spawn_pump`], or [`TtsHost::pump`] by hand.
    ///
    /// # Errors
    /// [`VoiceError::Provider`] when a feed can't be opened.
    pub fn open(
        engine: Arc<dyn AudioEngine>,
        session: Box<dyn Session>,
        voice: TtsVoice,
        lead: Option<Box<dyn VoiceLead>>,
    ) -> Result<Self, VoiceError> {
        let shared = Self::shared(engine, voice, lead)?;
        let worker = Arc::clone(&shared);
        let synth = std::thread::Builder::new()
            .name("tts synth".into())
            .spawn(move || worker.synthesise(session))
            .map_err(|e| VoiceError::Provider(format!("can't start the synth thread: {e}")))?;
        Ok(Self { shared, synth: Some(synth), pump: None })
    }

    /// Opens the three feeds for a voice that is handed its audio ([`PcmVoice`]): a realtime model's reply (#2347).
    /// There is no synth thread, so [`Voice::speak`] is refused; everything else (the feeds, the cut, the lead, the
    /// pump) is the same.
    ///
    /// # Errors
    /// [`VoiceError::Provider`] when a feed can't be opened.
    pub fn open_streamed(
        engine: Arc<dyn AudioEngine>,
        voice: TtsVoice,
        lead: Option<Box<dyn VoiceLead>>,
    ) -> Result<Self, VoiceError> {
        Ok(Self { shared: Self::shared(engine, voice, lead)?, synth: None, pump: None })
    }

    fn shared(
        engine: Arc<dyn AudioEngine>,
        voice: TtsVoice,
        lead: Option<Box<dyn VoiceLead>>,
    ) -> Result<Arc<Shared>, VoiceError> {
        let rate = voice.feed_rate();
        let feeds = FEEDS.map(|id| Feed {
            id: FeedId::new(id).unwrap_or_else(|_| unreachable!("a literal feed id")),
            writer: None,
            attached: false,
            closing: false,
        });
        let state = State {
            queue: VecDeque::new(),
            feeds,
            active: 0,
            cache: None,
            synthesising: false,
            drained: false,
            stats: TtsStats::default(),
            lead,
        };
        let shared = Arc::new(Shared {
            clock: engine.clock(),
            engine,
            voice,
            rate,
            state: Mutex::new(state),
            work: Condvar::new(),
            progress: Condvar::new(),
            stop: AtomicBool::new(false),
            echo: Mutex::new(None),
        });
        {
            let mut state = lock(&shared.state);
            for slot in 0..FEEDS.len() {
                shared.open_feed(&mut state, slot)?;
            }
        }
        Ok(shared)
    }

    /// Starts the pump thread: [`TtsHost::pump`] every 10 ms until the host is dropped.
    ///
    /// # Errors
    /// [`VoiceError::Provider`] when the thread can't start.
    pub fn spawn_pump(&mut self) -> Result<(), VoiceError> {
        if self.pump.is_some() {
            return Ok(());
        }
        let shared = Arc::clone(&self.shared);
        let pump = std::thread::Builder::new()
            .name("tts pump".into())
            .spawn(move || {
                while !shared.stop.load(Ordering::Acquire) {
                    shared.pump();
                    std::thread::sleep(PUMP_EVERY);
                }
            })
            .map_err(|e| VoiceError::Provider(format!("can't start the pump thread: {e}")))?;
        self.pump = Some(pump);
        Ok(())
    }

    /// One pump step: re-open cut feeds, move every chunk's state from its feed's playhead, top the feed up to
    /// [`LEAD`] and push the lead.
    pub fn pump(&self) {
        self.shared.pump();
    }

    /// Waits until the synth thread has nothing it could start (every chunk it may synthesise now is synthesised), or
    /// `timeout` passes. Returns whether it got there. For tests and previews that want synthesis to keep up.
    pub fn wait_synthesis(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.shared.state);
        loop {
            if !state.synthesising && Shared::next_job(&state).is_none() {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self.shared.progress.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Writes each chunk's text and play times to `echo` as it starts playing, so [`SpeechEars`](crate::SpeechEars)
    /// can drop the performer's own voice when the microphone hears it.
    pub fn set_echo(&self, echo: EchoLog) {
        *lock(&self.shared.echo) = Some(echo);
    }

    /// The counters.
    #[must_use]
    pub fn stats(&self) -> TtsStats {
        lock(&self.shared.state).stats
    }

    /// The public feed new speech goes to now.
    #[must_use]
    pub fn active_feed(&self) -> FeedId {
        let state = lock(&self.shared.state);
        state.feeds[state.active].id.clone()
    }

    /// The feeds' declared rate.
    #[must_use]
    pub fn feed_rate(&self) -> u32 {
        self.shared.rate
    }
}

impl Drop for TtsHost {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        {
            let _state = lock(&self.shared.state);
            self.shared.work.notify_all();
        }
        for thread in [self.synth.take(), self.pump.take()].into_iter().flatten() {
            let _ = thread.join();
        }
    }
}

impl PcmVoice for TtsHost {
    fn rate(&self) -> u32 {
        self.shared.rate
    }

    fn play(&mut self, pcm: Vec<f32>, private: bool) -> Result<SpeechHandle, VoiceError> {
        if pcm.is_empty() {
            return Err(VoiceError::Provider("no audio to play".into()));
        }
        let shared = &self.shared;
        let duration = Seconds(pcm.len() as f64 / f64::from(shared.rate));
        let chunk = Arc::new(Chunk {
            text: String::new(),
            key: String::new(),
            private,
            controls: StyleControls::of(&[]),
            model_speed: 1.0,
            pad: 0,
            rate: shared.rate,
            clock: Arc::clone(&shared.clock),
            inner: Mutex::new(ChunkInner {
                state: SpeechState::ready(duration),
                pcm: Some(Arc::new(pcm)),
                phonemes: None,
                feed: None,
                written: 0,
                pad_written: 0,
                cut: false,
                settled: false,
                late: false,
            }),
        });
        lock(&shared.state).queue.push_back(Arc::clone(&chunk));
        Ok(SpeechHandle::new(Arc::new(Handle(chunk))))
    }
}

impl Voice for TtsHost {
    fn speak(&mut self, sentence: &str, voice: &VoiceId, how: &SpeakHow) -> Result<SpeechHandle, VoiceError> {
        if self.synth.is_none() {
            return Err(VoiceError::Provider("this voice plays a realtime model's audio and can't synthesise".into()));
        }
        let chars = sentence.chars().count();
        if sentence.trim().is_empty() || chars > CHUNK_MAX_CHARS {
            return Err(VoiceError::bad_chunk(chars));
        }
        let shared = &self.shared;
        if *voice != shared.voice.id {
            return Err(VoiceError::unknown_voice(voice));
        }
        let controls = StyleControls::of(&how.style);
        let key = format!("{sentence}\u{0}{voice}\u{0}{how:?}");
        let mut state = lock(&shared.state);
        let cached = state
            .cache
            .as_ref()
            .filter(|(k, ..)| *k == key)
            .map(|(_, pcm, phonemes)| (Arc::clone(pcm), phonemes.clone()));
        let chunk = Arc::new(Chunk {
            text: sentence.to_owned(),
            key,
            private: how.private,
            controls,
            model_speed: shared.voice.model_speed(&controls),
            pad: (pad_after(sentence).0 * f64::from(shared.rate)).round() as usize,
            rate: shared.rate,
            clock: Arc::clone(&shared.clock),
            inner: Mutex::new(ChunkInner {
                state: SpeechState::Queued,
                pcm: None,
                phonemes: None,
                feed: None,
                written: 0,
                pad_written: 0,
                cut: false,
                settled: false,
                late: false,
            }),
        });
        if let Some((pcm, phonemes)) = cached {
            let mut inner = lock(&chunk.inner);
            inner.state = SpeechState::ready(chunk.seconds(pcm.len()));
            inner.pcm = Some(pcm);
            inner.phonemes = phonemes;
            state.stats.resumed += 1;
        }
        state.queue.push_back(Arc::clone(&chunk));
        shared.work.notify_all();
        Ok(SpeechHandle::new(Arc::new(Handle(chunk))))
    }
}

/// A job for the synth thread.
struct Job {
    chunk: Arc<Chunk>,
}

impl Shared {
    fn open_feed(&self, state: &mut State, slot: usize) -> Result<(), VoiceError> {
        let request = FeedRequest {
            id: state.feeds[slot].id.clone(),
            rate: self.rate,
            mono: true,
            capacity: FEED_CAPACITY,
            consumer: "tts host".into(),
        };
        let writer = self.engine.feed(&request).map_err(|e| VoiceError::Provider(e.to_string()))?;
        state.feeds[slot].writer = Some(writer);
        state.feeds[slot].attached = false;
        Ok(())
    }

    /// The next chunk to synthesise, if there is room: at most [`AHEAD`] synthesised chunks wait behind the one
    /// playing.
    fn next_job(state: &State) -> Option<Job> {
        let mut synthesised = 0;
        for chunk in &state.queue {
            let inner = lock(&chunk.inner);
            if inner.cut || inner.state.is_final() {
                continue;
            }
            if inner.pcm.is_some() {
                synthesised += 1;
                continue;
            }
            return (synthesised <= AHEAD).then(|| Job { chunk: Arc::clone(chunk) });
        }
        None
    }

    fn synthesise(&self, mut session: Box<dyn Session>) {
        let mut call = TtsCall::new();
        let voice = self.voice.id.voice().to_owned();
        loop {
            let job = {
                let mut state = lock(&self.state);
                loop {
                    if self.stop.load(Ordering::Acquire) {
                        return;
                    }
                    if let Some(job) = Self::next_job(&state) {
                        state.synthesising = true;
                        break job;
                    }
                    state = self.work.wait(state).unwrap_or_else(PoisonError::into_inner);
                }
            };
            let chunk = &job.chunk;
            let utterance = Utterance {
                text: &chunk.text,
                voice: &voice,
                speed: chunk.model_speed,
                expressiveness: chunk.controls.expressiveness,
            };
            let spoken = call.speak(session.as_mut(), &utterance).map(|pcm| {
                let gain = 10f32.powf(chunk.controls.gain_db / 20.0);
                // Trimmed in samples of the declared rate: the kept 40 ms are 40 ms as played.
                trim(pcm, self.rate).iter().map(|s| s * gain).collect::<Vec<f32>>()
            });
            let phonemes = session.phonemes().filter(|p| !p.is_empty()).map(Arc::<str>::from);
            let mut state = lock(&self.state);
            state.synthesising = false;
            {
                let mut inner = lock(&chunk.inner);
                match spoken {
                    Ok(pcm) if !pcm.is_empty() => {
                        state.stats.synthesised += 1;
                        if !inner.cut {
                            inner.state = SpeechState::ready(chunk.seconds(pcm.len()));
                        }
                        inner.pcm = Some(Arc::new(pcm));
                        inner.phonemes = phonemes;
                    }
                    _ => {
                        state.stats.failures += 1;
                        inner.cut = true;
                        chunk.settle(&mut inner);
                    }
                }
            }
            self.progress.notify_all();
        }
    }

    fn pump(&self) {
        let mut state = lock(&self.state);
        let state = &mut *state;
        self.handle_cuts(state);
        self.track(state);
        self.write(state);
        state.queue.retain(|c| {
            let inner = lock(&c.inner);
            !(inner.state.is_final() && (!inner.cut || inner.settled))
        });
        self.work.notify_all();
        self.progress.notify_all();
    }

    /// Feeds whose closer was raised: switch new speech to the other feed, settle the chunks that were playing, write
    /// again elsewhere the ones that hadn't started, and re-open the cut feed once its fade has ended.
    fn handle_cuts(&self, state: &mut State) {
        for slot in 0..FEEDS.len() {
            let Some(writer) = &state.feeds[slot].writer else {
                // A re-open that failed last time.
                let _ = self.open_feed(state, slot);
                continue;
            };
            if !writer.closer().is_raised() {
                continue;
            }
            let cut_at = writer.playhead().cut_at_sample();
            if !state.feeds[slot].closing {
                state.feeds[slot].closing = true;
                state.stats.cuts += 1;
                if slot == state.active {
                    state.active = 1 - state.active;
                }
            }
            let mut cut_pcm = None;
            for chunk in &state.queue {
                let mut inner = lock(&chunk.inner);
                let on_this = inner.feed.as_ref().is_some_and(|on| on.slot == slot);
                if !on_this || matches!(inner.state, SpeechState::Done { .. }) {
                    continue;
                }
                if inner.cut || matches!(inner.state, SpeechState::Playing { .. }) {
                    inner.cut = true;
                    if cut_pcm.is_none() {
                        cut_pcm = inner.pcm.clone().map(|pcm| (chunk.key.clone(), pcm, inner.phonemes.clone()));
                    }
                    chunk.settle(&mut inner);
                } else {
                    // Written behind the cut but never started: write it again on the next feed.
                    inner.feed = None;
                    inner.written = 0;
                    inner.pad_written = 0;
                }
            }
            if cut_pcm.is_some() {
                state.cache = cut_pcm;
            }
            if cut_at != u64::MAX {
                if let Some(lead) = state.lead.as_mut() {
                    lead.cut(self.clock.time_of(cut_at));
                }
                state.feeds[slot].writer = None;
                state.feeds[slot].closing = false;
                let _ = self.open_feed(state, slot);
            }
        }
    }

    /// Moves every chunk on a feed from its playhead: `Playing { from }` once its first sample played, `Done { at }`
    /// once its last (pause included) did.
    fn track(&self, state: &mut State) {
        let mut drained = false;
        for chunk in &state.queue {
            let mut inner = lock(&chunk.inner);
            if inner.cut {
                chunk.settle(&mut inner);
                continue;
            }
            let Some(on) = inner.feed.clone() else { continue };
            let len = Chunk::len(&inner);
            if inner.written == 0 || inner.state.is_final() {
                continue;
            }
            if !matches!(inner.state, SpeechState::Playing { .. })
                && let Some(at) = on.playhead.place(on.start, 0)
            {
                let (from, duration) = (self.clock.time_of(at), chunk.seconds(len));
                inner.state = SpeechState::playing(from, duration);
                if let Some(echo) = lock(&self.echo).as_ref() {
                    echo.said(&chunk.text, from, Seconds(from.0 + duration.0));
                }
            }
            let total = len + chunk.pad;
            if inner.written + inner.pad_written == total
                && let SpeechState::Playing { from, .. } = inner.state
                && let Some(last) = on.playhead.place(on.start + total as u64 - 1, 0)
            {
                let at = Seconds(self.clock.time_of(last).0 + 1.0 / f64::from(self.rate));
                inner.state = SpeechState::done(Seconds(at.0.max(from.0)));
                drained = true;
            }
        }
        if drained {
            state.drained = true;
        }
    }

    /// Writes chunks in order into their feeds, up to [`LEAD`] ahead of playback.
    fn write(&self, state: &mut State) {
        let lead_samples = (LEAD.0 * f64::from(self.rate)).round() as u64;
        let mut previous: Option<(usize, bool)> = None; // (slot, done) of the chunk before
        let queue: Vec<Arc<Chunk>> = state.queue.iter().cloned().collect();
        for chunk in queue {
            let mut inner = lock(&chunk.inner);
            if inner.cut || inner.state.is_final() {
                // Played or cut: it holds nothing up.
                if let Some(on) = &inner.feed {
                    previous = Some((on.slot, true));
                }
                continue;
            }
            let Some(pcm) = inner.pcm.clone() else {
                // Not synthesised: a pause grows once everything before it has played.
                // (Only after something was written: the first chunk of a turn is waited for, not late.)
                let drained = previous.map_or(state.drained, |(_, done)| done);
                if drained && !inner.late {
                    inner.late = true;
                    state.stats.late_chunks += 1;
                }
                return;
            };
            let slot = inner.feed.as_ref().map_or(if chunk.private { PRIVATE } else { state.active }, |on| on.slot);
            if let Some((before, done)) = previous
                && before != slot
                && !done
            {
                return; // another feed is still playing the chunk before
            }
            let feed = &mut state.feeds[slot];
            let Some(writer) = feed.writer.as_mut() else { return };
            if writer.closer().is_raised() {
                return;
            }
            if inner.feed.is_none() {
                state.drained = false;
                inner.feed = Some(OnFeed {
                    slot,
                    closer: writer.closer(),
                    playhead: writer.playhead(),
                    start: writer.written(),
                });
            }
            // The private feed reaches no viewer, so the mouth never follows it: its PCM only clears the layer.
            if !feed.attached && slot != PRIVATE {
                if let Some(lead) = state.lead.as_mut() {
                    let source = FeedCut {
                        closer: writer.closer(),
                        playhead: writer.playhead(),
                        clock: Arc::clone(&self.clock),
                    };
                    lead.attach(CutProbe::new(Arc::new(source)));
                }
                feed.attached = true;
            }
            let playhead = writer.playhead();
            let consumed = playhead.latest().map_or(0, |r| r.consumed + u64::from(r.n));
            let queued = writer.written().saturating_sub(consumed);
            let mut room = usize::try_from(lead_samples.saturating_sub(queued)).unwrap_or(0).min(writer.free());
            if inner.written < pcm.len() && room > 0 {
                let from = inner.written;
                if from == 0
                    && !chunk.private
                    && let (Some(lead), Some(phonemes)) = (state.lead.as_mut(), inner.phonemes.as_deref())
                {
                    lead.line(writer.written(), self.rate, &pcm, phonemes);
                }
                let take = room.min(pcm.len() - from);
                let pushed = writer.push(&pcm[from..from + take]);
                if let Some(lead) = state.lead.as_mut() {
                    let at = predicted(&self.clock, &playhead, writer.written() - pushed as u64, consumed, self.rate);
                    let first = writer.written() - pushed as u64;
                    lead.push(at, first, self.rate, &pcm[from..from + pushed], chunk.private);
                }
                inner.written += pushed;
                room -= pushed;
            }
            if inner.written == pcm.len() && inner.pad_written < chunk.pad && room > 0 {
                const SILENCE: [f32; 480] = [0.0; 480];
                while inner.pad_written < chunk.pad && room > 0 {
                    let take = room.min(chunk.pad - inner.pad_written).min(SILENCE.len());
                    let pushed = writer.push(&SILENCE[..take]);
                    if pushed == 0 {
                        break;
                    }
                    inner.pad_written += pushed;
                    room -= pushed;
                }
            }
            if inner.written < pcm.len() || inner.pad_written < chunk.pad {
                return; // the lead is full
            }
            previous = Some((slot, matches!(inner.state, SpeechState::Done { .. })));
        }
    }
}

/// When feed sample `sample` should play: placed after the newest record if the feed has played anything, else from
/// now. A pacing hint for the mouth drive; its stamps come from the playhead.
fn predicted(clock: &EngineClock, playhead: &FeedPlayhead, sample: u64, consumed: u64, rate: u32) -> Seconds {
    let ahead = sample.saturating_sub(consumed) as f64 / f64::from(rate);
    match playhead.latest() {
        Some(r) => {
            let end = clock.time_of(r.at_sample).0 + f64::from(r.n) / f64::from(rate);
            Seconds(end + ahead)
        }
        None => Seconds(clock.read().time.0 + ahead),
    }
}
