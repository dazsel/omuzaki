//! The decision layer's measure (#2348): how fast a provider answers (p50 and p90) and how often its answers agree
//! with a labelled set of transcript moments, through the same gates and rule floor the show uses.
//!
//! [`MOMENTS`] is the shipped set (`data/moments.toml`) and [`HOLDOUT`] a second set never used to tune the rule
//! floor. [`run`] asks a [`Decider`] about each moment, waits up to a timeout as the show does, fills what it missed
//! from the [`RuleFloor`], and scores what would have reached the character: the face it makes and the gesture it does.

use std::time::{Duration, Instant};

use omuzaki_anim_api::score::{ClipId, ExpressionId};
use omuzaki_performer_api::decide::{Ask, DECISION_TIMEOUT, Decider, Moment};
use omuzaki_performer_api::intent::Intent;
use omuzaki_performer_api::stage::{ClipInfo, Inventory};
use serde::{Deserialize, Serialize};

use crate::decide::{Gates, NONE, RuleFloor, questions_listening, questions_speaking, with_floor};

/// The shipped labelled set.
pub const MOMENTS: &str = include_str!("../data/moments.toml");
/// Held-out moments, never used to tune the rule floor.
pub const HOLDOUT: &str = include_str!("../data/holdout.toml");
/// The expressions the set's character has.
pub const EXPRESSIONS: [&str; 6] = ["happy", "sad", "surprised", "angry", "smug", "thinking"];
/// Its gestures.
pub const GESTURES: [&str; 4] = ["wave", "dance", "backflip", "bow"];
/// How often [`run`] looks at a pending ticket.
const POLL: Duration = Duration::from_micros(250);

/// One labelled moment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct LabelledMoment {
    /// Who is talking.
    pub moment: Moment,
    /// The transcript window, one `speaker: line` per line.
    pub state: String,
    /// The faces that fit; `none` for no reaction.
    pub face: Vec<String>,
    /// The gesture she does now (`speaking` only), or `none`.
    #[serde(default = "none")]
    pub gesture: String,
    /// Why the label is what it is, where that isn't obvious.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn none() -> String {
    NONE.to_owned()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    moment: Vec<LabelledMoment>,
}

/// Reads a set in the format of [`MOMENTS`].
///
/// # Errors
/// The TOML error.
pub fn parse(text: &str) -> Result<Vec<LabelledMoment>, toml::de::Error> {
    toml::from_str::<Set>(text).map(|s| s.moment)
}

/// The inventory of the set's character.
pub fn inventory() -> Inventory {
    Inventory::new(
        EXPRESSIONS.iter().filter_map(|e| ExpressionId::new(*e).ok()).collect(),
        GESTURES.iter().filter_map(|g| ClipId::new(*g).ok()).map(|id| ClipInfo::new(id, true, 1.5)).collect(),
        vec![],
        vec![],
    )
}

/// Counts of agreement on one thing.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize)]
pub struct Agreement {
    /// Moments scored.
    pub moments: u32,
    /// Moments where the outcome matched the label.
    pub agreed: u32,
}

impl Agreement {
    fn add(&mut self, agreed: bool) {
        self.moments += 1;
        self.agreed += u32::from(agreed);
    }

    /// `agreed / moments`, 0 when nothing was scored.
    pub fn rate(&self) -> f32 {
        if self.moments == 0 {
            0.0
        } else {
            // Counts are small.
            #[allow(clippy::cast_precision_loss)]
            let rate = self.agreed as f32 / self.moments as f32;
            rate
        }
    }
}

/// What one run measured.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Report {
    /// The provider's kind.
    pub provider: String,
    /// Moments asked.
    pub moments: u32,
    /// Moments the provider answered in time.
    pub answered: u32,
    /// Moments it answered too late (the floor answered).
    pub timeouts: u32,
    /// Moments it failed (the floor answered), with the first few reasons.
    pub errors: u32,
    /// The first few failure reasons.
    pub error_samples: Vec<String>,
    /// Latency of the answers it gave in time, in milliseconds.
    pub p50_ms: Option<f64>,
    /// The same, 90th percentile.
    pub p90_ms: Option<f64>,
    /// The slowest answer in time.
    pub max_ms: Option<f64>,
    /// The face the character makes (or doesn't) matches a label, over every moment.
    pub face: Agreement,
    /// The gesture she does (or doesn't) matches the label, over the speaking moments.
    pub gesture: Agreement,
    /// Gestures fired that the label says shouldn't have (the costly mistake).
    pub false_gestures: u32,
    /// Both face and gesture agree.
    pub overall: Agreement,
    /// Per moment, what the character did: `face` and `gesture` (`none` for nothing).
    pub outcomes: Vec<(String, String)>,
}

