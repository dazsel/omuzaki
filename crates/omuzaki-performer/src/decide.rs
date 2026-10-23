//! The decision layer (#2348): what the character's face and body do, decided beside the words.
//!
//! - [`questions_speaking`] and [`questions_listening`] build the typed questions from the member's inventory, so a
//!   provider can only ever choose an installed expression or clip.
//! - [`RuleFloor`] answers deterministically from the words, always, in microseconds: the default provider, and the
//!   answer for any question a model misses (timeout, rate limit, no key, a reply that doesn't parse).
//! - [`Gates`] turn answers into intents under the same rules for every provider: a confidence floor (higher for
//!   actions), a spoken commitment before an action, a refusal that vetoes it.
//! - [`LlmDecider`] asks a local model (any [`LlmClient`], typically an OpenAI-compatible server on loopback) on its
//!   own thread. Hosted providers are bring-your-own-key options only; the rule floor needs nothing.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use omuzaki_anim_api::score::{ClipId, ExpressionId};
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::message::{ContentBlock, Message, Role, SystemBlock, TurnRequest};
use omuzaki_llm_api::stream::Cancel;
use omuzaki_performer_api::decide::{
    Answer, Ask, DecideError, Decider, Decision, DecisionTicket, Moment, Question, QuestionKind,
};
use omuzaki_performer_api::intent::Intent;
use omuzaki_performer_api::stage::Inventory;

/// The "no reaction" option every choice ends with.
pub const NONE: &str = "none";
/// Question ids.
pub const REACTION: &str = "reaction";
/// How much energy the line carries, 0 to 4.
pub const ENERGY: &str = "energy";
/// Whether the character is committing to an action right now.
pub const COMMIT: &str = "commit";
/// Which clip, if she is.
pub const CLIP: &str = "clip";
/// The face a listening friend makes.
pub const LISTENING: &str = "listening";
/// Energy levels.
pub const ENERGY_LEVELS: u8 = 5;

/// Moods the rule floor knows, each with the expression names that show it and the words that call for it.
const MOODS: [(&[&str], &[&str]); 6] = [
    (
        &["happy", "joy", "smile", "fun", "glad", "excited"],
        &[
            "haha",
            "hehe",
            "yay",
            "great",
            "love",
            "awesome",
            "nice",
            "thanks",
            "thank",
            "glad",
            "fun",
            "cool",
            "hi",
            "hello",
            "welcome",
            "amazing",
            "wonderful",
            "yes",
            "good",
            "best",
            "cute",
        ],
    ),
    (
        &["sad", "sorrow", "cry", "sorry"],
        &["sorry", "sad", "unfortunately", "miss", "lost", "lose", "cry", "oh no", "bad", "tired", "hurt"],
    ),
    (
        &["surprised", "surprise", "shock", "shocked", "wow"],
        &["wow", "whoa", "no way", "really", "seriously", "omg", "wait", "huh"],
    ),
    (&["angry", "mad", "annoyed", "grumpy"], &["angry", "hate", "ugh", "annoying", "stop it", "rude", "mad"]),
    (
        &["smug", "proud", "confident"],
        &["obviously", "of course", "told you", "easy", "clearly", "naturally", "genius"],
    ),
    (
        &["thinking", "think", "curious", "confused", "neutral", "relaxed"],
        &["hmm", "let me think", "wonder", "maybe", "perhaps", "not sure", "guess", "how", "why", "what", "should"],
    ),
];
/// Phrases that commit to an action ("watch this").
const COMMITMENTS: [&str; 10] = [
    "watch this",
    "here goes",
    "let me show",
    "i'll do",
    "i will do",
    "time for",
    "let's do",
    "check this",
    "look at this",
    "here's my",
];
/// Words that refuse one ("no backflips").
const REFUSALS: [&str; 7] = ["no", "not", "won't", "can't", "cannot", "don't", "never"];

