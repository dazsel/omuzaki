//! The gesture phrase format and the built-in phrases.
//!
//! One TOML file per phrase in `library/gestures/`:
//!
//! ```toml
//! id = "beat-chop"
//! summary = "A small chop of the edge of the hand on each stressed word."
//! kind = "beat"            # beat | deictic | metaphoric | iconic | emblem | fidget
//! hands = "one"            # one | both
//! strokes = [1, 4]         # how many strokes it may repeat, on the following stressed syllables
//! [prep.lead]              # the pre-stroke pose of the gesturing hand (ArmPose numbers, degrees)
//! raise = 12.0
//! elbow = 96.0
//! hand = "open"            # a hand-shape id (RG-09)
//! [stroke]
//! chest = [2.0, 3.0, 0.0]  # optional: pitch, yaw toward the lead side, roll, degrees
//! [stroke.lead]            # where the stroke lands
//! elbow = 70.0
//! hand = "flat"
//! ```
//!
//! A two-handed phrase may give `[prep.other]` and `[stroke.other]`; without them the second hand mirrors the lead.

use std::sync::OnceLock;

use omuzaki_core::id::Slug;
use serde::{Deserialize, Serialize};

use super::super::arm::ArmPose;
use crate::hands::HandLibrary;
use crate::poses::Hand;

/// Why a gesture file failed to load.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum GestureError {
    /// Not valid TOML of the expected layout.
    #[error("gesture file {file}: {message}")]
    Parse {
        /// Which file.
        file: String,
        /// What the parser said.
        message: String,
    },
    /// The id is not a valid name.
    #[error("gesture {0:?} is not a valid name")]
    BadName(String),
    /// The summary is empty.
    #[error("gesture {0} has no summary")]
    EmptySummary(String),
    /// A pose number is outside what an arm can do, or the stroke counts are wrong.
    #[error("gesture {id}: {what}")]
    Implausible {
        /// The gesture.
        id: String,
        /// What is wrong.
        what: String,
    },
    /// A hand names a shape the hand-shape library lacks.
    #[error("gesture {id}: no hand shape called {shape:?}")]
    UnknownHandShape {
        /// The gesture.
        id: String,
        /// The shape.
        shape: String,
    },
    /// Two files use the same id.
    #[error("two gestures are called {0}")]
    DuplicateId(String),
}

/// What a gesture does for speech (the classes of gesture research, plus idle fidgets).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GestureKind {
    /// A rhythmic flick on the stress: no meaning of its own, the most common gesture.
    Beat,
    /// A point at something: me, you, there.
    Deictic,
    /// An abstract idea given a shape: offering, weighing, holding.
    Metaphoric,
    /// A concrete shape or size drawn in the air.
    Iconic,
    /// A conventional sign with its own meaning (a shrug).
    Emblem,
    /// An idle adjustment between phrases, not tied to speech.
    Fidget,
}

/// Whether one hand or both gesture.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Hands {
    /// The leading hand (or the other, for variety); the second hand stays at the talk-rest pose.
    One,
    /// Both hands.
    Both,
}

/// One pose of a phrase, compiled: both arms, both hand shapes and the chest accent.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Key {
    /// The gesturing arm.
    pub lead: ArmPose,
    /// The second arm, for a two-handed phrase.
    pub other: Option<ArmPose>,
    /// The gesturing hand's shape.
    pub lead_hand: Hand,
    /// The second hand's shape, for a two-handed phrase.
    pub other_hand: Hand,
    /// Chest pitch, yaw toward the lead side and roll, degrees.
    pub chest: [f32; 3],
}

