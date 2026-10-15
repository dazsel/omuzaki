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
#[non_exhaustive]
pub struct CastEntry {
    /// The member id in the scene.
    pub member: String,
    /// The character's id.
    pub character: String,
    /// The character's content hash, or the imported avatar's store hash.
    pub hash: String,
}

impl CastEntry {
    /// A cast member with its required fields.
    pub fn new(member: impl Into<String>, character: impl Into<String>, hash: impl Into<String>) -> Self {
        Self { member: member.into(), character: character.into(), hash: hash.into() }
    }
}

/// A tracker source: the index `Track` records use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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

impl SourceEntry {
    /// A tracker source with its required fields.
    pub fn new(index: u8, member: impl Into<String>, profile: impl Into<String>, kind: impl Into<String>) -> Self {
        Self { index, member: member.into(), profile: profile.into(), kind: kind.into() }
    }
}

/// An audio voice: the index `Voice`, `Cue` and `Withheld` records use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct VoiceEntry {
    /// The index in `Voice` and `Cue` records.
    pub index: u8,
    /// The cast member it drives.
    pub member: String,
    /// The tap the voice is read from.
    pub tap: String,
}

impl VoiceEntry {
    /// An audio voice with its required fields.
    pub fn new(index: u8, member: impl Into<String>, tap: impl Into<String>) -> Self {
        Self { index, member: member.into(), tap: tap.into() }
    }
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
    /// A cause this version does not know (a newer minor). The run is still a gap; the cause is not named.
    #[serde(other)]
    Unknown,
}

/// A muted or private run: the gap is shown, never silent. Open (`to` is `None`) at the start of a run.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
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

fn finite(at: Seconds) -> Seconds {
    if at.0.is_finite() { at } else { Seconds(0.0) }
}

impl Withheld {
    /// A run that is open from `from`: `to` is `None` until [`Withheld::with_to`] closes it. A time JSON cannot hold
    /// (NaN or infinite) is stored as 0, so every run the constructors build reads back.
    pub fn new(voice: u8, cause: WithheldCause, from: Seconds) -> Self {
        Self { voice, cause, from: finite(from), to: None }
    }

    /// The same run, closed at `to` (stored as 0 if it is NaN or infinite).
    #[must_use]
    pub fn with_to(self, to: Seconds) -> Self {
        Self { to: Some(finite(to)), ..self }
    }
}

/// A project's name: never empty or blank, so a full State always names its project (T2). The one check both the
/// constructors and the reader go through.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectName(String);

impl ProjectName {
    /// The name, or `None` if it is empty or only whitespace.
    pub fn new(name: impl Into<String>) -> Option<Self> {
        let name = name.into();
        if name.trim().is_empty() { None } else { Some(Self(name)) }
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProjectName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::new(name).ok_or_else(|| serde::de::Error::custom("`project` must not be empty or blank"))
    }
}

/// The start state, repeated at every group start so any keyframe starts a clip. Bounded: at most 16 KiB serialised
/// (T2); providers that would pass the cap are named in `omitted`, and on overflow only the minimal fields remain.
///
/// Later fields are added with `#[serde(default)]`, so an older reader keeps reading the record. Empty and default
/// fields are not written, so the minimal State serialises to exactly `doc_version`, `after_lsn`, `withheld` and
/// `overflow` (T2); `overflow` is always written.
///
/// Reading checks what the constructors guarantee: `project` may be absent only in the minimal State
/// (`overflow: true`); any other State without a non-blank one is an error, not a full State with no name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawState")]
#[non_exhaustive]
pub struct StateRecord {
    /// The document version.
    pub doc_version: DocVersion,
    /// The project's name. `None` only in the minimal State, which leaves it out (T2): every constructor but
    /// [`StateRecord::minimal`] sets it, and the reader refuses a full State without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectName>,
    /// The active scene.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The active camera.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    /// The cast.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cast: Vec<CastEntry>,
    /// The tracker sources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceEntry>,
    /// The audio voices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voices: Vec<VoiceEntry>,
    /// Registry slot to model ref, for the tracker and lip sync in use.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, String>,
    /// The playout delay `D`, in seconds.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub delay_s: f32,
    /// The last journal entry already reflected.
    pub after_lsn: Lsn,
    /// The muted or private runs open at this frame.
    pub withheld: Vec<Withheld>,
    /// Providers left out by the size cap: at most 32 names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<String>,
    /// How many more were left out beyond the names listed.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub omitted_more: u32,
    /// True for the minimal State: the cast and live state are unknown for this group.
    #[serde(default)]
    pub overflow: bool,
    /// Live state by provider name (see [`register_take_state!`](crate::register_take_state!)). A reader skips a name
    /// it does not know and reports it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub live: BTreeMap<String, Value>,
}