/// The questions asked while the character speaks a line.
pub fn questions_speaking(inventory: &Inventory) -> Vec<Question> {
    let mut q = vec![
        Question::new(REACTION, "Which expression fits the character's last line best?", reaction_kind(inventory)),
        Question::new(
            ENERGY,
            "How much energy does the last line carry, from 0 (flat) to 4 (very high)?",
            QuestionKind::score(ENERGY_LEVELS),
        ),
    ];
    let gestures: Vec<String> = inventory.clips.iter().filter(|c| c.gesture).map(|c| c.id.to_string()).collect();
    if !gestures.is_empty() {
        q.push(Question::new(COMMIT, "Is the character committing to doing an action right now?", QuestionKind::YesNo));
        let mut options = gestures;
        options.push(NONE.to_owned());
        q.push(Question::new(CLIP, "Which gesture is she doing, if any?", QuestionKind::choice(options)));
    }
    q
}

/// The question asked when the person has spoken: which face would a listening friend make?
pub fn questions_listening(inventory: &Inventory) -> Vec<Question> {
    vec![
        Question::new(
            LISTENING,
            "Which expression would a friend listening to the person make?",
            reaction_kind(inventory),
        ),
        Question::new(
            ENERGY,
            "How much energy does the person's line carry, from 0 to 4?",
            QuestionKind::score(ENERGY_LEVELS),
        ),
    ]
}

fn reaction_kind(inventory: &Inventory) -> QuestionKind {
    let mut options: Vec<String> = inventory.expressions.iter().map(ToString::to_string).collect();
    options.push(NONE.to_owned());
    QuestionKind::choice(options)
}

/// The deterministic rule floor: answers every question from the words alone.
#[derive(Clone, Debug, Default)]
pub struct RuleFloor;

impl RuleFloor {
    /// Answers `ask` at once.
    pub fn answer(&self, ask: &Ask) -> Decision {
        let line = last_line(&ask.state).to_lowercase();
        let answers = ask
            .questions
            .iter()
            .map(|q| match (&q.kind, q.id.as_str()) {
                (QuestionKind::Choice { options, .. }, CLIP) => clip_answer(&q.id, options, &line),
                (QuestionKind::Choice { options, .. }, id) => mood_answer(id, options, &line, ask.moment),
                (QuestionKind::YesNo, _) => {
                    let yes = COMMITMENTS.iter().any(|c| line.contains(c));
                    Answer::pick(&q.id, 2, usize::from(yes), 0.8)
                }
                (QuestionKind::Score { levels, .. }, _) => {
                    let level = usize::from(energy(&line)).min(usize::from(*levels).saturating_sub(1));
                    Answer::pick(&q.id, usize::from(*levels), level, 0.7)
                }
                _ => Answer::new(&q.id, vec![1.0; q.kind.arity()]),
            })
            .collect();
        Decision::new("rules", answers)
    }
}

impl Decider for RuleFloor {
    fn kind(&self) -> &'static str {
        "rules"
    }

    fn ask(&mut self, ask: Ask) -> DecisionTicket {
        DecisionTicket::ready(Ok(self.answer(&ask)))
    }
}

/// The state's last line, without its speaker.
fn last_line(state: &str) -> &str {
    let line = state.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    line.split_once(": ").map_or(line, |(_, text)| text)
}

fn has_word(line: &str, cue: &str) -> bool {
    if cue.contains(' ') {
        return line.contains(cue);
    }
    line.split(|c: char| !(c.is_alphanumeric() || c == '\'')).any(|w| w == cue)
}

