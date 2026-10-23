//! A hosted decision provider (#2348): the System One API of Jev (by `TypeSafe`), as a bring-your-own-key option and never
//! the default.
//!
//! One `POST /v1/systemone` carries the state and the ask's questions, keyed by their ids: a
//! [`Choice`](QuestionKind::Choice) becomes a `choice` question with one criterion per option, a
//! [`YesNo`](QuestionKind::YesNo) a `noul`, a [`Score`](QuestionKind::Score) a `score` with one rubric line per level.
//! The answers come back as probabilities (`probabilities` for a choice or a score, `noul` for yes or no), which map
//! straight onto [`Answer`]s; the gates and the rule floor then treat them like any other provider's.
//!
//! The request leaves the machine through a [`JsonTransport`] the App builds (`omuzaki-llm`'s JSON client over the
//! person's key), so this crate never touches the network or a key, and tests answer from a fake.

use std::sync::Arc;

use omuzaki_performer_api::decide::{Answer, Ask, DecideError, Decider, Decision, DecisionTicket, QuestionKind};
use serde_json::{Map, Value, json};

use crate::decide::NONE;

/// The provider kind, as decisions and the `performer.decider` setting name it.
pub const KIND: &str = "systemone";
/// The path of the one request.
pub const PATH: &str = "/v1/systemone";
/// Jev's hosted endpoint.
pub const DEFAULT_ENDPOINT: &str = "https://api.typesafe.ai";
/// The model the System One examples ask for.
pub const DEFAULT_MODEL: &str = "jev-latest";
/// The confidence a bare `choice` or `score` answer gets when the reply carries no probabilities.
const BARE_CONFIDENCE: f32 = 0.75;

/// Sends one JSON request and returns the JSON answer. The App's implementation holds the endpoint and the key.
pub trait JsonTransport: Send + Sync {
    /// Posts `body` to [`PATH`].
    ///
    /// # Errors
    /// [`DecideError::NotConfigured`] without a key, [`DecideError::Provider`] for anything the server or the
    /// network said.
    fn post(&self, body: &Value) -> Result<Value, DecideError>;
}

/// The System One provider over a [`JsonTransport`].
pub struct SystemOneDecider {
    transport: Arc<dyn JsonTransport>,
    model: String,
}

impl SystemOneDecider {
    /// A decider that asks `model` (`jev-latest`) through `transport`.
    pub fn new(transport: Arc<dyn JsonTransport>, model: impl Into<String>) -> Self {
        Self { transport, model: model.into() }
    }

    /// The request body for `ask`.
    pub fn request(ask: &Ask, model: &str) -> Value {
        let mut questions = Map::new();
        for q in &ask.questions {
            let question = match &q.kind {
                QuestionKind::Choice { options, .. } => {
                    let criteria: Map<String, Value> =
                        options.iter().map(|o| (o.clone(), Value::String(describe(&q.id, o)))).collect();
                    json!({ "type": "choice", "instructions": q.prompt, "criteria": criteria })
                }
                QuestionKind::YesNo => json!({ "type": "noul", "instructions": q.prompt }),
                QuestionKind::Score { levels, .. } => {
                    let top = levels.saturating_sub(1);
                    let rubric: Vec<String> = (0..*levels)
                        .map(|l| match l {
                            0 => "0: the lowest".to_owned(),
                            l if l == top => format!("{l}: the highest"),
                            l => l.to_string(),
                        })
                        .collect();
                    json!({ "type": "score", "instructions": q.prompt, "criteria": rubric })
                }
                _ => continue,
            };
            questions.insert(q.id.clone(), question);
        }
        json!({ "model": model, "state": ask.state, "questions": questions })
    }