/// What serde reads before [`StateRecord`] checks it: the same fields, no rules.
#[derive(Deserialize)]
struct RawState {
    doc_version: DocVersion,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    scene: Option<SceneId>,
    #[serde(default)]
    camera: Option<String>,
    #[serde(default)]
    cast: Vec<CastEntry>,
    #[serde(default)]
    sources: Vec<SourceEntry>,
    #[serde(default)]
    voices: Vec<VoiceEntry>,
    #[serde(default)]
    models: BTreeMap<String, String>,
    #[serde(default)]
    delay_s: f32,
    after_lsn: Lsn,
    withheld: Vec<Withheld>,
    #[serde(default)]
    omitted: Vec<String>,
    #[serde(default)]
    omitted_more: u32,
    #[serde(default)]
    overflow: bool,
    #[serde(default)]
    live: BTreeMap<String, Value>,
}

impl TryFrom<RawState> for StateRecord {
    type Error = String;

    fn try_from(raw: RawState) -> Result<Self, Self::Error> {
        // The one rule: a name is kept only if it is a real one, and only the overflow fallback may go without.
        let project = raw.project.and_then(ProjectName::new);
        if project.is_none() && !raw.overflow {
            return Err("a State without `overflow: true` needs a `project` that is not empty or blank (T2)".to_owned());
        }
        Ok(Self {
            doc_version: raw.doc_version,
            project,
            scene: raw.scene,
            camera: raw.camera,
            cast: raw.cast,
            sources: raw.sources,
            voices: raw.voices,
            models: raw.models,
            delay_s: raw.delay_s,
            after_lsn: raw.after_lsn,
            withheld: raw.withheld,
            omitted: raw.omitted,
            omitted_more: raw.omitted_more,
            overflow: raw.overflow,
            live: raw.live,
        })
    }
}

// `skip_serializing_if` passes the field by reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_f32(v: &f32) -> bool {
    *v == 0.0
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

impl StateRecord {
    /// A full State with its required fields, the project's name among them; everything else is empty. Set the rest
    /// with `with_*`. Only [`StateRecord::minimal`] leaves the project out.
    pub fn new(doc_version: DocVersion, project: ProjectName, after_lsn: Lsn, withheld: Vec<Withheld>) -> Self {
        Self::base(doc_version, Some(project), after_lsn, withheld)
    }

    /// The minimal State the feed writes when the full one would pass 16 KiB (T2): the same required fields, with
    /// `overflow` set and nothing else. Readers treat the cast and live state as unknown for that group.
    pub fn minimal(doc_version: DocVersion, after_lsn: Lsn, withheld: Vec<Withheld>) -> Self {
        Self { overflow: true, ..Self::base(doc_version, None, after_lsn, withheld) }
    }

    fn base(doc_version: DocVersion, project: Option<ProjectName>, after_lsn: Lsn, withheld: Vec<Withheld>) -> Self {
        Self {
            doc_version,
            project,
            scene: None,
            camera: None,
            cast: Vec::new(),
            sources: Vec::new(),
            voices: Vec::new(),
            models: BTreeMap::new(),
            delay_s: 0.0,
            after_lsn,
            withheld,
            omitted: Vec::new(),
            omitted_more: 0,
            overflow: false,
            live: BTreeMap::new(),
        }
    }

    /// The same State with its active scene.
    #[must_use]
    pub fn with_scene(self, scene: SceneId) -> Self {
        Self { scene: Some(scene), ..self }
    }

    /// The same State with its active camera.
    #[must_use]
    pub fn with_camera(self, camera: impl Into<String>) -> Self {
        Self { camera: Some(camera.into()), ..self }
    }

    /// The same State with its cast.
    #[must_use]
    pub fn with_cast(self, cast: Vec<CastEntry>) -> Self {
        Self { cast, ..self }
    }

    /// The same State with its tracker sources.
    #[must_use]
    pub fn with_sources(self, sources: Vec<SourceEntry>) -> Self {
        Self { sources, ..self }
    }

    /// The same State with its audio voices.
    #[must_use]
    pub fn with_voices(self, voices: Vec<VoiceEntry>) -> Self {
        Self { voices, ..self }
    }

    /// The same State with its model refs by registry slot.
    #[must_use]
    pub fn with_models(self, models: BTreeMap<String, String>) -> Self {
        Self { models, ..self }
    }

    /// The same State with the playout delay `D`, in seconds. A value JSON cannot hold (NaN or infinite) is stored
    /// as 0, so every State the constructors build reads back.
    #[must_use]
    pub fn with_delay_s(self, delay_s: f32) -> Self {
        Self { delay_s: if delay_s.is_finite() { delay_s } else { 0.0 }, ..self }
    }

    /// The same State with the providers the size cap left out: the names listed, and how many more there were.
    #[must_use]
    pub fn with_omitted(self, omitted: Vec<String>, omitted_more: u32) -> Self {
        Self { omitted, omitted_more, ..self }
    }

    /// The same State with its live state by provider name.
    #[must_use]
    pub fn with_live(self, live: BTreeMap<String, Value>) -> Self {
        Self { live, ..self }
    }
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
