//! Pose clips (RG-06): key poses from the pose library placed in time, looped or played once.
//!
//! A [`PoseClip`] is the data locomotion's jump, sit and emote clips and the wave, yawn and walk examples are made of
//! (`docs/design/input-motion.md` §7.2). Sampling it at a time gives a [`Posture`] (or the [`HumanoidPose`] the
//! animator plays): the two keys around the time, blended with the clip's [`Easing`].
//!
//! # File format
//!
//! One TOML file per clip, in `library/poses/clips/`:
//!
//! ```toml
//! id = "wave"
//! summary = "A looping hello."
//! length = 0.6          # seconds; every key time is inside 0..=length
//! looped = true         # a looped clip returns from its last key to its first at `length`
//! easing = "smooth"     # "linear", "smooth" or "smoother"
//!
//! [[keys]]
//! t = 0.0
//! pose = "wave-right"   # a pose id
//! mirror = false        # optional: use the mirror of the pose
//! contacts = { left = true }   # optional: foot contacts here; a side left out is the planter's own detection
//! ```
//!
//! # Seams
//!
//! Easing `smooth` and `smoother` stop at every key (zero speed), so the clip is C1-continuous at every key including
//! the loop seam. `linear` does not, so a looped clip refuses it.

use std::sync::OnceLock;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::id::Slug;
use serde::Deserialize;

use super::{PoseError, PoseLibrary, Posture};
use crate::hands::{HandLibrary, HandRest};

/// Why a clip file failed to load.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ClipError {
    /// The file is not valid TOML of the expected layout.
    #[error("clip file {file}: {message}")]
    Parse {
        /// Which file.
        file: String,
        /// What the parser said.
        message: String,
    },
    /// The id is not a valid name (`a-z`, `0-9`, `-`, `_`).
    #[error("clip {id:?} is not a valid name: {message}")]
    BadName {
        /// The id as written.
        id: String,
        /// Which rule it breaks.
        message: String,
    },
    /// The summary is empty.
    #[error("clip {0} has no summary")]
    EmptySummary(String),
    /// The length is not finite and above zero.
    #[error("clip {id}: length {length} is not a finite number above zero and at most {} s", MAX_LENGTH)]
    BadLength {
        /// The clip.
        id: String,
        /// The value found.
        length: f32,
    },
    /// The clip has no keys.
    #[error("clip {0} has no keys")]
    NoKeys(String),
    /// A key time is not finite or lies outside the clip (a looped clip's last key must be before `length`).
    #[error("clip {id}: key {index} is at {t} s, outside the clip")]
    KeyOutside {
        /// The clip.
        id: String,
        /// The key's position.
        index: usize,
        /// The time found.
        t: f32,
    },
    /// A key is not later than the one before it.
    #[error("clip {id}: key {index} at {t} s is not after the key before it")]
    NotIncreasing {
        /// The clip.
        id: String,
        /// The key's position.
        index: usize,
        /// The time found.
        t: f32,
    },
    /// A key names a pose that no library has.
    #[error("clip {id}: key {index} names the pose {pose:?}, which no library has")]
    UnknownPose {
        /// The clip.
        id: String,
        /// The key's position.
        index: usize,
        /// The pose id.
        pose: String,
    },
    /// A key asks for the mirror of a pose that is not mirrorable.
    #[error("clip {id}: key {index} mirrors the pose {pose:?}, which is not mirrorable")]
    NotMirrorable {
        /// The clip.
        id: String,
        /// The key's position.
        index: usize,
        /// The pose id.
        pose: String,
    },
    /// The easing is not one of the known names.
    #[error("clip {id}: no easing called {easing:?}")]
    UnknownEasing {
        /// The clip.
        id: String,
        /// The name found.
        easing: String,
    },
    /// A looped clip with an easing that moves at the keys would jump in speed at the seam.
    #[error("clip {id}: a looped clip needs an easing that stops at the keys, not {easing:?}: its seam would not be C1")]
    SeamNotSmooth {
        /// The clip.
        id: String,
        /// The easing's name.
        easing: &'static str,
    },
    /// Two clips use the same id.
    #[error("two clips are called {0}")]
    DuplicateId(String),
    /// A pose or its hand shape failed to resolve.
    #[error(transparent)]
    Pose(#[from] PoseError),
}

