//! Voice commands (AP-10): the person says "wave" and the show waves.
//!
//! Speech becomes an automation event and the automation graph turns it into ops, intents and clips, so voice commands
//! need no action catalogue of their own:
//!
//! ```text
//!  raw mic --listen tap--> SpeechHost --Final--> CommandListener --match_transcript--> voice.command --> EventSink
//! ```
//!
//! - [`matcher`]: the phrase list and the tolerant whole-phrase match, English and Japanese.
//! - [`listener`]: [`CommandListener`] reads final transcripts of the person's raw microphone only (never the AI's voice,
//!   TTS or the soundboard), and publishes one [`VoiceCommand`] per match. No transcript text leaves it.
//! - [`VoiceCommands`]: the shared state the ops and the UI read: on or off (off by default), the phrase list, and the
//!   status on a watch.
//! - The `voice_commands.*` ops: `enable` (a person only), `disable` (anyone), `status`, `set_phrases` (kept in the
//!   person's profile as the `voice_commands.phrases` setting, beside the voice calibration, never in a show).

pub mod listener;
pub mod matcher;
mod ops;

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::EventPayload;
use omuzaki_live_api::register_event_kind;
use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::setting::Setting;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use listener::{CommandListener, ListenerStats, ListenerThread};
pub use matcher::{Command, Match, PhraseError, PhraseList, match_transcript};
pub use ops::{VoiceCommandsDisable, VoiceCommandsEnable, VoiceCommandsSetPhrases, VoiceCommandsStatus};

/// The phrase list a new person starts with.
const DEFAULT_PHRASES: &str = include_str!("default-phrases.toml");

/// The default phrase list: ten command ids (`wave`, `nod`, `shake_head`, `smile`, `surprised`, `sad`, `angry`, `spin`,
/// `bow`, `clip`) in English and Japanese.
pub fn default_phrases() -> PhraseList {
    // The shipped list is checked by a test; an unreadable one would mean no commands rather than a crash.
    PhraseList::from_toml(DEFAULT_PHRASES).unwrap_or_default()
}

/// The person said a voice command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct VoiceCommand {
    /// The command id (`wave`), as shows and templates name it.
    pub id: String,
    /// The phrase-list entry that matched, never what was actually said.
    pub phrase: String,
    /// How closely it matched: 1 for exact, down to about 0.8 at the tolerance.
    pub score: f32,
}

impl VoiceCommand {
    /// The command `id`, matched by `phrase` at `score`.
    pub fn new(id: impl Into<String>, phrase: impl Into<String>, score: f32) -> Self {
        Self { id: id.into(), phrase: phrase.into(), score }
    }
}

impl From<Match> for VoiceCommand {
    fn from(m: Match) -> Self {
        Self { id: m.id, phrase: m.phrase, score: m.score }
    }
}

impl EventPayload for VoiceCommand {
    const KIND: &'static str = "voice.command";
    const SUMMARY: &'static str = "The person said a voice command.";
}

register_event_kind!(VoiceCommand);

/// `voice_commands.phrases`: the person's phrase list. Personal like the voice calibration: a `User` key no profile
/// holds, and an agent's change waits for the person.
pub struct Phrases;

impl Setting for Phrases {
    type Value = PhraseList;
    const META: SettingMeta = SettingMeta::new(
        "voice_commands.phrases",
        "What you say for each voice command, per language",
        SettingScope::User,
        AgentWrite::Confirm,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> PhraseList {
        default_phrases()
    }

    fn check(value: &PhraseList) -> Result<(), SettingError> {
        value.check().map_err(|e| SettingError::invalid(Self::META.key, e.to_string()))
    }
}

register_setting!(Phrases);

/// `voice_commands.enabled`: whether voice commands listen. Off until the person turns them on; no agent ever does.
pub struct Enabled;

impl Setting for Enabled {
    type Value = bool;
    const META: SettingMeta = SettingMeta::new(
        "voice_commands.enabled",
        "Listen for voice commands on your microphone",
        SettingScope::User,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> bool {
        false
    }
}

register_setting!(Enabled);

/// What voice commands are doing, for the status light and `voice_commands.status`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CommandStatus {
    /// Turned off (the default).
    Off,
    /// On, nothing heard yet.
    Listening,
    /// On; the last command heard.
    Heard {
        /// Its id.
        id: String,
        /// When the utterance ended, on the runner clock.
        at: Seconds,
    },
    /// On, but not hearing anything.
    Paused {
        /// Why, for the person: "the microphone is muted".
        why: String,
    },
}

/// The shared state of voice commands: on or off, the phrase list, the status. The app makes one, inserts it as a
/// service (`ServicesBuilder::insert::<VoiceCommands>`) and hands a clone to the [`CommandListener`]. Cloning shares it.
#[derive(Clone, Default)]
pub struct VoiceCommands(Arc<Shared>);

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    enabled: bool,
    phrases: Arc<PhraseList>,
    /// Why nothing is heard, when something stops it (the mute, a wrong tap).
    paused: Option<String>,
    /// The last command heard.
    last: Option<(String, Seconds)>,
    /// Bumped on every change of [`VoiceCommands::status`].
    version: u64,
}