fn mood_answer(id: &str, options: &[String], line: &str, moment: Moment) -> Answer {
    let mut scores = vec![0.0f32; options.len()];
    for (names, cues) in MOODS {
        let hits = cues.iter().filter(|c| has_word(line, c)).count();
        if hits == 0 {
            continue;
        }
        // The first installed expression that shows this mood gets it.
        if let Some(i) = options.iter().position(|o| names.contains(&o.as_str())) {
            // Hit counts are tiny.
            #[allow(clippy::cast_precision_loss)]
            let s = 1.0 + hits as f32;
            scores[i] += s;
        }
    }
    let none = options.iter().position(|o| o == NONE);
    let best = scores.iter().copied().enumerate().max_by(|a, b| a.1.total_cmp(&b.1));
    match best {
        Some((i, s)) if s > 0.0 => Answer::pick(id, options.len(), i, (0.55 + 0.1 * s).min(0.9)),
        // Listening with nothing to go on: a friend's small, warm face, when there is one.
        _ if moment == Moment::Listening && line.contains('?') => {
            let curious = MOODS[5].0.iter().find_map(|n| options.iter().position(|o| o == n));
            curious.map_or_else(|| none_answer(id, options.len(), none), |i| Answer::pick(id, options.len(), i, 0.6))
        }
        _ if moment == Moment::Listening => {
            let warm = MOODS[0].0.iter().find_map(|n| options.iter().position(|o| o == n));
            warm.map_or_else(|| none_answer(id, options.len(), none), |i| Answer::pick(id, options.len(), i, 0.55))
        }
        _ => none_answer(id, options.len(), none),
    }
}

fn none_answer(id: &str, arity: usize, none: Option<usize>) -> Answer {
    none.map_or_else(|| Answer::new(id, vec![1.0; arity]), |i| Answer::pick(id, arity, i, 0.8))
}

fn clip_answer(id: &str, options: &[String], line: &str) -> Answer {
    let named = options.iter().position(|o| o != NONE && mentions(line, o));
    let none = options.iter().position(|o| o == NONE);
    match named {
        Some(i) => Answer::pick(id, options.len(), i, 0.8),
        None => none_answer(id, options.len(), none),
    }
}

/// Whether `line` mentions clip `name` (`wave` in "I'll wave", `back_flip` as "back flip" or "backflip").
fn mentions(line: &str, name: &str) -> bool {
    let spaced = name.replace(['_', '-'], " ");
    let joined = name.replace(['_', '-'], "");
    let stem = |w: &str| w.trim_end_matches('s').to_owned();
    let words: Vec<String> = line.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(stem).collect();
    let joined_line = words.join(" ");
    words.iter().any(|w| *w == stem(&joined)) || joined_line.contains(&spaced)
}

/// Whether `line` refuses clip `name` ("no backflips", "I won't wave").
pub fn vetoed(line: &str, name: &str) -> bool {
    let line = line.to_lowercase();
    let words: Vec<&str> =
        line.split(|c: char| !(c.is_alphanumeric() || c == '\'')).filter(|w| !w.is_empty()).collect();
    words.iter().enumerate().any(|(i, _)| {
        let window = words[i..words.len().min(i + 4)].join(" ");
        REFUSALS.contains(&words[i]) && mentions(&window, name)
    })
}

/// Energy 0 to 4 from the words: exclamations, capitals, laughter.
fn energy(line: &str) -> u8 {
    let bangs = line.matches('!').count();
    let laugh = ["haha", "hehe", "lol", "yay", "woo"].iter().any(|w| line.contains(w));
    let level = 1 + bangs.min(2) + usize::from(laugh);
    u8::try_from(level.min(4)).unwrap_or(4)
}

/// The gates every provider's answers pass (#2348).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Gates {
    /// The least probability a reaction needs.
    pub reaction: f32,
    /// The least probability a clip needs.
    pub clip: f32,
    /// The least `p(yes)` the commitment needs before a clip plays.
    pub commit: f32,
}

impl Default for Gates {
    fn default() -> Self {
        Self { reaction: 0.5, clip: 0.6, commit: 0.7 }
    }
}