/// How a segment between two keys is paced. A segment moves from the key before to the key after as the easing of
/// its time fraction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Easing {
    /// Constant speed. Not C1 at the keys, so not allowed on a looped clip.
    Linear,
    /// Smoothstep: starts and ends at rest, so every key is C1.
    Smooth,
    /// Smootherstep: at rest at both ends with zero acceleration there too.
    Smoother,
}

impl Easing {
    /// The easing called `name` (`"linear"`, `"smooth"`, `"smoother"`).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "linear" => Some(Self::Linear),
            "smooth" => Some(Self::Smooth),
            "smoother" => Some(Self::Smoother),
            _ => None,
        }
    }

    /// The file name of the easing.
    pub fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Smooth => "smooth",
            Self::Smoother => "smoother",
        }
    }

    /// Maps the fraction `u` (`0..=1`) of a segment to the blend weight; exact `0` at `0` and exact `1` at `1`.
    pub fn apply(self, u: f32) -> f32 {
        let u = u.clamp(0.0, 1.0);
        match self {
            Self::Linear => u,
            Self::Smooth => u * u * (3.0 - 2.0 * u),
            Self::Smoother => u * u * u * (u * (u * 6.0 - 15.0) + 10.0),
        }
    }

    /// Whether the pace is zero at both ends of a segment, which makes the join between segments C1.
    fn stops_at_keys(self) -> bool {
        !matches!(self, Self::Linear)
    }
}

/// The longest clip, in seconds. It keeps every time sum in sampling far from overflow and precision loss.
pub const MAX_LENGTH: f32 = 3600.0;

/// One key of a clip: a pose at a time, with the foot contacts there.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ClipKey {
    /// Seconds from the clip's start.
    pub t: f32,
    /// The library pose the key was made from (for display; the clip plays [`ClipKey::posture`]).
    pub pose: String,
    /// The pose resolved, and mirrored if the key asked.
    pub posture: Posture,
    /// Left and right foot contact from this key on, in the shape `FootPlanting::set_contacts` takes: `None` leaves
    /// the planter's own detection.
    pub contacts: [Option<bool>; 2],
}

/// A clip: key poses in time (see the [module](self)). Make one with [`PoseClip::parse`], or take a built-in from
/// [`ClipLibrary::builtin`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct PoseClip {
    /// The library name (`wave`).
    pub id: String,
    /// One line saying what the clip is.
    pub summary: String,
    /// Seconds a play takes; a looped clip repeats with this period.
    pub length: f32,
    /// Whether the clip repeats; the seam from the last key to the first is then part of the clip.
    pub looped: bool,
    /// How each segment is paced.
    pub easing: Easing,
    /// The keys, in strictly increasing time; never empty (see [`PoseClip::keys`]).
    keys: Vec<ClipKey>,
}

/// One key in a file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyFile {
    t: f32,
    pose: String,
    #[serde(default)]
    mirror: bool,
    #[serde(default)]
    contacts: ContactsFile,
}

/// The `contacts` table of a key.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ContactsFile {
    left: Option<bool>,
    right: Option<bool>,
}

/// A clip file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipFile {
    id: String,
    summary: String,
    length: f32,
    #[serde(default)]
    looped: bool,
    easing: String,
    keys: Vec<KeyFile>,
}