impl State {
    fn status(&self) -> CommandStatus {
        match (self.enabled, &self.paused, &self.last) {
            (false, _, _) => CommandStatus::Off,
            (true, Some(why), _) => CommandStatus::Paused { why: why.clone() },
            (true, None, Some((id, at))) => CommandStatus::Heard { id: id.clone(), at: *at },
            (true, None, None) => CommandStatus::Listening,
        }
    }
}

impl VoiceCommands {
    /// Off, with `phrases`.
    pub fn new(phrases: PhraseList) -> Self {
        let commands = Self::default();
        commands.lock().phrases = Arc::new(phrases);
        commands
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes the state with `change` and wakes the watchers when the status moved.
    fn update(&self, change: impl FnOnce(&mut State)) {
        let mut state = self.lock();
        let before = state.status();
        change(&mut state);
        if state.status() != before {
            state.version += 1;
            self.0.changed.notify_all();
        }
    }

    /// Whether voice commands are on.
    pub fn enabled(&self) -> bool {
        self.lock().enabled
    }

    /// Turns voice commands on or off. The `voice_commands.*` ops decide who may; this only does it. Turning them off
    /// forgets the last command heard.
    pub fn set_enabled(&self, on: bool) {
        self.update(|state| {
            state.enabled = on;
            if !on {
                state.last = None;
            }
        });
    }

    /// The phrase list.
    pub fn phrases(&self) -> Arc<PhraseList> {
        Arc::clone(&self.lock().phrases)
    }

    /// Replaces the phrase list.
    pub fn set_phrases(&self, phrases: PhraseList) {
        self.lock().phrases = Arc::new(phrases);
    }

    /// The status now.
    pub fn status(&self) -> CommandStatus {
        self.lock().status()
    }

    /// The status and its version: a watcher passes the version back to [`watch`](Self::watch).
    pub fn versioned(&self) -> (u64, CommandStatus) {
        let state = self.lock();
        (state.version, state.status())
    }

    /// Waits up to `timeout` for the status to change from version `since`, and returns the newest one (the UI's
    /// watch). Returns at once when it already has.
    pub fn watch(&self, since: u64, timeout: Duration) -> (u64, CommandStatus) {
        let state = self.lock();
        let (state, _) = self
            .0
            .changed
            .wait_timeout_while(state, timeout, |s| s.version == since)
            .unwrap_or_else(PoisonError::into_inner);
        (state.version, state.status())
    }

    /// The listener heard `id` in an utterance that ended at `at`.
    pub(crate) fn heard(&self, id: &str, at: Seconds) {
        self.update(|state| state.last = Some((id.to_owned(), at)));
    }

    /// The listener can't hear for `why` (`None` when it can again).
    pub(crate) fn pause(&self, why: Option<&str>) {
        self.update(|state| state.paused = why.map(str::to_owned));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_phrases_read() {
        let list = PhraseList::from_toml(DEFAULT_PHRASES).unwrap();
        assert_eq!(list.command.len(), 20);
        assert_eq!(default_phrases(), list);
    }

    #[test]
    fn the_status_follows_the_state_and_wakes_watchers() {
        let commands = VoiceCommands::new(default_phrases());
        assert_eq!(commands.status(), CommandStatus::Off);
        let (v0, _) = commands.versioned();
        commands.set_enabled(true);
        let (v1, status) = commands.watch(v0, Duration::ZERO);
        assert_eq!((v1, status), (v0 + 1, CommandStatus::Listening));
        commands.heard("wave", Seconds(3.0));
        assert_eq!(commands.status(), CommandStatus::Heard { id: "wave".into(), at: Seconds(3.0) });
        commands.pause(Some("the microphone is muted"));
        assert_eq!(commands.status(), CommandStatus::Paused { why: "the microphone is muted".into() });
        commands.pause(None);
        commands.set_enabled(false);
        assert_eq!(commands.status(), CommandStatus::Off);
        commands.set_enabled(true);
        assert_eq!(commands.status(), CommandStatus::Listening, "turning off forgets the last command");
    }
}