    /// Reads an answer body: each question it answered, as probabilities in the question's own order. A question it
    /// skipped, or answered in a shape this doesn't know, is left for the rule floor.
    ///
    /// # Errors
    /// [`DecideError::Provider`] when the body has no `answers` object.
    pub fn parse(ask: &Ask, body: &Value) -> Result<Decision, DecideError> {
        let answers = body
            .get("answers")
            .and_then(Value::as_object)
            .ok_or_else(|| DecideError::provider("the System One answer has no `answers`"))?;
        let mut out = Vec::new();
        for q in &ask.questions {
            let Some(a) = answers.get(&q.id) else { continue };
            let answer = match &q.kind {
                QuestionKind::Choice { options, .. } => {
                    let labels: Vec<&str> = options.iter().map(String::as_str).collect();
                    probabilities(a, &labels).or_else(|| {
                        let chosen = a.get("choice")?.as_str()?;
                        let i = options.iter().position(|o| o == chosen)?;
                        Some(bare(a, options.len(), i))
                    })
                }
                QuestionKind::YesNo => a.get("noul").and_then(Value::as_f64).map(|p| {
                    // A probability, so the narrowing loses nothing that matters.
                    #[allow(clippy::cast_possible_truncation)]
                    let p = p.clamp(0.0, 1.0) as f32;
                    vec![1.0 - p, p]
                }),
                QuestionKind::Score { levels, .. } => {
                    let names: Vec<String> = (0..*levels).map(|l| l.to_string()).collect();
                    let labels: Vec<&str> = names.iter().map(String::as_str).collect();
                    probabilities(a, &labels).or_else(|| {
                        let score = a.get("score")?.as_f64()?;
                        // A level index from 0 to at most `MAX_LEVELS`.
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let i = score.round().clamp(0.0, f64::from(levels.saturating_sub(1))) as usize;
                        Some(bare(a, usize::from(*levels), i))
                    })
                }
                _ => None,
            };
            if let Some(probs) = answer {
                out.push(Answer::new(&q.id, probs));
            }
        }
        Ok(Decision::new(KIND, out))
    }
}

/// What an option means, for the model: `none` in words, other names with spaces.
fn describe(question: &str, option: &str) -> String {
    let name = option.replace(['_', '-'], " ");
    match (question, option) {
        (_, NONE) => "None of the other options fits.".to_owned(),
        (crate::decide::CLIP, _) => format!("The {name} gesture."),
        _ => format!("The {name} expression."),
    }
}

/// The `probabilities` object of `answer`, in `labels`' order, if it names at least one of them.
fn probabilities(answer: &Value, labels: &[&str]) -> Option<Vec<f32>> {
    let map = answer.get("probabilities")?.as_object()?;
    let mut any = false;
    let probs = labels
        .iter()
        .map(|l| {
            let p = map.get(*l).and_then(Value::as_f64);
            any |= p.is_some();
            // A probability.
            #[allow(clippy::cast_possible_truncation)]
            let p = p.unwrap_or(0.0) as f32;
            p
        })
        .collect();
    any.then_some(probs)
}

/// One pick with the answer's `confidence` (or a default) and the rest spread evenly.
fn bare(answer: &Value, arity: usize, pick: usize) -> Vec<f32> {
    // A probability.
    #[allow(clippy::cast_possible_truncation)]
    let confidence = answer.get("confidence").and_then(Value::as_f64).map_or(BARE_CONFIDENCE, |c| c as f32);
    Answer::pick("", arity, pick, confidence).probs
}

impl std::fmt::Debug for SystemOneDecider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemOneDecider").field("model", &self.model).finish_non_exhaustive()
    }
}

impl Decider for SystemOneDecider {
    fn kind(&self) -> &str {
        KIND
    }

