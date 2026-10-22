//! `asr@1`: one utterance of 16 kHz mono speech (at most 30 s) to its text, with the language in and out.
//!
//! `language` in is a code such as `en`, or empty to let a multilingual model detect it; `language` out is the one the
//! model used. Text is UTF-8 bytes.

use omuzaki_ai_api::{
    Better, Check, DType, Dim, EvalEnv, EvalError, EvalLevel, EvalReport, Metric, Session, Signature, Slot, SlotInfo,
    SlotMode, SlotRef, TensorSpec, TestSet, TestSetKind,
};

use crate::SpeechError;
use crate::asr::AsrCall;
use crate::slots::clips::CLIPS;

/// The largest word error rate a model may have on the smoke set. Whisper `tiny.en`, the smallest shipped model, scores
/// 0.051 and `base.en` 0.026 on these clips (the LibriVox preambles included); the ceiling leaves room for a smaller
/// or quantised model while refusing one that misses one word in six.
pub const SMOKE_CEILING: f64 = 0.15;

/// The longest utterance the slot takes, in seconds.
pub const MAX_SECONDS: f64 = 30.0;

/// `asr@1`.
#[must_use]
pub fn asr_slot() -> SlotRef {
    super::slot_ref("asr")
}

/// `pcm f32[any]`, `language u8[any]` → `text u8[any]`, `language u8[any]`.
#[must_use]
pub fn asr_signature() -> Signature {
    let any = |name: &str, dtype| TensorSpec::new(name, dtype, [Dim::ANY]);
    Signature {
        inputs: vec![any("pcm", DType::F32), any("language", DType::U8)],
        outputs: vec![any("text", DType::U8), any("language", DType::U8)],
    }
}

/// One item: the utterances of a reading and what was read.
pub struct Reading<'a> {
    /// The utterances, 16 kHz mono, each at most [`MAX_SECONDS`].
    pub utterances: Vec<&'a [f32]>,
    /// What was read.
    pub text: &'a str,
}

/// The word error rate of `session` over `items`, in English: every item's utterances are transcribed in order and
/// joined, and the edit distance in words to the reference, summed over items, is divided by the reference's words.
/// Case, punctuation and the spelling of numbers don't count.
///
/// # Errors
/// [`EvalError::Run`] when the model fails, [`EvalError::Output`] when its signature isn't `asr@1`'s or its text isn't
/// UTF-8.
pub fn word_error_rate(session: &mut dyn Session, items: &[Reading<'_>]) -> Result<f64, EvalError> {
    if session.signature() != &asr_signature() {
        return Err(EvalError::Output("the session doesn't present asr@1's signature".into()));
    }
    let mut call = AsrCall::new();
    let (mut errors, mut total) = (0usize, 0usize);
    for item in items {
        let mut heard = String::new();
        for utterance in &item.utterances {
            let transcript = call.transcribe(session, utterance, Some("en")).map_err(|e| match e {
                SpeechError::Run(run) => EvalError::Run(run),
                other => EvalError::Output(other.to_string()),
            })?;
            heard.push_str(&transcript.text);
            heard.push(' ');
        }
        let reference = words(item.text);
        errors += edit_distance(&words(&heard), &reference);
        total += reference.len();
    }
    if total == 0 {
        return Err(EvalError::Output("the test set has no words".into()));
    }
    Ok(errors as f64 / total as f64)
}

/// Splits `text` into lower-case words without punctuation, with numbers under 100 spelled out ("3" and "three" are one
/// word read two ways).
#[must_use]
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == '-' || c == '\u{2014}')
        .map(|w| w.chars().filter(|c| c.is_alphanumeric() || *c == '\'').collect::<String>().to_lowercase())
        .map(|w| w.trim_matches('\'').to_owned())
        .filter(|w| !w.is_empty())
        .flat_map(|w| match w.parse::<u8>() {
            Ok(n) if n < 100 => number(n).split(' ').map(str::to_owned).collect(),
            _ => vec![w],
        })
        .collect()
}

/// `n` (under 100) in English words.
fn number(n: u8) -> String {
    const ONES: [&str; 20] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve",
        "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen",
    ];
    const TENS: [&str; 10] = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];
    match n {
        0..=19 => ONES[usize::from(n)].to_owned(),
        _ if n.is_multiple_of(10) => TENS[usize::from(n / 10)].to_owned(),
        _ => format!("{} {}", TENS[usize::from(n / 10)], ONES[usize::from(n % 10)]),
    }
}

/// The edit distance between two word lists.
#[must_use]
pub fn edit_distance(a: &[String], b: &[String]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let next = (diagonal + usize::from(x != y)).min(row[j] + 1).min(row[j + 1] + 1);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// `asr@1`. Smoke and full run the same set today: the three LibriVox fables, cut at their pauses.
#[derive(Default)]
pub struct AsrSlot;

impl Slot for AsrSlot {
    fn info(&self) -> SlotInfo {
        SlotInfo {
            slot: asr_slot(),
            summary: "Speech to text: what was said in one utterance from the microphone.".into(),
            without_model: "speech isn't turned into text: live captions, AI listening and voice commands are off".into(),
            mode: SlotMode::Call { stall_ms: 30_000 },
            threads: 4,
            likeness: None,
        }
    }

    fn signature(&self) -> Signature {
        asr_signature()
    }

    fn evaluate(
        &self,
        session: &mut dyn Session,
        env: &dyn EvalEnv,
        level: EvalLevel,
    ) -> Result<EvalReport, EvalError> {
        let audio: Vec<Vec<f32>> = CLIPS.iter().map(|clip| clip.load(env)).collect::<Result<_, _>>()?;
        let items: Vec<Reading<'_>> = CLIPS
            .iter()
            .zip(&audio)
            .map(|(clip, audio)| Reading { utterances: clip.utterances(audio, MAX_SECONDS - 5.0), text: clip.text })
            .collect();
        let wer = word_error_rate(session, &items)?;
        let set = TestSet { id: "librivox-aesop-3-1".into(), kind: TestSetKind::Real };
        let metric = Metric { name: "wer".into(), value: wer, better: Better::Lower, set };
        Ok(EvalReport { level, checks: vec![Check { metric, floor: Some(SMOKE_CEILING) }] })
    }
}

omuzaki_ai_api::register_slot!(AsrSlot);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_ignore_case_punctuation_and_how_numbers_are_written() {
        assert_eq!(words("Volume 3. \"Pray do not kill me!\""), ["volume", "three", "pray", "do", "not", "kill", "me"]);
        assert_eq!(words("Section 23,"), ["section", "twenty", "three"]);
        assert_eq!(words("neighbours' faults; That's it"), ["neighbours", "faults", "that's", "it"]);
    }

    #[test]
    fn the_edit_distance_counts_substitutions_insertions_and_deletions() {
        let w = |s: &str| words(s);
        assert_eq!(edit_distance(&w("a b c"), &w("a b c")), 0);
        assert_eq!(edit_distance(&w("a x c"), &w("a b c")), 1);
        assert_eq!(edit_distance(&w("a b"), &w("a b c")), 1);
        assert_eq!(edit_distance(&w("a b c d"), &w("a b c")), 1);
        assert_eq!(edit_distance(&w(""), &w("a b c")), 3);
    }
}