impl Gates {
    /// The intents `decision` earns for `ask`, given the line it was about (for the veto): a reaction emote with an
    /// intensity from the energy, and a gesture only on a spoken commitment that the line doesn't refuse.
    pub fn intents(&self, ask: &Ask, decision: &Decision, line: &str) -> Vec<Intent> {
        let mut out = Vec::new();
        let energy = decision.answer(ENERGY).map_or(1.0, Answer::expected);
        for id in [REACTION, LISTENING] {
            let (Some(q), Some(a)) = (ask.question(id), decision.answer(id)) else { continue };
            let QuestionKind::Choice { options, .. } = &q.kind else { continue };
            let Some((i, p)) = a.best() else { continue };
            let name = &options[i];
            if name == NONE || p < self.reaction {
                continue;
            }
            let Ok(expression) = ExpressionId::new(name) else { continue };
            let base = if ask.moment == Moment::Listening { 0.35 } else { 0.5 };
            let intensity = (base + 0.12 * energy).min(1.0);
            let hold = if ask.moment == Moment::Listening { 2.5 } else { 1.8 };
            out.push(Intent::emote(expression, intensity, hold));
        }
        if let (Some(q), Some(commit), Some(clip)) =
            (ask.question(CLIP), decision.answer(COMMIT), decision.answer(CLIP))
            && let (QuestionKind::Choice { options, .. }, Some((i, p))) = (&q.kind, clip.best())
            && let Some(name) = options.get(i)
            && name != NONE
            && p >= self.clip
            && commit.yes() >= self.commit
            && !vetoed(line, name)
            && let Ok(clip) = ClipId::new(name.as_str())
        {
            out.push(Intent::gesture(clip, 1.0));
        }
        out
    }
}

/// Fills the questions `decision` missed from the rule floor's answers.
pub fn with_floor(ask: &Ask, decision: Decision, floor: &Decision) -> Decision {
    let provider = decision.provider.clone();
    let mut decision = decision.fitted(ask);
    for q in &ask.questions {
        if decision.answer(&q.id).is_none()
            && let Some(a) = floor.answer(&q.id)
        {
            decision.answers.push(a.clone());
        }
    }
    Decision::new(provider, decision.answers)
}

/// A local model as the decider: one short JSON request per ask, on its own thread, through any [`LlmClient`]
/// (a loopback OpenAI-compatible server for a local model; a hosted one only if the person configured it).
pub struct LlmDecider {
    client: Arc<dyn LlmClient>,
    kind: String,
    idle: Duration,
}

impl LlmDecider {
    /// A decider over `client`, named `kind` in decisions (`local`).
    pub fn new(client: Arc<dyn LlmClient>, kind: impl Into<String>) -> Self {
        Self { client, kind: kind.into(), idle: Duration::from_secs(2) }
    }

    /// The request for `ask`.
    pub fn request(ask: &Ask) -> TurnRequest {
        let mut prompt = String::from("Answer each question about the conversation with one option from its list.\n");
        prompt.push_str("Reply with only a JSON object mapping each question id to the chosen option.\n\n");
        prompt.push_str("<conversation>\n");
        prompt.push_str(&crate::prompt::sanitise(&ask.state));
        prompt.push_str("\n</conversation>\n\n");
        for q in &ask.questions {
            let options = match &q.kind {
                QuestionKind::Choice { options, .. } => options.join(" | "),
                QuestionKind::YesNo => "yes | no".to_owned(),
                QuestionKind::Score { levels, .. } => {
                    (0..*levels).map(|l| l.to_string()).collect::<Vec<_>>().join(" | ")
                }
                _ => String::new(),
            };
            let _ = writeln!(prompt, "{}: {} Options: {options}", q.id, q.prompt);
        }
        TurnRequest::new(vec![Message::new(Role::User, vec![ContentBlock::Text(prompt)])], 120)
            .with_system(vec![SystemBlock::new("You label conversations. Output JSON only.")])
    }

