//! The decision layer's seam (AP-03, AP-04, AP-05; #2348): the model that *talks* is separate from the model that
//! *moves*.
//!
//! While the character speaks, a fast [`Decider`] reads the live transcript and answers a few typed [`Question`]s in
//! one [`Ask`]: which reaction, which face, how much energy, whether she is committing to an action right now. While
//! the person speaks, it answers which face a listening friend would make. Answers come back as probabilities, and
//! code keeps control: the performer core holds every provider to the same gates (only installed expressions and
//! clips can be chosen, a confidence floor that is higher for big actions, a spoken commitment before an action, a
//! refusal that vetoes one, cooldowns) and turns what passes into ordinary intents.
//!
//! Providers: a deterministic rule floor (always on, the default, answers whenever a model misses), a local model,
//! and hosted ones as bring-your-own-key options only. A provider never blocks the performer's thread: [`Decider::ask`]
//! returns a [`DecisionTicket`] that the loop polls.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most questions one [`Ask`] may carry.
pub const MAX_QUESTIONS: usize = 8;
/// The most options a [`QuestionKind::Choice`] may offer.
pub const MAX_OPTIONS: usize = 32;
/// The most levels a [`QuestionKind::Score`] may have.
pub const MAX_LEVELS: u8 = 10;
/// The longest state text an [`Ask`] may carry, in characters (about a minute of speech).
pub const MAX_STATE_CHARS: usize = 2_000;
/// How long the loop waits for a provider before the rule floor answers instead.
pub const DECISION_TIMEOUT: Duration = Duration::from_millis(400);

/// What kind of answer a question wants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum QuestionKind {
    /// One of `options` (the answer has one probability per option).
    #[non_exhaustive]
    Choice {
        /// The options, each a short name (an expression or clip id, or `none`).
        options: Vec<String>,
    },
    /// Yes or no (the answer is `[p(no), p(yes)]`).
    YesNo,
    /// An ordered score from 0 to `levels - 1` (one probability per level).
    #[non_exhaustive]
    Score {
        /// How many levels.
        levels: u8,
    },
}

impl QuestionKind {
    /// A choice among `options`.
    pub fn choice(options: Vec<String>) -> Self {
        Self::Choice { options }
    }

    /// A score with `levels` levels.
    pub fn score(levels: u8) -> Self {
        Self::Score { levels }
    }

    /// How many probabilities an answer to it has.
    pub fn arity(&self) -> usize {
        match self {
            Self::Choice { options } => options.len(),
            Self::YesNo => 2,
            Self::Score { levels } => usize::from(*levels),
        }
    }
}

/// One typed question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Question {
    /// Its id, unique in the ask: a slug the core matches answers by (`reaction`, `energy`, `commit`, `clip`).
    pub id: String,
    /// The question in plain words, as a model reads it.
    pub prompt: String,
    /// What kind of answer it wants.
    pub kind: QuestionKind,
}

impl Question {
    /// A question.
    pub fn new(id: impl Into<String>, prompt: impl Into<String>, kind: QuestionKind) -> Self {
        Self { id: id.into(), prompt: prompt.into(), kind }
    }
}

/// Who is talking when the question is asked: what the answers will be used for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Moment {
    /// The character is speaking `state`'s last line.
    Speaking,
    /// The person is speaking; the character listens.
    Listening,
}

/// One request: the state text and the questions about it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Ask {
    /// Who is talking.
    pub moment: Moment,
    /// The live transcript window, oldest first, one speaker per line (`person: …`, `<name>: …`), at most
    /// [`MAX_STATE_CHARS`].
    pub state: String,
    /// The questions, at most [`MAX_QUESTIONS`].
    pub questions: Vec<Question>,
}

impl Ask {
    /// An ask about `state`.
    pub fn new(moment: Moment, state: impl Into<String>, questions: Vec<Question>) -> Self {
        Self { moment, state: state.into(), questions }
    }

