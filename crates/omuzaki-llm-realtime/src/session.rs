//! The connection thread: it owns the socket, appends the microphone's audio as it comes, sends the loop's requests
//! and turns the provider's events into [`DuplexEvent`]s.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use omuzaki_audio_api::ring::Consumer;
use omuzaki_performer_api::duplex::DuplexEvent;
use serde_json::Value;
use tungstenite::{Message, WebSocket};

use crate::protocol::{self, Decimator, Server, TurnDetection, WIRE_RATE};
use crate::socket::{Stream, timed_out};

/// What the performer's loop asks of the thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Reply(Option<String>),
    Cancel { reply: String, item: Option<String>, played_ms: u64 },
    Auto(bool),
    Close,
}

/// Microphone audio is sent in pieces of at least this many wire samples (20 ms): small enough that the provider's
/// turn detection hears the person at once, large enough to keep the message count near 50 a second.
const APPEND_MIN: usize = 480;
/// The microphone's read buffer: 100 ms at 48 kHz.
const MIC_READ: usize = 4_800;
/// The most silence a gap in the tap is filled with (a second at 48 kHz), keeping the provider's clock near ours.
const GAP_FILL_MAX: u64 = 48_000;
/// Messages read before the thread looks at the microphone and its commands again.
const READS_PER_TURN: usize = 64;
/// The privacy of the person's last few turns, by item.
const TURNS_KEPT: usize = 16;
/// Error codes that only mean a cancel arrived after the reply had finished: nothing to report.
const QUIET_CODES: [&str; 2] = ["response_cancel_not_active", "item_truncate_invalid_audio_end_ms"];
/// Error codes after which the session can't go on.
const FATAL_CODES: [&str; 3] = ["session_expired", "invalid_api_key", "insufficient_quota"];

/// Sends one event.
pub(crate) fn send(socket: &mut WebSocket<Stream>, event: &Value) -> Result<(), String> {
    socket.send(Message::text(event.to_string())).map_err(|e| format!("the connection to the provider failed: {e}"))
}

pub(crate) struct Thread {
    socket: WebSocket<Stream>,
    inbox: Receiver<Command>,
    outbox: Sender<DuplexEvent>,
    mic: Option<Consumer>,
    turns: TurnDetection,
    decimator: Decimator,
    read: Vec<f32>,
    wire: Vec<f32>,
    /// Whether the microphone's latest audio was captured in `CohostOnly`.
    private_now: bool,
    /// Whether any of the person's current turn was.
    private_turn: bool,
    /// The privacy of recent turns, by the provider's item id.
    turn_privacy: VecDeque<(String, bool)>,
}

impl Thread {
    pub(crate) fn new(
        socket: WebSocket<Stream>,
        inbox: Receiver<Command>,
        outbox: Sender<DuplexEvent>,
        mic: Option<Consumer>,
        turns: TurnDetection,
    ) -> Self {
        Self {
            socket,
            inbox,
            outbox,
            mic,
            turns,
            decimator: Decimator::new(),
            read: vec![0.0; MIC_READ],
            wire: Vec::with_capacity(MIC_READ),
            private_now: false,
            private_turn: false,
            turn_privacy: VecDeque::new(),
        }
    }

    pub(crate) fn run(mut self) {
        if let Err(message) = self.turn_loop() {
            let _ = self.outbox.send(DuplexEvent::error(message, true));
        }
        let _ = self.socket.close(None);
        let _ = self.socket.flush();
    }

    /// Runs until closed; an error ends the session.
    fn turn_loop(&mut self) -> Result<(), String> {
        loop {
            loop {
                match self.inbox.try_recv() {
                    Ok(Command::Close) | Err(TryRecvError::Disconnected) => return Ok(()),
                    Ok(command) => self.command(command)?,
                    Err(TryRecvError::Empty) => break,
                }
            }
            self.pump_mic()?;
            for _ in 0..READS_PER_TURN {
                match self.socket.read() {
                    Ok(Message::Text(text)) => self.server(text.as_str())?,
                    Ok(Message::Close(_)) => return Err("the provider ended the session".to_owned()),
                    Ok(_) => {}
                    Err(e) if timed_out(&e) => break,
                    Err(e) => return Err(format!("the connection to the provider failed: {e}")),
                }
            }
        }
    }