    fn ask(&mut self, ask: Ask) -> DecisionTicket {
        let (ticket, writer) = DecisionTicket::pending();
        let transport = Arc::clone(&self.transport);
        let body = Self::request(&ask, &self.model);
        let spawned = std::thread::Builder::new().name("performer-decide".into()).spawn(move || {
            writer.fill(transport.post(&body).and_then(|answer| Self::parse(&ask, &answer)));
        });
        if let Err(e) = spawned {
            return DecisionTicket::ready(Err(DecideError::provider(e.to_string())));
        }
        ticket
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use omuzaki_anim_api::score::{ClipId, ExpressionId};
    use omuzaki_performer_api::decide::Moment;
    use omuzaki_performer_api::stage::{ClipInfo, Inventory};

    use super::*;
    use crate::decide::{CLIP, COMMIT, ENERGY, REACTION, questions_speaking};

    fn ask() -> Ask {
        let inventory = Inventory::new(
            ["happy", "sad"].iter().map(|e| ExpressionId::new(*e).unwrap()).collect(),
            vec![ClipInfo::new(ClipId::new("wave").unwrap(), true, 1.2)],
            vec![],
            vec![],
        );
        Ask::new(Moment::Speaking, "person: hi\nmochi: Hi chat! Watch this, I'll wave!", questions_speaking(&inventory))
    }

    struct Fake {
        seen: Mutex<Vec<Value>>,
        answer: Result<Value, DecideError>,
    }

    impl JsonTransport for Fake {
        fn post(&self, body: &Value) -> Result<Value, DecideError> {
            self.seen.lock().unwrap().push(body.clone());
            self.answer.clone()
        }
    }

    #[test]
    fn an_ask_becomes_typed_system_one_questions() {
        let body = SystemOneDecider::request(&ask(), DEFAULT_MODEL);
        assert_eq!(body["model"], "jev-latest");
        assert!(body["state"].as_str().unwrap().ends_with("I'll wave!"));
        let reaction = &body["questions"][REACTION];
        assert_eq!(reaction["type"], "choice");
        assert_eq!(reaction["criteria"]["happy"], "The happy expression.");
        assert_eq!(reaction["criteria"]["none"], "None of the other options fits.");
        assert_eq!(body["questions"][COMMIT]["type"], "noul");
        assert!(body["questions"][COMMIT].get("criteria").is_none());
        assert_eq!(body["questions"][ENERGY]["type"], "score");
        assert_eq!(body["questions"][ENERGY]["criteria"].as_array().unwrap().len(), 5);
        assert_eq!(body["questions"][CLIP]["criteria"]["wave"], "The wave gesture.");
    }

    #[test]
    fn probabilities_map_onto_the_options_and_odd_answers_are_left_to_the_floor() {
        let answer = json!({
            "model": "jev-1.13.0",
            "answers": {
                "reaction": { "type": "choice", "choice": "happy", "confidence": 0.9,
                              "probabilities": { "happy": 0.9, "sad": 0.04, "none": 0.06 } },
                "commit": { "type": "noul", "noul": 0.93 },
                "energy": { "type": "score", "score": 3.1, "confidence": 0.6,
                            "probabilities": { "2": 0.2, "3": 0.6, "4": 0.2 } },
                "clip": { "type": "choice", "choice": "wave", "confidence": 0.8 },
                "unasked": { "type": "noul", "noul": 1.0 }
            },
            "usage": { "input_tokens": 120, "output_tokens": 0 }
        });
        let d = SystemOneDecider::parse(&ask(), &answer).unwrap();
        assert_eq!(d.provider, KIND);
        assert_eq!(d.answer(REACTION).and_then(Answer::best).map(|b| b.0), Some(0));
        assert!((d.answer(COMMIT).unwrap().yes() - 0.93).abs() < 1e-6);
        assert!((d.answer(ENERGY).unwrap().expected() - 3.0).abs() < 1e-6);
        // A bare choice takes its confidence.
        let clip = d.answer(CLIP).unwrap();
        assert_eq!(clip.best().map(|b| b.0), Some(0));
        assert!((clip.probs[0] - 0.8).abs() < 1e-6);
        assert!(d.answer("unasked").is_none());

        let odd = json!({ "answers": { "reaction": { "type": "choice", "choice": "backflip" },
                                       "commit": { "type": "noul", "noul": "maybe" } } });
        assert_eq!(SystemOneDecider::parse(&ask(), &odd).unwrap().answers, Vec::<Answer>::new());
        assert!(SystemOneDecider::parse(&ask(), &json!({ "error": "nope" })).is_err());
    }

    #[test]
    fn the_decider_asks_off_the_loop_thread_and_reports_failures() {
        let answer = json!({ "answers": { "commit": { "type": "noul", "noul": 0.2 } } });
        let fake = Arc::new(Fake { seen: Mutex::new(Vec::new()), answer: Ok(answer) });
        let mut decider = SystemOneDecider::new(Arc::clone(&fake) as Arc<dyn JsonTransport>, DEFAULT_MODEL);
        assert_eq!(decider.kind(), KIND);
        let ticket = decider.ask(ask());
        let decision = loop {
            if let Some(result) = ticket.poll() {
                break result.unwrap();
            }
            std::thread::yield_now();
        };
        assert!(decision.answer(COMMIT).unwrap().yes() < 0.5);
        assert_eq!(fake.seen.lock().unwrap().len(), 1);

        let failing = Arc::new(Fake { seen: Mutex::new(Vec::new()), answer: Err(DecideError::provider("429")) });
        let ticket = SystemOneDecider::new(failing, DEFAULT_MODEL).ask(ask());
        let result = loop {
            if let Some(result) = ticket.poll() {
                break result;
            }
            std::thread::yield_now();
        };
        assert!(matches!(result, Err(DecideError::Provider(_))));
    }
}