    /// Reads a model's reply: the first JSON object in it, each value matched to an option.
    ///
    /// # Errors
    /// [`DecideError::Provider`] when there is no JSON object.
    pub fn parse(ask: &Ask, reply: &str, kind: &str) -> Result<Decision, DecideError> {
        let start = reply.find('{').ok_or_else(|| DecideError::provider("no JSON object in the reply"))?;
        let end = reply
            .rfind('}')
            .filter(|e| *e > start)
            .ok_or_else(|| DecideError::provider("no JSON object in the reply"))?;
        let value: serde_json::Value =
            serde_json::from_str(&reply[start..=end]).map_err(|e| DecideError::provider(e.to_string()))?;
        let mut answers = Vec::new();
        for q in &ask.questions {
            let Some(v) = value.get(&q.id) else { continue };
            let text = match v {
                serde_json::Value::String(s) => s.trim().to_lowercase(),
                other => other.to_string(),
            };
            let pick = match &q.kind {
                QuestionKind::Choice { options, .. } => options.iter().position(|o| *o == text),
                QuestionKind::YesNo => match text.as_str() {
                    "yes" | "true" => Some(1),
                    "no" | "false" => Some(0),
                    _ => None,
                },
                QuestionKind::Score { levels, .. } => text.parse::<u8>().ok().filter(|l| l < levels).map(usize::from),
                _ => None,
            };
            if let Some(i) = pick {
                answers.push(Answer::pick(&q.id, q.kind.arity(), i, 0.85));
            }
        }
        Ok(Decision::new(kind, answers))
    }
}

impl std::fmt::Debug for LlmDecider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmDecider").field("kind", &self.kind).finish_non_exhaustive()
    }
}

impl Decider for LlmDecider {
    fn kind(&self) -> &str {
        &self.kind
    }

