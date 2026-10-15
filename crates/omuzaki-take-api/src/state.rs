//! The `State` record and the live-state providers that fill it (design T1, T5).

use std::collections::BTreeMap;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::DocVersion;
use omuzaki_history_api::Lsn;
use omuzaki_stage_api::SceneId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A cast member as the take found it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CastEntry {
    /// The member id in the scene.
    pub member: String,
    /// The character's id.
    pub character: String,
    /// The character's content hash, or the imported avatar's store hash.
    pub hash: String,
}

/// A tracker source: the index `Track` records use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceEntry {
    /// The index in `Track` records.
    pub index: u8,
    /// The cast member it drives.
    pub member: String,
    /// The device profile, as the retarget names it.
    pub profile: String,
    /// The kind of source (`iphone`, `webcam`, ...).
    pub kind: String,
}

/// An audio voice: the index `Voice`, `Cue` and `Withheld` records use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceEntry {
    /// The index in `Voice` and `Cue` records.
    pub index: u8,
    /// The cast member it drives.
    pub member: String,
    /// The tap the voice is read from.
    pub tap: String,
}

/// Why a voice's derived values were not recorded (T5).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithheldCause {
    /// The voice was muted.
    Muted,
    /// The voice was private.
    Private,
    /// The voice was heard by the co-host only.
    CohostOnly,
}

/// A muted or private run: the gap is shown, never silent. Open (`to` is `None`) at the start of a run.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Withheld {
    /// The voice index.
    pub voice: u8,
    /// Why.
    pub cause: WithheldCause,
    /// When the run started, on the pipeline clock.
    pub from: Seconds,
    /// When it ended; `None` while it is open.
    pub to: Option<Seconds>,
}

/// The start state, repeated at every group start so any keyframe starts a clip. Bounded: at most 16 KiB serialised
/// (T2); providers that would pass the cap are named in `omitted`, and on overflow only the minimal fields remain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateRecord {
    /// The document version.
    pub doc_version: DocVersion,
    /// The project's name; empty in the minimal State, which leaves it out (T2).
    #[serde(default)]
    pub project: String,
    /// The active scene.
    #[serde(default)]
    pub scene: Option<SceneId>,
    /// The active camera.
    #[serde(default)]
    pub camera: Option<String>,
    /// The cast.
    #[serde(default)]
    pub cast: Vec<CastEntry>,
    /// The tracker sources.
    #[serde(default)]
    pub sources: Vec<SourceEntry>,
    /// The audio voices.
    #[serde(default)]
    pub voices: Vec<VoiceEntry>,
    /// Registry slot to model ref, for the tracker and lip sync in use.
    #[serde(default)]
    pub models: BTreeMap<String, String>,
    /// The playout delay `D`, in seconds.
    #[serde(default)]
    pub delay_s: f32,
    /// The last journal entry already reflected.
    pub after_lsn: Lsn,
    /// The muted or private runs open at this frame.
    pub withheld: Vec<Withheld>,
    /// Providers left out by the size cap: at most 32 names.
    #[serde(default)]
    pub omitted: Vec<String>,
    /// How many more were left out beyond the names listed.
    #[serde(default)]
    pub omitted_more: u32,
    /// True for the minimal State: the cast and live state are unknown for this group.
    #[serde(default)]
    pub overflow: bool,
    /// Live state by provider name (see [`register_take_state!`](crate::register_take_state!)). A reader skips a name it does not know and reports it.
    #[serde(default)]
    pub live: BTreeMap<String, Value>,
}

/// One registered live-state provider. Built by [`register_take_state!`](crate::register_take_state!); never by hand.
#[derive(Copy, Clone)]
pub struct StateProvider {
    /// The key in [`StateRecord::live`].
    pub name: &'static str,
    /// Captures the subsystem's live state now, off the frame thread.
    pub capture: fn() -> Value,
}

inventory::collect!(StateProvider);

/// Every provider linked into this binary, in name order (the order the size cap fills them in).
pub fn state_providers() -> Vec<StateProvider> {
    let mut all: Vec<StateProvider> = inventory::iter::<StateProvider>.into_iter().copied().collect();
    all.sort_by_key(|p| p.name);
    all
}

/// Registers a live-state provider: `register_take_state!("expressions", capture_fn);`, where `capture_fn` is a
/// `fn() -> serde_json::Value`. Put it next to the subsystem with live state (held expressions, the show's variables,
/// the OBS scene, the mic mode). The name is the key under `State.live`; a new provider never needs a format change.
#[macro_export]
macro_rules! register_take_state {
    ($name:literal, $capture:expr) => {
        $crate::__inventory::submit! {
            $crate::state::StateProvider { name: $name, capture: $capture }
        }
        $crate::__record_registration!("take_state", $name);
    };
}