    fn command(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Reply(instructions) => send(&mut self.socket, &protocol::create(instructions.as_deref())),
            Command::Cancel { reply, item, played_ms } => {
                send(&mut self.socket, &protocol::cancel(&reply))?;
                match item {
                    Some(item) => send(&mut self.socket, &protocol::truncate(&item, played_ms)),
                    None => Ok(()),
                }
            }
            Command::Auto(on) => send(&mut self.socket, &protocol::auto_update(self.turns, on)),
            Command::Close => Ok(()),
        }
    }

    /// Sends what the microphone captured since the last look.
    fn pump_mic(&mut self) -> Result<(), String> {
        let Some(mic) = self.mic.as_mut() else { return Ok(()) };
        loop {
            let got = mic.read(&mut self.read);
            if got.gap_before > 0 {
                // The gap is short and positive, and bounded above.
                #[allow(clippy::cast_possible_truncation)]
                let gap = got.gap_before.min(GAP_FILL_MAX) as usize;
                let silence = vec![0.0; gap];
                self.decimator.push(&silence, &mut self.wire);
            }
            if got.len == 0 {
                break;
            }
            self.private_now = got.private;
            self.private_turn |= got.private;
            let run = self.read.get(..got.len).unwrap_or_default();
            if got.muted {
                // Muted: nobody hears it, the provider included; its clock still runs.
                let silence = vec![0.0; run.len()];
                self.decimator.push(&silence, &mut self.wire);
            } else {
                self.decimator.push(run, &mut self.wire);
            }
        }
        if self.wire.len() >= APPEND_MIN {
            let event = protocol::append(&self.wire);
            self.wire.clear();
            send(&mut self.socket, &event)?;
        }
        Ok(())
    }

    fn emit(&self, event: DuplexEvent) -> Result<(), String> {
        self.outbox.send(event).map_err(|_| "the performer stopped listening".to_owned())
    }

    fn server(&mut self, message: &str) -> Result<(), String> {
        let event = match protocol::decode(message) {
            Ok(event) => event,
            Err(message) => return self.emit(DuplexEvent::error(message, false)),
        };
        match event {
            Server::SpeechStarted => {
                self.private_turn = self.private_now;
                self.emit(DuplexEvent::SpeechStarted)
            }
            Server::SpeechStopped { item } => {
                if let Some(item) = item {
                    self.turn_privacy.push_back((item, self.private_turn));
                    while self.turn_privacy.len() > TURNS_KEPT {
                        self.turn_privacy.pop_front();
                    }
                }
                self.emit(DuplexEvent::SpeechStopped)
            }
            Server::Heard { item, text } => {
                let private = item
                    .and_then(|item| self.turn_privacy.iter().find(|(i, _)| *i == item).map(|(_, p)| *p))
                    .unwrap_or(self.private_turn);
                if private { self.emit(DuplexEvent::heard_private(text)) } else { self.emit(DuplexEvent::heard(text)) }
            }
            Server::ReplyStarted { reply } => self.emit(DuplexEvent::reply_started(reply)),
            Server::Audio { reply, item, pcm } => self.emit(DuplexEvent::reply_audio(reply, item, pcm, WIRE_RATE)),
            Server::Text { reply, delta } => self.emit(DuplexEvent::reply_text(reply, delta)),
            Server::ReplyDone { reply, cancelled, usage } => {
                self.emit(DuplexEvent::reply_done(reply, cancelled, usage))
            }
            Server::Error { message, code } => {
                let code = code.as_deref().unwrap_or_default();
                if QUIET_CODES.contains(&code) {
                    return Ok(());
                }
                if FATAL_CODES.contains(&code) {
                    return Err(format!("the provider ended the session: {message}"));
                }
                self.emit(DuplexEvent::error(format!("the provider said: {message}"), false))
            }
            Server::Other => Ok(()),
        }
    }
}