/// The `p`th percentile of sorted `values` (nearest rank).
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    // Ranks are small; the product is in range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted.get(rank - 1).copied()
}

/// Asks `decider` about each of `moments`, waiting at most `timeout` (the show waits [`DECISION_TIMEOUT`]), and scores
/// what the gates let through against the labels.
pub fn run(decider: &mut dyn Decider, moments: &[LabelledMoment], timeout: Duration) -> Report {
    let inventory = inventory();
    let gates = Gates::default();
    let mut report = Report { provider: decider.kind().to_owned(), ..Report::default() };
    let mut latencies = Vec::new();
    for m in moments {
        let questions = match m.moment {
            Moment::Listening => questions_listening(&inventory),
            _ => questions_speaking(&inventory),
        };
        let ask = Ask::new(m.moment, m.state.clone(), questions);
        let floor = RuleFloor.answer(&ask);
        let started = Instant::now();
        let ticket = decider.ask(ask.clone());
        let decision = loop {
            if let Some(result) = ticket.poll() {
                match result {
                    Ok(d) => {
                        report.answered += 1;
                        latencies.push(started.elapsed().as_secs_f64() * 1000.0);
                        break with_floor(&ask, d, &floor);
                    }
                    Err(e) => {
                        report.errors += 1;
                        if report.error_samples.len() < 3 {
                            report.error_samples.push(e.to_string());
                        }
                        break floor;
                    }
                }
            }
            if started.elapsed() >= timeout {
                report.timeouts += 1;
                break floor;
            }
            std::thread::sleep(POLL);
        };
        report.moments += 1;
        let line = ask.state.lines().last().map_or("", |l| l.split_once(": ").map_or(l, |(_, t)| t));
        let intents = gates.intents(&ask, &decision, line);
        let face = intents
            .iter()
            .find_map(|i| if let Intent::Emote { expression, .. } = i { Some(expression.to_string()) } else { None })
            .unwrap_or_else(none);
        let gesture = intents
            .iter()
            .find_map(|i| if let Intent::Gesture { clip, .. } = i { Some(clip.to_string()) } else { None })
            .unwrap_or_else(none);
        let face_ok = m.face.contains(&face);
        report.face.add(face_ok);
        let mut gesture_ok = true;
        if m.moment == Moment::Speaking {
            gesture_ok = gesture == m.gesture;
            report.gesture.add(gesture_ok);
            if gesture != NONE && gesture != m.gesture {
                report.false_gestures += 1;
            }
        }
        report.overall.add(face_ok && gesture_ok);
        report.outcomes.push((face, gesture));
    }
    latencies.sort_by(f64::total_cmp);
    report.p50_ms = percentile(&latencies, 50.0);
    report.p90_ms = percentile(&latencies, 90.0);
    report.max_ms = latencies.last().copied();
    report
}

/// [`run`] with the show's own timeout.
pub fn run_as_the_show(decider: &mut dyn Decider, moments: &[LabelledMoment]) -> Report {
    run(decider, moments, DECISION_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_take_the_nearest_rank() {
        let v: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), Some(5.0));
        assert_eq!(percentile(&v, 90.0), Some(9.0));
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(percentile(&[3.0], 90.0), Some(3.0));
    }

    #[test]
    fn the_shipped_set_parses_and_names_only_what_the_character_has() {
        let set = parse(MOMENTS).unwrap();
        assert!(set.len() >= 40);
        let holdout = parse(HOLDOUT).unwrap();
        assert!(holdout.len() >= 20);
        for m in set.iter().chain(&holdout) {
            assert!(m.face.iter().all(|f| f == NONE || EXPRESSIONS.contains(&f.as_str())), "{m:?}");
            assert!(m.gesture == NONE || GESTURES.contains(&m.gesture.as_str()), "{m:?}");
            assert!(m.moment == Moment::Speaking || m.gesture == NONE, "{m:?}");
            assert!(m.state.chars().count() < omuzaki_performer_api::decide::MAX_STATE_CHARS);
        }
    }
}
