//! What a refusal and a finding look like: a path into the scene and the rule it breaks.

use omuzaki_core::text::nearest;

/// A refusal: the scene (or a value in it) breaks a rule, so the edit must not be saved. Names the path and the rule
/// (D13). `path` is dotted from the scene node, e.g. `lighting.lights.key.azimuth`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {rule}")]
pub struct SceneError {
    /// Where, dotted from the scene node.
    pub path: String,
    /// The rule that is broken, in words.
    pub rule: String,
}

impl SceneError {
    /// An error at `path` for `rule`.
    pub fn new(path: impl Into<String>, rule: impl Into<String>) -> Self {
        Self { path: path.into(), rule: rule.into() }
    }
}

/// What a [`Finding`] is about. Findings never stop an edit; they tell a person or an agent what looks wrong.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// A cast member names a character the project doesn't have; the stage skips the member.
    MissingCharacter,
    /// The lighting has no light other than the ambient, so nothing casts a shape.
    NoLights,
    /// The backdrop is the environment and the lighting has none, so the lighting's `background` colour shows.
    NoEnvironment,
    /// A prop names a kind this build doesn't have; the stage skips the prop and the scene keeps it.
    UnknownPropKind,
}

/// Something that looks wrong but is allowed (D13).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Finding {
    /// What kind of thing.
    pub kind: FindingKind,
    /// Where, dotted from the scene node.
    pub path: String,
    /// In words.
    pub message: String,
}

/// The findings of one check, in a stable order (by path).
pub type Findings = Vec<Finding>;

/// "`word`, did you mean `near`?" when `word` is close to a candidate (a typo), else the candidates.
pub(crate) fn did_you_mean(word: &str, candidates: &[&str]) -> String {
    let close = word.chars().count() / 3 + 1;
    match nearest(word, candidates.iter().copied()) {
        Some(near) if omuzaki_core::text::edit_distance(word, near) <= close => {
            format!("unknown field `{word}`, did you mean `{near}`?")
        }
        _ => format!(
            "unknown field `{word}`, expected one of {}",
            candidates.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ")
        ),
    }
}
