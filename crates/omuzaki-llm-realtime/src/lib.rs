//! A realtime speech-to-speech provider for the AI performer (#2347): [`OpenAiRealtime`], `OpenAI`'s Realtime API
//! over a WebSocket, as a [`Duplex`].
//!
//! It is bring-your-own-key only and never the default: the person's own key comes from the keychain, the host joins
//! the hosts `performer.start` asks the person to confirm, and every reply is reserved and billed through the
//! performer's spend gate at the model's [`prices`]. The local ASR, LLM and TTS path stays the default.
//!
//! One connection thread owns the socket. It appends the microphone's audio as it is captured (48 kHz taken to the
//! wire's 24 kHz PCM16), sends the loop's requests, and turns the provider's events into [`DuplexEvent`]s for
//! [`poll`](Duplex::poll). A line heard while the microphone is in `CohostOnly` is marked private. Plain `ws://` reaches
//! only this machine (a test server).

omuzaki_core::define_link! {
    omuzaki_performer_api::link();
}

mod protocol;
mod session;
mod socket;

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use omuzaki_audio_api::ring::Consumer;
use omuzaki_performer_api::duplex::{Duplex, DuplexError, DuplexEvent, DuplexPrices};
use omuzaki_secrets_api::value::SecretValue;

pub use protocol::{TurnDetection, WIRE_RATE};

use crate::session::Command;

/// The provider's kind, as `performer.status` shows it.
pub const KIND: &str = "openai-realtime";
/// The Realtime API's address.
pub const DEFAULT_ENDPOINT: &str = "wss://api.openai.com/v1/realtime";
/// The model asked for when none is set.
pub const DEFAULT_MODEL: &str = "gpt-realtime";
/// The voice asked for when none is set.
pub const DEFAULT_VOICE: &str = "marin";
/// The model that transcribes the person's speech.
pub const DEFAULT_TRANSCRIPTION: &str = "gpt-4o-mini-transcribe";
/// The microphone tap's rate the session reads.
pub const MIC_RATE: u32 = 48_000;

/// A model's prices: `gpt-realtime`'s from the provider's pricing page (checked 2026-10-23): audio $32 in, $0.40
/// cached, $64 out per million tokens; text $4 in, $16 out; a second of the person's speech is 10 audio tokens and a
/// second of the reply's 20. Another model is billed at these too, so the spending cap holds for any model at least
/// as cheap; `known` says whether the model was in the table.
pub fn prices(model: &str) -> (DuplexPrices, bool) {
    let known = matches!(model, "gpt-realtime" | "gpt-realtime-2025-08-28");
    (DuplexPrices::new(32_000_000, 4_000_000, 400_000, 64_000_000, 16_000_000, 10.0, 20.0), known)
}

/// What a session is opened with.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RealtimeConfig {
    /// The WebSocket address, without the model.
    pub endpoint: String,
    /// The model.
    pub model: String,
    /// The voice it speaks in.
    pub voice: String,
    /// The persona's instructions.
    pub instructions: String,
    /// The model that transcribes the person.
    pub transcription: String,
    /// How it finds the end of a turn.
    pub turns: TurnDetection,
}

impl RealtimeConfig {
    /// The defaults, speaking as `instructions` say.
    pub fn new(instructions: impl Into<String>) -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            voice: DEFAULT_VOICE.to_owned(),
            instructions: instructions.into(),
            transcription: DEFAULT_TRANSCRIPTION.to_owned(),
            turns: TurnDetection::Semantic,
        }
    }

    /// The same at `endpoint`.
    #[must_use]
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// The same with `model`.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// The same in `voice`.
    #[must_use]
    pub fn voice(mut self, voice: impl Into<String>) -> Self {
        self.voice = voice.into();
        self
    }

    /// The same, finding turns as `turns` says.
    #[must_use]
    pub fn turns(mut self, turns: TurnDetection) -> Self {
        self.turns = turns;
        self
    }

    /// The address with the model in its query.
    fn url(&self) -> String {
        let sep = if self.endpoint.contains('?') { '&' } else { '?' };
        format!("{}{sep}model={}", self.endpoint, self.model)
    }
}

