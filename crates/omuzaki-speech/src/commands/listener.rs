//! [`CommandListener`]: final transcripts of the person's microphone become `voice.command` events.
//!
//! **Only the raw mic.** The listener is told which tap its speech host reads, and hears only the `listen` voice tap
//! (`TapPoint::Voice(VoiceUse::Listen)`, the person's microphone before any changer). On any other tap (the AI's voice,
//! TTS, the soundboard, a bus) it stays paused, so neither the AI performer nor the chat reader can trigger a command.
//!
//! **Nothing kept.** A transcript is matched and dropped: it never enters the event, the op log or the values monitor,
//! and the listener keeps no text but the last command's id.
//!
//! **Never blocks.** A publish the show's inbox refuses as full is kept, in order, and retried before the next one; a
//! retry buffer that overflows drops its oldest and reports it to the sink (automation design D9).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_audio_api::TapPoint;
use omuzaki_audio_api::tap::VoiceUse;
use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::{EventSink, LiveEvent, PublishError, Source};

use super::matcher::match_transcript;
use super::{VoiceCommand, VoiceCommands};
use crate::host::{SpeechEvent, SpeechEvents, SpeechHost, TranscriptDemand, TranscriptSwitch};

/// How many refused events the listener keeps for a retry.
pub const RETRY_EVENTS: usize = 16;
/// How long the listener's thread waits for an event before it looks at the state again.
const POLL: Duration = Duration::from_millis(50);
/// The status while the microphone is muted.
const MUTED: &str = "the microphone is muted";
/// The status on a tap that isn't the person's microphone.
const NOT_THE_MIC: &str = "voice commands only listen to your microphone";

/// What the listener has done.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListenerStats {
    /// Final transcripts read while on.
    pub transcripts: u64,
    /// Commands published.
    pub published: u64,
    /// Transcripts ignored because the tap isn't the microphone.
    pub not_the_mic: u64,
    /// Events the inbox refused as full and the listener kept for a retry.
    pub retried: u64,
    /// Events lost because the retry buffer overflowed (reported to the sink too).
    pub dropped: u64,
    /// Events the inbox refused for good (an unknown kind or a bad payload; a bug).
    pub refused: u64,
}

/// Turns final transcripts into `voice.command` events.
pub struct CommandListener {
    commands: VoiceCommands,
    sink: Arc<dyn EventSink>,
    /// The runner time of the host's first sample: host times are offsets from it.
    origin: Seconds,
    mic: bool,
    retry: VecDeque<LiveEvent>,
    stats: ListenerStats,
}

impl CommandListener {
    /// A listener for the speech host reading `tap`, whose first sample is at `origin` on the runner clock. On any tap
    /// but the `listen` voice tap it never publishes, and the status says why.
    pub fn new(commands: VoiceCommands, sink: Arc<dyn EventSink>, tap: &TapPoint, origin: Seconds) -> Self {
        let mic = *tap == TapPoint::Voice(VoiceUse::Listen);
        if !mic {
            commands.pause(Some(NOT_THE_MIC));
        }
        Self { commands, sink, origin, mic, retry: VecDeque::new(), stats: ListenerStats::default() }
    }

    /// The counters so far.
    pub fn stats(&self) -> ListenerStats {
        self.stats
    }

    /// Reads one host event; returns how many commands it published.
    pub fn on_event(&mut self, event: &SpeechEvent) -> usize {
        self.flush();
        match event {
            SpeechEvent::Muted(muted) if self.mic => {
                self.commands.pause(muted.then_some(MUTED));
                0
            }
            SpeechEvent::Final { text, end, .. } => self.on_final(text, *end),
            _ => 0,
        }
    }

    fn on_final(&mut self, text: &str, end: f64) -> usize {
        if !self.mic {
            self.stats.not_the_mic += 1;
            return 0;
        }
        if !self.commands.enabled() {
            return 0;
        }
        self.stats.transcripts += 1;
        let at = Seconds(self.origin.0 + end);
        let phrases = self.commands.phrases();
        let mut published = 0;
        for found in match_transcript(&phrases, text) {
            let id = found.id.clone();
            let Ok(event) = LiveEvent::of(&VoiceCommand::from(found), Source::Voice, at) else {
                self.stats.refused += 1;
                continue;
            };
            self.publish(event);
            self.commands.heard(&id, at);
            published += 1;
        }
        published
    }

    /// Publishes `event`, behind any still waiting for a retry; one the inbox refuses as full is kept for a retry.
    fn publish(&mut self, event: LiveEvent) {
        if self.retry.is_empty() {
            match self.sink.publish(event.clone()) {
                Ok(()) => self.stats.published += 1,
                Err(PublishError::Full) => self.keep(event),
                Err(_) => self.stats.refused += 1,
            }
        } else {
            self.keep(event);
        }
    }

    /// Keeps `event` for a retry, dropping (and reporting) the oldest kept when the buffer is full.
    fn keep(&mut self, event: LiveEvent) {
        self.stats.retried += 1;
        if self.retry.len() == RETRY_EVENTS {
            self.retry.pop_front();
            self.stats.dropped += 1;
            self.sink.producer_dropped(1);
        }
        self.retry.push_back(event);
    }

    /// Retries the kept events, in order, until one is refused again.
    fn flush(&mut self) {
        while let Some(event) = self.retry.front() {
            match self.sink.publish(event.clone()) {
                Ok(()) => {
                    self.stats.published += 1;
                    self.retry.pop_front();
                }
                Err(PublishError::Full) => return,
                Err(_) => {
                    self.stats.refused += 1;
                    self.retry.pop_front();
                }
            }
        }
    }

    /// Runs the listener on its own thread over `host`'s events (its own copy: the AI performer's ears still get
    /// theirs). It holds a transcript claim on the host only while voice commands are on, so Whisper runs for them only
    /// then.
    ///
    /// # Errors
    /// The OS refused a thread.
    pub fn spawn(self, host: &SpeechHost) -> std::io::Result<ListenerThread> {
        ListenerThread::start(self, host.subscribe(64), host.transcript_switch())
    }
}

/// The listener's thread. Dropping it stops the thread.
pub struct ListenerThread {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<CommandListener>>,
}

impl ListenerThread {
    fn start(mut listener: CommandListener, events: SpeechEvents, switch: TranscriptSwitch) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new().name("omuzaki-voice-commands".into()).spawn(move || {
            let mut claim: Option<TranscriptDemand> = None;
            while !flag.load(Ordering::Relaxed) {
                let on = listener.commands.enabled();
                if on != claim.is_some() {
                    claim = on.then(|| switch.claim());
                }
                if let Some(event) = events.next_timeout(POLL) {
                    listener.on_event(&event);
                }
            }
            listener
        })?;
        Ok(Self { stop, thread: Some(thread) })
    }

    /// Stops the thread and returns the listener (with its counters).
    pub fn stop(mut self) -> Option<CommandListener> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().and_then(|thread| thread.join().ok())
    }
}

impl Drop for ListenerThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // A panic on the thread has ended it already; there is nothing to recover here.
            let _ = thread.join();
        }
    }
}