impl PoseClip {
    /// Parses one clip file and checks it: a valid id and summary, a finite length above zero and at most
    /// [`MAX_LENGTH`] (3600 s), at least one key, key
    /// times finite, strictly increasing and inside `0..=length` (below `length` when looped), every pose in
    /// one of `libraries` (the first that has it wins), a known easing, and an easing that stops at the keys when
    /// looped. `label` names the file in error messages.
    ///
    /// # Errors
    /// The first [`ClipError`] found.
    pub fn parse(
        label: &str,
        text: &str,
        libraries: &[&PoseLibrary],
        hands: &HandLibrary,
    ) -> Result<Self, ClipError> {
        let file: ClipFile =
            toml::from_str(text).map_err(|e| ClipError::Parse { file: label.to_owned(), message: e.to_string() })?;
        let id = file.id;
        if let Err(e) = Slug::new(&id) {
            return Err(ClipError::BadName { id, message: e.to_string() });
        }
        if file.summary.trim().is_empty() {
            return Err(ClipError::EmptySummary(id));
        }
        if !(file.length.is_finite() && file.length > 0.0 && file.length <= MAX_LENGTH) {
            return Err(ClipError::BadLength { id, length: file.length });
        }
        let Some(easing) = Easing::from_name(&file.easing) else {
            return Err(ClipError::UnknownEasing { id, easing: file.easing });
        };
        if file.looped && !easing.stops_at_keys() {
            return Err(ClipError::SeamNotSmooth { id, easing: easing.name() });
        }
        if file.keys.is_empty() {
            return Err(ClipError::NoKeys(id));
        }
        let mut keys: Vec<ClipKey> = Vec::with_capacity(file.keys.len());
        for (index, k) in file.keys.into_iter().enumerate() {
            let inside = k.t.is_finite() && k.t >= 0.0 && if file.looped { k.t < file.length } else { k.t <= file.length };
            if !inside {
                return Err(ClipError::KeyOutside { id, index, t: k.t });
            }
            if keys.last().is_some_and(|prev| k.t <= prev.t) {
                return Err(ClipError::NotIncreasing { id, index, t: k.t });
            }
            let Some(def) = libraries.iter().find_map(|l| l.get(&k.pose)) else {
                return Err(ClipError::UnknownPose { id, index, pose: k.pose });
            };
            if k.mirror && !def.mirrorable {
                return Err(ClipError::NotMirrorable { id, index, pose: k.pose });
            }
            let posture = def.posture(hands)?;
            let posture = if k.mirror { posture.mirrored() } else { posture };
            keys.push(ClipKey {
                t: k.t,
                pose: k.pose,
                posture,
                contacts: [k.contacts.left, k.contacts.right],
            });
        }
        Ok(Self { id, summary: file.summary, length: file.length, looped: file.looped, easing, keys })
    }

    /// The keys, in strictly increasing time. A parsed clip has at least one.
    pub fn keys(&self) -> &[ClipKey] {
        &self.keys
    }

    /// The clip with left and right swapped: every key posture mirrored and the contacts exchanged. Exact: mirroring
    /// twice gives back the same clip. A walk mirrored and shifted by half a cycle is the walk.
    #[must_use]
    pub fn mirrored(&self) -> Self {
        let keys = self
            .keys
            .iter()
            .map(|k| ClipKey {
                posture: k.posture.mirrored(),
                contacts: [k.contacts[1], k.contacts[0]],
                ..k.clone()
            })
            .collect();
        Self { keys, ..self.clone() }
    }

    /// The posture at `t` seconds. A looped clip wraps `t` into `0..length`; a one-shot clip holds its first key before
    /// it and its last key after it, and a time that is not a number is the start. Exactly a key's posture at a
    /// key's time. Never allocates.
    #[must_use]
    pub fn sample_posture(&self, t: f32) -> Posture {
        let (Some(first), Some(last)) = (self.keys.first(), self.keys.last()) else {
            return Posture { body: HumanoidPose::REST, left: super::Hand::NONE, right: super::Hand::NONE };
        };
        if !t.is_finite() {
            return first.posture;
        }
        // The segment to play, how far into it `t` is, and how long it is: every sum is of differences inside `0..=length`
        // (at most `MAX_LENGTH`), so none can overflow.
        let seam = (self.length - last.t) + first.t;
        let last_index = self.keys.len() - 1;
        let (segment, elapsed, span) = if self.looped {
            let t = t.rem_euclid(self.length);
            // Before the first key the clip is still on the seam that began at the last key one cycle ago.
            if t < first.t { (last_index, (self.length - last.t) + t, seam) } else { self.within(t, seam) }
        } else if t <= first.t {
            return first.posture;
        } else if t >= last.t {
            return last.posture;
        } else {
            self.within(t, seam)
        };
        let (from, to) = (&self.keys[segment], self.keys.get(segment + 1).unwrap_or(first));
        let u = elapsed / span;
        Posture::blend(&from.posture, &to.posture, self.easing.apply(u))
    }

    /// The pose the animator plays at `t` seconds: [`PoseClip::sample_posture`] with the hand shapes applied on a rig
    /// whose thumbs rest as `rest`.
    #[must_use]
    pub fn sample(&self, t: f32, rest: &HandRest) -> HumanoidPose {
        self.sample_posture(t).to_humanoid(rest)
    }