/// The host a session at `endpoint` talks to, and whether it is this machine.
pub fn endpoint_host(endpoint: &str) -> Option<(String, bool)> {
    socket::host_of(endpoint).map(|h| {
        let local = socket::loopback(&h);
        (h, local)
    })
}

/// An open Realtime session: see the crate docs. Dropping it closes the session.
pub struct OpenAiRealtime {
    commands: Sender<Command>,
    events: Receiver<DuplexEvent>,
    prices: DuplexPrices,
    thread: Option<JoinHandle<()>>,
    closed: bool,
}

impl OpenAiRealtime {
    /// Opens a session: connects, sends the session's settings and starts the connection thread, which reads the
    /// microphone from `mic` (a 48 kHz mono tap ring) when given.
    ///
    /// # Errors
    /// [`DuplexError::Provider`] with what failed, in words (a refused key, an unreachable host, a plain `ws://` to
    /// another machine); the key never appears in it.
    pub fn connect(
        config: &RealtimeConfig,
        key: Option<&SecretValue>,
        mic: Option<Consumer>,
    ) -> Result<Self, DuplexError> {
        let mut socket = socket::connect(&config.url(), key).map_err(DuplexError::Provider)?;
        let settings = protocol::Session {
            instructions: &config.instructions,
            voice: &config.voice,
            transcription: &config.transcription,
            turns: config.turns,
            auto: false,
        };
        session::send(&mut socket, &protocol::session_update(&settings)).map_err(DuplexError::Provider)?;
        let (commands, inbox) = mpsc::channel();
        let (outbox, events) = mpsc::channel();
        let thread = session::Thread::new(socket, inbox, outbox, mic, config.turns);
        let thread = std::thread::Builder::new()
            .name("realtime session".to_owned())
            .spawn(move || thread.run())
            .map_err(|e| DuplexError::Provider(format!("the session thread couldn't start: {e}")))?;
        Ok(Self { commands, events, prices: prices(&config.model).0, thread: Some(thread), closed: false })
    }

    fn send(&mut self, command: Command) -> Result<(), DuplexError> {
        if self.closed {
            return Err(DuplexError::Closed);
        }
        self.commands.send(command).map_err(|_| {
            self.closed = true;
            DuplexError::Closed
        })
    }
}

impl Duplex for OpenAiRealtime {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn prices(&self) -> DuplexPrices {
        self.prices
    }

    fn poll(&mut self) -> Vec<DuplexEvent> {
        let events: Vec<DuplexEvent> = self.events.try_iter().collect();
        if events.iter().any(|e| matches!(e, DuplexEvent::Error { fatal: true, .. })) {
            self.closed = true;
        }
        events
    }

    fn reply(&mut self, instructions: Option<&str>) -> Result<(), DuplexError> {
        self.send(Command::Reply(instructions.map(str::to_owned)))
    }

    fn cancel(&mut self, reply: &str, item: Option<&str>, played_ms: u64) -> Result<(), DuplexError> {
        self.send(Command::Cancel { reply: reply.to_owned(), item: item.map(str::to_owned), played_ms })
    }

    fn auto_reply(&mut self, on: bool) -> Result<(), DuplexError> {
        self.send(Command::Auto(on))
    }

    fn close(&mut self) {
        if !self.closed {
            let _ = self.commands.send(Command::Close);
            self.closed = true;
        }
    }
}

impl Drop for OpenAiRealtime {
    fn drop(&mut self) {
        self.close();
        if let Some(thread) = self.thread.take() {
            // The thread looks at its commands every read tick, so this is short.
            let _ = thread.join();
        }
    }
}

impl std::fmt::Debug for OpenAiRealtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiRealtime").field("closed", &self.closed).finish_non_exhaustive()
    }
}
