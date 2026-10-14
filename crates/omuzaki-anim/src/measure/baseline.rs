//! What a human does, as data: the bands [`liveliness`](super::liveliness) is compared against.

use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Liveliness;

/// The built-in files, one per situation. Adding a baseline is adding a file and a line here.
const BUILTIN: [(&str, &str); 3] = [
    ("rest", include_str!("../../library/liveliness/rest.toml")),
    ("talking", include_str!("../../library/liveliness/talking.toml")),
    ("reading", include_str!("../../library/liveliness/reading.toml")),
];

/// Why a baseline could not be had.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum BaselineError {
    /// No built-in baseline has this name.
    #[error("no baseline named `{0}` (known: rest, talking, reading)")]
    Unknown(String),
    /// A built-in file does not parse: a bug in the library, caught by the tests.
    #[error("baseline `{name}` is malformed: {message}")]
    Malformed {
        /// The baseline's name.
        name: String,
        /// What the parser said.
        message: String,
    },
}

/// An inclusive range a metric is expected to fall in.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Band {
    /// Below this is too little.
    pub low: f32,
    /// Above this is too much.
    pub high: f32,
}

impl Band {
    /// Where `value` falls against this band. A NaN is never `Ok`.
    pub fn judge(self, value: f32) -> Verdict {
        if value.is_nan() || value < self.low {
            Verdict::Low
        } else if value > self.high {
            Verdict::High
        } else {
            Verdict::Ok
        }
    }
}

/// How a measured number compares with its band.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Under the band: the face looks dead on this metric.
    Low,
    /// Inside the band.
    Ok,
    /// Over the band: the face looks jittery on this metric.
    High,
}

/// One verdict per compared metric.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Comparison {
    /// Verdict on `change_per_second`.
    pub change_per_second: Verdict,
    /// Verdict on `blinks_per_minute`.
    pub blinks_per_minute: Verdict,
    /// Verdict on `saccades_per_second`.
    pub saccades_per_second: Verdict,
    /// Verdict on `longest_frozen_s`.
    pub longest_frozen_s: Verdict,
}

impl Comparison {
    /// Whether every metric is inside its band.
    pub fn all_ok(&self) -> bool {
        [self.change_per_second, self.blinks_per_minute, self.saccades_per_second, self.longest_frozen_s]
            .iter()
            .all(|v| *v == Verdict::Ok)
    }
}

/// A situation's expected numbers with where they come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// The situation: `rest`, `talking` or `reading`.
    pub name: String,
    /// One line on what the situation is.
    pub summary: String,
    /// Where each band comes from, and which are estimates.
    pub sources: Vec<String>,
    /// Mean change per second over face and head channels.
    pub change_per_second: Band,
    /// Blinks per minute.
    pub blinks_per_minute: Band,
    /// Saccades per second.
    pub saccades_per_second: Band,
    /// Longest frozen window, seconds.
    pub longest_frozen_s: Band,
}

impl Baseline {
    /// The built-in baseline `name`.
    ///
    /// # Errors
    /// [`BaselineError::Unknown`] for another name.
    pub fn builtin(name: &str) -> Result<&'static Self, BaselineError> {
        static ALL: OnceLock<Result<Vec<Baseline>, BaselineError>> = OnceLock::new();
        let all = ALL.get_or_init(|| {
            BUILTIN
                .iter()
                .map(|(name, text)| {
                    toml::from_str(text)
                        .map_err(|e| BaselineError::Malformed { name: (*name).to_owned(), message: e.to_string() })
                })
                .collect()
        });
        match all {
            Ok(list) => list.iter().find(|b| b.name == name).ok_or_else(|| BaselineError::Unknown(name.to_owned())),
            Err(e) => Err(e.clone()),
        }
    }

    /// Low, ok or high for each metric of `measured`.
    pub fn compare(&self, measured: &Liveliness) -> Comparison {
        Comparison {
            change_per_second: self.change_per_second.judge(measured.change_per_second),
            blinks_per_minute: self.blinks_per_minute.judge(measured.blinks_per_minute),
            saccades_per_second: self.saccades_per_second.judge(measured.saccades_per_second),
            longest_frozen_s: self.longest_frozen_s.judge(measured.longest_frozen_s),
        }
    }
}