    fn ask(&mut self, ask: Ask) -> DecisionTicket {
        let (ticket, writer) = DecisionTicket::pending();
        let client = Arc::clone(&self.client);
        let kind = self.kind.clone();
        let idle = self.idle;
        let spawned = std::thread::Builder::new().name("performer-decide".into()).spawn(move || {
            let result = client
                .stream(Self::request(&ask), &Cancel::new())
                .and_then(|mut stream| stream.collect(idle))
                .map_err(|e| DecideError::provider(e.to_string()))
                .and_then(|turn| {
                    let text: String = turn
                        .message
                        .content
                        .iter()
                        .filter_map(|c| if let ContentBlock::Text(t) = c { Some(t.as_str()) } else { None })
                        .collect();
                    Self::parse(&ask, &text, &kind)
                });
            writer.fill(result);
        });
        if let Err(e) = spawned {
            return DecisionTicket::ready(Err(DecideError::provider(e.to_string())));
        }
        ticket
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_llm_api::stream::{LlmEvent, StopReason};
    use omuzaki_llm_api::usage::Usage;
    use omuzaki_llm_mock::{FakeLlm, Script, ScriptTurn};
    use omuzaki_performer_api::stage::ClipInfo;

    use super::*;

    fn inventory() -> Inventory {
        Inventory::new(
            ["happy", "sad", "surprised", "smug", "thinking"].iter().map(|e| ExpressionId::new(*e).unwrap()).collect(),
            vec![
                ClipInfo::new(ClipId::new("wave").unwrap(), true, 1.2),
                ClipInfo::new(ClipId::new("backflip").unwrap(), true, 2.0),
            ],
            vec![],
            vec![],
        )
    }

    fn speaking(line: &str) -> Ask {
        Ask::new(Moment::Speaking, format!("person: hi\nmochi: {line}"), questions_speaking(&inventory()))
    }

    fn decide(ask: &Ask) -> Vec<Intent> {
        let line = last_line(&ask.state).to_owned();
        Gates::default().intents(ask, &RuleFloor.answer(ask), &line)
    }

    fn verbs(intents: &[Intent]) -> Vec<String> {
        intents
            .iter()
            .map(|i| match i {
                Intent::Emote { expression, .. } => format!("emote {expression}"),
                Intent::Gesture { clip, .. } => format!("gesture {clip}"),
                other => other.verb().unwrap_or("?").to_owned(),
            })
            .collect()
    }

    #[test]
    fn the_rule_floor_reads_the_mood_of_a_line() {
        assert_eq!(verbs(&decide(&speaking("Hi chat! Welcome, this is great!"))), ["emote happy"]);
        assert_eq!(verbs(&decide(&speaking("Oh no, I'm so sorry about that."))), ["emote sad"]);
        assert_eq!(verbs(&decide(&speaking("Wow, no way!"))), ["emote surprised"]);
        assert_eq!(verbs(&decide(&speaking("The crane, obviously."))), ["emote smug"]);
        assert_eq!(decide(&speaking("The train leaves at four.")), []);
    }

    #[test]
    fn an_action_needs_a_commitment_and_no_refusal() {
        assert_eq!(verbs(&decide(&speaking("Watch this, I'll wave!"))), ["gesture wave"]);
        assert!(verbs(&decide(&speaking("I like to wave."))).iter().all(|v| !v.starts_with("gesture")));
        assert!(verbs(&decide(&speaking("Watch this: no backflips today!"))).iter().all(|v| !v.starts_with("gesture")));
        assert!(vetoed("No backflips, sorry", "backflip") && !vetoed("Here's my backflip", "backflip"));
    }

    #[test]
    fn a_listening_friend_always_has_a_face() {
        let listen =
            |line: &str| Ask::new(Moment::Listening, format!("person: {line}"), questions_listening(&inventory()));
        assert_eq!(verbs(&decide(&listen("I lost my keys today"))), ["emote sad"]);
        assert_eq!(verbs(&decide(&listen("What should I build next?"))), ["emote thinking"]);
        assert_eq!(verbs(&decide(&listen("The weather is mild."))), ["emote happy"]);
        // Intensity rises with energy and stays in range.
        let Intent::Emote { intensity, .. } = &decide(&listen("This is amazing!!! haha"))[0] else {
            panic!("an emote")
        };
        assert!(*intensity > 0.6 && *intensity <= 1.0);
    }

    #[test]
    fn the_floor_fills_what_a_model_missed() {
        let ask = speaking("Hi chat!");
        let floor = RuleFloor.answer(&ask);
        let partial = Decision::new("local", vec![Answer::pick(REACTION, 6, 2, 0.9), Answer::new("bogus", vec![1.0])]);
        let filled = with_floor(&ask, partial, &floor);
        assert_eq!(filled.provider, "local");
        assert_eq!(filled.answers.len(), ask.questions.len());
        assert_eq!(filled.answer(REACTION).and_then(Answer::best).map(|b| b.0), Some(2));
    }

    #[test]
    fn a_local_model_answers_through_the_seam() {
        let reply = "Sure! {\"reaction\": \"happy\", \"energy\": 3, \"commit\": \"no\", \"clip\": \"none\"}";
        let llm = FakeLlm::new(Script::new("local").with_turn(ScriptTurn::new(vec![
            LlmEvent::start("local"),
            LlmEvent::TextDelta(reply.into()),
            LlmEvent::Usage(Usage::new(100, 20)),
            LlmEvent::Stop(StopReason::EndTurn),
        ])));
        let mut decider = LlmDecider::new(Arc::new(llm), "local");
        let ask = speaking("Hi chat!");
        let ticket = decider.ask(ask.clone());
        let decision = loop {
            if let Some(result) = ticket.poll() {
                break result.unwrap();
            }
            std::thread::yield_now();
        };
        assert_eq!(decision.provider, "local");
        assert_eq!(decision.answer(REACTION).and_then(Answer::best).map(|b| b.0), Some(0));
        assert_eq!(decision.answer(ENERGY).and_then(Answer::best).map(|b| b.0), Some(3));
        assert!(decision.answer(COMMIT).is_some_and(|a| a.yes() < 0.5));
        assert!(LlmDecider::parse(&ask, "no json here", "local").is_err());
    }
}