/// A gesture phrase, compiled: names resolved, nothing left to look up while it plays.
#[derive(Clone, Debug, PartialEq)]
pub struct Gesture {
    /// Its id.
    pub id: String,
    /// What it means, for `gesture.list`.
    pub summary: String,
    /// Its class.
    pub kind: GestureKind,
    /// One hand or both.
    pub hands: Hands,
    /// Fewest and most strokes.
    pub strokes: [u8; 2],
    /// The pre-stroke pose.
    pub prep: Key,
    /// Where the stroke lands.
    pub stroke: Key,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArmFile {
    #[serde(default)]
    raise: f32,
    #[serde(default)]
    forward: f32,
    #[serde(default)]
    twist: f32,
    #[serde(default)]
    elbow: f32,
    #[serde(default)]
    pronate: f32,
    #[serde(default)]
    wrist: f32,
    #[serde(default)]
    shrug: f32,
    #[serde(default)]
    reach: f32,
    hand: String,
}

impl ArmFile {
    fn pose(&self) -> ArmPose {
        ArmPose {
            raise: self.raise,
            forward: self.forward,
            twist: self.twist,
            elbow: self.elbow,
            pronate: self.pronate,
            wrist: self.wrist,
            shrug: self.shrug,
            reach: self.reach,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyFile {
    lead: ArmFile,
    #[serde(default)]
    other: Option<ArmFile>,
    #[serde(default)]
    chest: [f32; 3],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GestureFile {
    id: String,
    summary: String,
    kind: GestureKind,
    hands: Hands,
    strokes: [u8; 2],
    prep: KeyFile,
    stroke: KeyFile,
}

impl GestureFile {
    fn compile(self, hands: &HandLibrary) -> Result<Gesture, GestureError> {
        let id = self.id;
        if Slug::new(&id).is_err() {
            return Err(GestureError::BadName(id));
        }
        if self.summary.trim().is_empty() {
            return Err(GestureError::EmptySummary(id));
        }
        let implausible = |what: String| GestureError::Implausible { id: id.clone(), what };
        let [lo, hi] = self.strokes;
        if lo == 0 || lo > hi || hi > 8 {
            return Err(implausible(format!("strokes {lo} to {hi} must be 1 to 8, the first no more than the second")));
        }
        if self.hands == Hands::One && (self.prep.other.is_some() || self.stroke.other.is_some()) {
            return Err(implausible("a one-handed gesture has no `other` hand".to_owned()));
        }
        let shape = |name: &str| {
            hands
                .get(name)
                .map(|s| Hand::of(s, 1.0))
                .ok_or_else(|| GestureError::UnknownHandShape { id: id.clone(), shape: name.to_owned() })
        };
        let key = |file: &KeyFile, which: &str| -> Result<Key, GestureError> {
            let lead = file.lead.pose();
            let other = match (self.hands, &file.other) {
                (Hands::One, _) => None,
                (Hands::Both, Some(o)) => Some(o.pose()),
                (Hands::Both, None) => Some(lead),
            };
            for (arm, side) in [(Some(lead), "lead"), (other, "other")] {
                if arm.is_some_and(|a| !a.is_plausible()) {
                    return Err(implausible(format!("the {which} pose's {side} arm is out of an arm's range")));
                }
            }
            if file.chest.iter().any(|c| !c.is_finite() || c.abs() > 25.0) {
                return Err(implausible(format!("the {which} chest accent must be within 25 degrees")));
            }
            let lead_hand = shape(&file.lead.hand)?;
            let other_hand = match (&file.other, self.hands) {
                (Some(o), _) => shape(&o.hand)?,
                (None, Hands::Both) => lead_hand,
                (None, Hands::One) => Hand::NONE,
            };
            Ok(Key { lead, other, lead_hand, other_hand, chest: file.chest })
        };
        let prep = key(&self.prep, "prep")?;
        let stroke = key(&self.stroke, "stroke")?;
        Ok(Gesture { id, summary: self.summary, kind: self.kind, hands: self.hands, strokes: self.strokes, prep, stroke })
    }
}

/// The built-in files, in the order `gesture.list` shows them. Adding a gesture is adding a file and a line here.
const BUILTIN: [(&str, &str); 17] = [
    ("beat-chop", include_str!("../../../library/gestures/beat-chop.toml")),
    ("beat-press", include_str!("../../../library/gestures/beat-press.toml")),
    ("beat-flick", include_str!("../../../library/gestures/beat-flick.toml")),
    ("beat-pinch", include_str!("../../../library/gestures/beat-pinch.toml")),
    ("present", include_str!("../../../library/gestures/present.toml")),
    ("open-both", include_str!("../../../library/gestures/open-both.toml")),
    ("container", include_str!("../../../library/gestures/container.toml")),
    ("weigh", include_str!("../../../library/gestures/weigh.toml")),
    ("point-self", include_str!("../../../library/gestures/point-self.toml")),
    ("point-you", include_str!("../../../library/gestures/point-you.toml")),
    ("point-aside", include_str!("../../../library/gestures/point-aside.toml")),
    ("shrug", include_str!("../../../library/gestures/shrug.toml")),
    ("big", include_str!("../../../library/gestures/big.toml")),
    ("small", include_str!("../../../library/gestures/small.toml")),
    ("fidget-resettle", include_str!("../../../library/gestures/fidget-resettle.toml")),
    ("fidget-shoulders", include_str!("../../../library/gestures/fidget-shoulders.toml")),
    ("fidget-hand", include_str!("../../../library/gestures/fidget-hand.toml")),
];

static BUILTIN_LIBRARY: OnceLock<Result<GestureLibrary, GestureError>> = OnceLock::new();

/// A set of gestures with unique ids, in a stable order.
#[derive(Clone, Debug, PartialEq)]
pub struct GestureLibrary {
    gestures: Vec<Gesture>,
}

impl GestureLibrary {
    /// No gestures.
    pub const EMPTY: Self = Self { gestures: Vec::new() };

    /// Parses gesture files against `hands` and checks each.
    ///
    /// # Errors
    /// The first [`GestureError`].
    pub fn parse(sources: &[(&str, &str)], hands: &HandLibrary) -> Result<Self, GestureError> {
        let mut gestures: Vec<Gesture> = Vec::with_capacity(sources.len());
        for (label, text) in sources {
            let file: GestureFile = toml::from_str(text)
                .map_err(|e| GestureError::Parse { file: (*label).to_owned(), message: e.to_string() })?;
            let gesture = file.compile(hands)?;
            if gestures.iter().any(|g| g.id == gesture.id) {
                return Err(GestureError::DuplicateId(gesture.id));
            }
            gestures.push(gesture);
        }
        Ok(Self { gestures })
    }

    /// The gestures that ship with Omuzaki.
    ///
    /// # Errors
    /// Only if the embedded files are broken, which the crate's tests rule out.
    pub fn builtin() -> Result<&'static Self, GestureError> {
        BUILTIN_LIBRARY
            .get_or_init(|| {
                let hands = HandLibrary::builtin()
                    .map_err(|e| GestureError::Parse { file: "hand-shapes".to_owned(), message: e.to_string() })?;
                Self::parse(&BUILTIN, hands)
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// The gesture called `id`.
    pub fn get(&self, id: &str) -> Option<&Gesture> {
        self.gestures.iter().find(|g| g.id == id)
    }

    /// Every gesture, in file order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Gesture> {
        self.gestures.iter()
    }

    /// How many gestures.
    pub fn len(&self) -> usize {
        self.gestures.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.gestures.is_empty()
    }

    /// The gesture at `index` in file order.
    pub fn at(&self, index: usize) -> Option<&Gesture> {
        self.gestures.get(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_library_loads_with_every_kind() {
        let lib = GestureLibrary::builtin().expect("the built-in gestures load");
        assert_eq!(lib.len(), BUILTIN.len());
        for kind in [
            GestureKind::Beat,
            GestureKind::Deictic,
            GestureKind::Metaphoric,
            GestureKind::Iconic,
            GestureKind::Emblem,
            GestureKind::Fidget,
        ] {
            assert!(lib.iter().any(|g| g.kind == kind), "{kind:?}");
        }
        for (label, _) in BUILTIN {
            assert!(lib.get(label).is_some(), "{label} is listed under its own id");
        }
    }

    #[test]
    fn bad_files_are_refused_with_a_reason() {
        let hands = HandLibrary::builtin().expect("hands");
        let ok = |prep: &str, extra: &str| {
            format!(
                "id = \"x\"\nsummary = \"s\"\nkind = \"beat\"\nhands = \"one\"\nstrokes = [1, 2]\n{extra}\n[prep.lead]\n{prep}\nhand = \"open\"\n[stroke.lead]\nhand = \"flat\"\n"
            )
        };
        assert!(GestureLibrary::parse(&[("x", &ok("elbow = 40.0", ""))], hands).is_ok());
        let err = GestureLibrary::parse(&[("x", &ok("elbow = 400.0", ""))], hands).unwrap_err();
        assert!(matches!(err, GestureError::Implausible { .. }), "{err}");
        let err = GestureLibrary::parse(&[("x", &ok("elbo = 4.0", ""))], hands).unwrap_err();
        assert!(matches!(err, GestureError::Parse { .. }), "{err}");
        let unknown = ok("elbow = 4.0", "").replace("\"flat\"", "\"claw\"");
        assert!(matches!(
            GestureLibrary::parse(&[("x", &unknown)], hands).unwrap_err(),
            GestureError::UnknownHandShape { .. }
        ));
        let zero = ok("", "").replace("[1, 2]", "[0, 2]");
        assert!(matches!(GestureLibrary::parse(&[("x", &zero)], hands).unwrap_err(), GestureError::Implausible { .. }));
        let twice = ok("", "");
        assert_eq!(
            GestureLibrary::parse(&[("x", &twice), ("y", &twice)], hands).unwrap_err(),
            GestureError::DuplicateId("x".to_owned())
        );
    }
}