    /// The foot contacts at `t` seconds: those of the latest key at or before `t` (the clip's last key before its
    /// first key, on a loop), for `FootPlanting::set_contacts`.
    #[must_use]
    pub fn contacts(&self, t: f32) -> [Option<bool>; 2] {
        let (Some(first), Some(last)) = (self.keys.first(), self.keys.last()) else {
            return [None, None];
        };
        let t = if !t.is_finite() {
            return first.contacts;
        } else if self.looped {
            t.rem_euclid(self.length)
        } else {
            t
        };
        if t < first.t {
            return if self.looped { last.contacts } else { first.contacts };
        }
        self.keys[self.segment(t)].contacts
    }

    /// The segment `t` is in, how far into it, and how long it is, for `t` at or after the first key and (when
    /// looped) before `length`. `seam` is the length of the segment from the last key to the first.
    fn within(&self, t: f32, seam: f32) -> (usize, f32, f32) {
        let i = self.segment(t);
        let from = self.keys[i].t;
        match self.keys.get(i + 1) {
            Some(next) => (i, t - from, next.t - from),
            None => (i, t - from, seam),
        }
    }

    /// The index of the last key at or before `t`; `t` is at or after the first key.
    fn segment(&self, t: f32) -> usize {
        self.keys.partition_point(|k| k.t <= t).saturating_sub(1)
    }
}

/// A set of clips with unique ids, in a stable order.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipLibrary {
    clips: Vec<PoseClip>,
}

/// The built-in clip files, in the order [`ClipLibrary::iter`] gives them.
const BUILTIN_CLIPS: [(&str, &str); 4] = [
    ("wave", include_str!("../../library/poses/clips/wave.toml")),
    ("wave-both", include_str!("../../library/poses/clips/wave-both.toml")),
    ("yawn", include_str!("../../library/poses/clips/yawn.toml")),
    ("walk", include_str!("../../library/poses/clips/walk.toml")),
];

/// The key poses only the built-in clips use, beside them in `library/poses/clips/`.
const BUILTIN_KEY_POSES: [(&str, &str); 8] = [
    ("stand-neutral", include_str!("../../library/poses/clips/stand-neutral.pose.toml")),
    ("wave-right-swing", include_str!("../../library/poses/clips/wave-right-swing.pose.toml")),
    ("wave-both", include_str!("../../library/poses/clips/wave-both.pose.toml")),
    ("wave-both-swing", include_str!("../../library/poses/clips/wave-both-swing.pose.toml")),
    ("yawn-anticipation", include_str!("../../library/poses/clips/yawn-anticipation.pose.toml")),
    ("yawn-peak", include_str!("../../library/poses/clips/yawn-peak.pose.toml")),
    ("walk-contact", include_str!("../../library/poses/clips/walk-contact.pose.toml")),
    ("walk-passing", include_str!("../../library/poses/clips/walk-passing.pose.toml")),
];

/// The built-in clips, or why they failed to load, parsed once.
static BUILTIN_LIBRARY: OnceLock<Result<ClipLibrary, ClipError>> = OnceLock::new();

impl ClipLibrary {
    /// Parses clip files (`sources` pairs a label with the text) against `libraries` and `hands`.
    ///
    /// # Errors
    /// The first [`ClipError`], including a repeated clip id.
    pub fn parse(
        sources: &[(&str, &str)],
        libraries: &[&PoseLibrary],
        hands: &HandLibrary,
    ) -> Result<Self, ClipError> {
        let mut clips: Vec<PoseClip> = Vec::with_capacity(sources.len());
        for (label, text) in sources {
            let clip = PoseClip::parse(label, text, libraries, hands)?;
            if clips.iter().any(|c| c.id == clip.id) {
                return Err(ClipError::DuplicateId(clip.id));
            }
            clips.push(clip);
        }
        Ok(Self { clips })
    }

    /// The clips that ship with Omuzaki: `wave`, `wave-both`, `yawn` and `walk`.
    ///
    /// # Errors
    /// Only if the embedded files are broken, which the crate's tests rule out.
    pub fn builtin() -> Result<&'static Self, ClipError> {
        BUILTIN_LIBRARY
            .get_or_init(|| {
                let key_poses = PoseLibrary::parse(&BUILTIN_KEY_POSES)?;
                Self::parse(&BUILTIN_CLIPS, &[PoseLibrary::builtin()?, &key_poses], HandLibrary::builtin().map_err(PoseError::from)?)
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// The clip called `id`.
    pub fn get(&self, id: &str) -> Option<&PoseClip> {
        self.clips.iter().find(|c| c.id == id)
    }

    /// Every clip, in file order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &PoseClip> {
        self.clips.iter()
    }
}