    /// Whether it is within the limits: questions, options, levels, ids unique, state length.
    ///
    /// # Errors
    /// [`DecideError::Invalid`] naming the first problem.
    pub fn check(&self) -> Result<(), DecideError> {
        if self.questions.is_empty() || self.questions.len() > MAX_QUESTIONS {
            return Err(DecideError::invalid(format!("an ask has 1 to {MAX_QUESTIONS} questions")));
        }
        if self.state.chars().count() > MAX_STATE_CHARS {
            return Err(DecideError::invalid(format!("the state is over {MAX_STATE_CHARS} characters")));
        }
        for (i, q) in self.questions.iter().enumerate() {
            if self.questions[..i].iter().any(|p| p.id == q.id) {
                return Err(DecideError::invalid(format!("question id `{}` is used twice", q.id)));
            }
            match &q.kind {
                QuestionKind::Choice { options } if options.is_empty() || options.len() > MAX_OPTIONS => {
                    return Err(DecideError::invalid(format!("`{}` needs 1 to {MAX_OPTIONS} options", q.id)));
                }
                QuestionKind::Score { levels } if *levels < 2 || *levels > MAX_LEVELS => {
                    return Err(DecideError::invalid(format!("`{}` needs 2 to {MAX_LEVELS} levels", q.id)));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The question with `id`.
    pub fn question(&self, id: &str) -> Option<&Question> {
        self.questions.iter().find(|q| q.id == id)
    }
}

/// One question's answer: a probability per option, level, or `[no, yes]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Answer {
    /// The question's id.
    pub id: String,
    /// The probabilities, in the question's order; they sum to about 1.
    pub probs: Vec<f32>,
}

impl Answer {
    /// An answer of `probs`, normalised to sum to 1 (all zero becomes uniform; negatives and NaN count as 0).
    pub fn new(id: impl Into<String>, probs: Vec<f32>) -> Self {
        let mut probs: Vec<f32> = probs.into_iter().map(|p| if p.is_finite() && p > 0.0 { p } else { 0.0 }).collect();
        let sum: f32 = probs.iter().sum();
        if sum > 0.0 {
            for p in &mut probs {
                *p /= sum;
            }
        } else if !probs.is_empty() {
            // Lengths here are at most `MAX_OPTIONS`.
            #[allow(clippy::cast_precision_loss)]
            let even = 1.0 / probs.len() as f32;
            probs.fill(even);
        }
        Self { id: id.into(), probs }
    }

    /// An answer that puts `confidence` on index `pick` of `arity` and spreads the rest evenly.
    pub fn pick(id: impl Into<String>, arity: usize, pick: usize, confidence: f32) -> Self {
        let confidence = confidence.clamp(0.0, 1.0);
        // `arity` is at most `MAX_OPTIONS`.
        #[allow(clippy::cast_precision_loss)]
        let rest = if arity > 1 { (1.0 - confidence) / (arity - 1) as f32 } else { 0.0 };
        let probs = (0..arity).map(|i| if i == pick { confidence } else { rest }).collect();
        Self::new(id, probs)
    }

    /// The most likely index and its probability.
    pub fn best(&self) -> Option<(usize, f32)> {
        self.probs.iter().copied().enumerate().max_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// For a yes/no answer, `p(yes)`.
    pub fn yes(&self) -> f32 {
        self.probs.get(1).copied().unwrap_or(0.0)
    }

    /// For a score, its expected level.
    pub fn expected(&self) -> f32 {
        // Indices are at most `MAX_LEVELS`.
        #[allow(clippy::cast_precision_loss)]
        self.probs.iter().enumerate().map(|(i, p)| i as f32 * p).sum()
    }
}

/// A provider's answers to one ask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Decision {
    /// One answer per question it answered; a question it could not answer is missing, and the rule floor fills it.
    pub answers: Vec<Answer>,
    /// The provider that answered (`rules`, `local`, a hosted provider's kind).
    pub provider: String,
}

impl Decision {
    /// `answers` from `provider`.
    pub fn new(provider: impl Into<String>, answers: Vec<Answer>) -> Self {
        Self { answers, provider: provider.into() }
    }

    /// The answer to `id`.
    pub fn answer(&self, id: &str) -> Option<&Answer> {
        self.answers.iter().find(|a| a.id == id)
    }

    /// Keeps only answers that fit `ask`: a known id, the right number of probabilities, once each.
    #[must_use]
    pub fn fitted(mut self, ask: &Ask) -> Self {
        let mut seen = Vec::new();
        self.answers.retain(|a| {
            let fits = ask.question(&a.id).is_some_and(|q| q.kind.arity() == a.probs.len()) && !seen.contains(&a.id);
            if fits {
                seen.push(a.id.clone());
            }
            fits
        });
        self
    }
}

/// Why a provider gave no decision.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DecideError {
    /// The ask is outside the limits.
    #[error("invalid ask: {0}")]
    Invalid(String),
    /// The provider is not configured (no key, no model).
    #[error("the decision provider is not configured: {0}")]
    NotConfigured(String),
    /// It took longer than the loop waits.
    #[error("the decision provider timed out")]
    Timeout,
    /// It said no or failed (a rate limit, a network error, a reply that doesn't parse).
    #[error("the decision provider failed: {0}")]
    Provider(String),
}

impl DecideError {
    /// [`DecideError::Invalid`].
    pub fn invalid(why: impl Into<String>) -> Self {
        Self::Invalid(why.into())
    }

    /// [`DecideError::Provider`].
    pub fn provider(why: impl Into<String>) -> Self {
        Self::Provider(why.into())
    }
}

/// Answers asks. Never blocks the caller: a slow provider answers on its own thread through the ticket.
pub trait Decider: Send {
    /// Its kind, as [`Decision::provider`] names it.
    fn kind(&self) -> &str;

    /// Starts answering `ask` (already checked).
    fn ask(&mut self, ask: Ask) -> DecisionTicket;
}

type Slot = Arc<Mutex<Option<Result<Decision, DecideError>>>>;

/// A decision on its way. Clone the [`TicketWriter`] side into the provider's thread; the loop polls this side.
#[derive(Clone)]
pub struct DecisionTicket(Slot);

/// The provider's side of a [`DecisionTicket`]: fills it once.
#[derive(Clone)]
pub struct TicketWriter(Slot);

impl DecisionTicket {
    /// A ticket that is already answered.
    pub fn ready(result: Result<Decision, DecideError>) -> Self {
        Self(Arc::new(Mutex::new(Some(result))))
    }

    /// An empty ticket and the writer that fills it.
    pub fn pending() -> (Self, TicketWriter) {
        let slot: Slot = Arc::new(Mutex::new(None));
        (Self(Arc::clone(&slot)), TicketWriter(slot))
    }

    /// The result, once it is there; takes it, so a second poll returns `None`.
    pub fn poll(&self) -> Option<Result<Decision, DecideError>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

impl TicketWriter {
    /// Fills the ticket. A second fill replaces an unread first one; a ticket the loop gave up on is simply dropped.
    pub fn fill(&self, result: Result<Decision, DecideError>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
    }
}

impl fmt::Debug for DecisionTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecisionTicket").finish_non_exhaustive()
    }
}

impl fmt::Debug for TicketWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TicketWriter").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask() -> Ask {
        Ask::new(
            Moment::Speaking,
            "person: hi\nmochi: Hi chat!",
            vec![
                Question::new(
                    "reaction",
                    "Which face fits the line?",
                    QuestionKind::choice(vec!["happy".into(), "none".into()]),
                ),
                Question::new("commit", "Is she committing to an action now?", QuestionKind::YesNo),
                Question::new("energy", "How much energy?", QuestionKind::score(5)),
            ],
        )
    }

    #[test]
    fn asks_are_checked() {
        assert!(ask().check().is_ok());
        let mut twice = ask();
        twice.questions.push(Question::new("commit", "again", QuestionKind::YesNo));
        assert!(matches!(twice.check(), Err(DecideError::Invalid(_))));
        let empty = Ask::new(Moment::Listening, "", vec![]);
        assert!(empty.check().is_err());
        let one_level = Ask::new(Moment::Listening, "", vec![Question::new("e", "", QuestionKind::score(1))]);
        assert!(one_level.check().is_err());
        let long = Ask::new(Moment::Listening, "x".repeat(MAX_STATE_CHARS + 1), ask().questions);
        assert!(long.check().is_err());
    }

    #[test]
    fn answers_normalise_and_read() {
        let a = Answer::new("reaction", vec![3.0, 1.0]);
        assert_eq!(a.probs, vec![0.75, 0.25]);
        assert_eq!(a.best(), Some((0, 0.75)));
        let zero = Answer::new("x", vec![0.0, f32::NAN, -1.0]);
        assert!(zero.probs.iter().all(|p| (p - 1.0 / 3.0).abs() < 1e-6));
        let pick = Answer::pick("commit", 2, 1, 0.9);
        assert!((pick.yes() - 0.9).abs() < 1e-6);
        let score = Answer::new("energy", vec![0.0, 0.0, 1.0, 0.0, 0.0]);
        assert!((score.expected() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn decisions_keep_only_answers_that_fit() {
        let d = Decision::new(
            "test",
            vec![
                Answer::new("reaction", vec![1.0, 0.0]),
                Answer::new("reaction", vec![0.0, 1.0]),
                Answer::new("commit", vec![1.0, 0.0, 0.0]),
                Answer::new("unknown", vec![1.0]),
            ],
        )
        .fitted(&ask());
        assert_eq!(d.answers.len(), 1);
        assert_eq!(d.answer("reaction").and_then(Answer::best), Some((0, 1.0)));
    }

    #[test]
    fn tickets_are_filled_once_and_read_once() {
        let (ticket, writer) = DecisionTicket::pending();
        assert!(ticket.poll().is_none());
        writer.fill(Err(DecideError::Timeout));
        assert_eq!(ticket.poll(), Some(Err(DecideError::Timeout)));
        assert!(ticket.poll().is_none());
        let ready = DecisionTicket::ready(Ok(Decision::new("rules", vec![])));
        assert!(matches!(ready.poll(), Some(Ok(_))));
    }
}
