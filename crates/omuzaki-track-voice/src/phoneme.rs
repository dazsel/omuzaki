//! The mouth from phonemes and their times (#2351): when the words are known (the AI performer's own TTS line, a short's
//! voice track), the mouth comes from what is said, not from guessing vowels in the sound.
//!
//! - [`Phone`]: the classes the mouth cares about: the five vowels, the lip closures (`p b m`), the lip-to-teeth
//!   sounds (`f v`), rounded `w`, the other consonants, and silence.
//! - [`phones`]: Kokoro's (misaki's) phoneme string into phones; a diphthong is two vowels.
//! - [`PhoneSpan`]: a phone and its time; [`align`](crate::align) finds the times in the TTS audio when the engine does
//!   not report them.
//! - [`PhonemeMouth`]: the mouth at any time over a list of spans: each vowel's shape eased in and out across its
//!   edges, lips shut through a closure (reached [`CLOSE_LEAD_S`] before it, so they are shut when it starts), the
//!   neighbouring vowel kept half open through other consonants, and closed in silence.
//!
//! [`PhonemeMouth::write`] writes the frame into a layer through the character's own viseme expressions, as
//! [`LipSync`](crate::LipSync) does, so both mouths are the character's.

use omuzaki_anim_api::{Channel, ChannelBuf, ChannelMask};
use omuzaki_face_api::expression::{ExpressionId, ExpressionSet, IntensityCurve};

use crate::analyzer::VoiceFrame;
use crate::lipsync::PRESS;
use crate::viseme::Viseme;

/// What a sound does to the mouth.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Phone {
    /// A vowel, shown as its viseme.
    Vowel(Viseme),
    /// Both lips shut: `p`, `b`, `m`.
    Bilabial,
    /// The lower lip to the upper teeth: `f`, `v`. Nearly shut.
    Labiodental,
    /// Rounded lips: `w`. Shown as a small `ou`.
    Round,
    /// Any other consonant: the mouth stays near its neighbouring vowel, half open.
    Consonant,
    /// No sound: the mouth shuts.
    Silence,
}

impl Phone {
    /// Whether the lips are shut for it (the closures lip-sync recall is scored on).
    pub fn is_closure(self) -> bool {
        self == Self::Bilabial
    }
}

/// Seconds before a closure that the lips are already shut: speakers close ahead of the sound.
pub const CLOSE_LEAD_S: f64 = 0.02;
/// Seconds a vowel's shape takes to come in at its start and go at its end, each way.
pub const EASE_S: f64 = 0.04;
/// How open the mouth stays through a consonant that isn't a closure, as a share of the nearest vowel.
const CONSONANT_OPEN: f32 = 0.45;
/// How shut the lips are for `f` and `v`.
const LABIODENTAL_SHUT: f32 = 0.75;
/// How open `w` shows as `ou`.
const ROUND_OPEN: f32 = 0.5;

/// Kokoro's (misaki's) phonemes as phones, in order. Stress and length marks are dropped; a space or a punctuation mark
/// is a silence (one per run); a diphthong (`A` eɪ, `I` aɪ, `O` oʊ, `W` aʊ, `Y` ɔɪ, `Q` əʊ) is its two vowels.
pub fn phones(text: &str) -> Vec<Phone> {
    let mut out: Vec<Phone> = Vec::with_capacity(text.chars().count());
    let push = |phone: Phone, out: &mut Vec<Phone>| {
        if !(phone == Phone::Silence && out.last().is_none_or(|last| *last == Phone::Silence)) {
            out.push(phone);
        }
    };
    for c in text.chars() {
        let vowel = |v: Viseme| Phone::Vowel(v);
        let two: Option<[Phone; 2]> = match c {
            'A' => Some([vowel(Viseme::Ee), vowel(Viseme::Ih)]),
            'I' => Some([vowel(Viseme::Aa), vowel(Viseme::Ih)]),
            'O' | 'Q' => Some([vowel(Viseme::Oh), vowel(Viseme::Ou)]),
            'W' => Some([vowel(Viseme::Aa), vowel(Viseme::Ou)]),
            'Y' => Some([vowel(Viseme::Oh), vowel(Viseme::Ih)]),
            _ => None,
        };
        if let Some(pair) = two {
            for phone in pair {
                push(phone, &mut out);
            }
            continue;
        }
        let phone = match c {
            'a' | 'ɑ' | 'æ' | 'ɐ' | 'ʌ' | 'ə' | 'ᵊ' => vowel(Viseme::Aa),
            'i' | 'ɪ' | 'ᵻ' => vowel(Viseme::Ih),
            'u' | 'ʊ' => vowel(Viseme::Ou),
            'e' | 'ɛ' | 'ɜ' | 'ɚ' => vowel(Viseme::Ee),
            'o' | 'ɔ' | 'ɒ' => vowel(Viseme::Oh),
            'p' | 'b' | 'm' => Phone::Bilabial,
            'f' | 'v' => Phone::Labiodental,
            'w' => Phone::Round,
            // Stress, length and syllable marks say nothing about the mouth.
            'ˈ' | 'ˌ' | 'ː' | '\u{0329}' => continue,
            c if c.is_whitespace() || c.is_ascii_punctuation() || "—…“”".contains(c) => Phone::Silence,
            c if c.is_alphabetic() => Phone::Consonant,
            _ => continue,
        };
        push(phone, &mut out);
    }
    while out.last() == Some(&Phone::Silence) {
        out.pop();
    }
    if out.first() == Some(&Phone::Silence) {
        out.remove(0);
    }
    out
}

/// A phone and when it sounds, seconds from the start of the audio.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PhoneSpan {
    /// The phone.
    pub phone: Phone,
    /// When it starts.
    pub start_s: f64,
    /// When it ends (after `start_s`).
    pub end_s: f64,
}

/// The mouth at one moment: the five viseme weights (`aa` … `oh`) and how shut the lips are for a closure.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct MouthFrame {
    /// Viseme weights, `0..1` each.
    pub visemes: [f32; 5],
    /// `0` open as the visemes say, `1` lips shut and pressed.
    pub closure: f32,
}

impl MouthFrame {
    /// The strongest viseme, when any shows.
    pub fn viseme(&self) -> Option<Viseme> {
        let (index, weight) =
            self.visemes.iter().copied().enumerate().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap_or((0, 0.0));
        (weight > 0.0).then(|| Viseme::ALL[index])
    }

    /// How open it is, `0..1`: the viseme weights' sum, less the closure.
    pub fn openness(&self) -> f32 {
        (self.visemes.iter().sum::<f32>() * (1.0 - self.closure)).clamp(0.0, 1.0)
    }

    /// Puts this mouth on an analyser frame, as [`LipSync`](crate::LipSync) reads it: the visemes narrowed by a
    /// partial closure (`f v`), and the frame's closure set when the lips meet (`p b m`, from [`SHUT_FROM`]). Level,
    /// voicing and onsets stay the audio's.
    pub fn onto(&self, frame: &mut VoiceFrame) {
        let shut = self.closure >= SHUT_FROM;
        let open = if shut { 0.0 } else { 1.0 - self.closure.clamp(0.0, 1.0) };
        frame.visemes = self.visemes.map(|v| v * open);
        frame.closure = if shut { 1.0 } else { 0.0 };
    }
}

/// A phone mouth this shut is a closure: the lips meet (`p b m`); less is a narrowing that shrinks the vowel (`f v`).
pub const SHUT_FROM: f32 = 0.9;

/// `t` eased: 0 at 0, 1 at 1, smooth at both ends.
fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The mouth over a list of spans in time order (see the module docs). Cheap to build; [`frame`](Self::frame) is a
/// binary search and a few neighbours, and allocates nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhonemeMouth {
    spans: Vec<PhoneSpan>,
}

impl PhonemeMouth {
    /// The mouth over `spans`, sorted by start.
    pub fn new(mut spans: Vec<PhoneSpan>) -> Self {
        spans.retain(|s| s.start_s.is_finite() && s.end_s.is_finite() && s.end_s > s.start_s);
        spans.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
        Self { spans }
    }

    /// The spans.
    pub fn spans(&self) -> &[PhoneSpan] {
        &self.spans
    }

    /// The mouth at `t` seconds.
    pub fn frame(&self, t: f64) -> MouthFrame {
        let mut out = MouthFrame::default();
        // The span sounding at `t`, if any.
        let at = self.spans.partition_point(|s| s.start_s <= t).checked_sub(1).filter(|&i| self.spans[i].end_s > t);
        // Each vowel near `t` shows by how far into it `t` is, eased over its edges.
        let first = self.spans.partition_point(|s| s.end_s + EASE_S < t);
        for span in self.spans[first..].iter().take_while(|s| s.start_s - EASE_S <= t) {
            let Phone::Vowel(viseme) = span.phone else { continue };
            let ease = EASE_S.min((span.end_s - span.start_s) / 2.0);
            let rise = smooth((t - (span.start_s - ease)) / (2.0 * ease));
            let fall = smooth(((span.end_s + ease) - t) / (2.0 * ease));
            #[allow(clippy::cast_possible_truncation)] // a weight
            let w = rise.min(fall) as f32;
            let slot = &mut out.visemes[viseme.index()];
            *slot = slot.max(w);
        }
        let current = at.map(|i| self.spans[i].phone);
        let total: f32 = out.visemes.iter().sum();
        if total > 1.0 {
            for v in &mut out.visemes {
                *v /= total;
            }
        }
        match current {
            Some(Phone::Consonant) => {
                // Half open on the nearer vowel's shape.
                let near = self.nearest_vowel(at.unwrap_or(0));
                if let Some(v) = near {
                    let slot = &mut out.visemes[v.index()];
                    *slot = slot.max(CONSONANT_OPEN);
                }
            }
            Some(Phone::Round) => {
                let slot = &mut out.visemes[Viseme::Ou.index()];
                *slot = slot.max(ROUND_OPEN);
            }
            Some(Phone::Labiodental) => out.closure = out.closure.max(LABIODENTAL_SHUT),
            _ => {}
        }
        // Lips shut through a closure, reached a little before it and let go at its end.
        let first = self.spans.partition_point(|s| s.end_s < t);
        for span in self.spans[first..].iter().take_while(|s| s.start_s - CLOSE_LEAD_S <= t) {
            if span.phone.is_closure() && t < span.end_s {
                #[allow(clippy::cast_possible_truncation)] // a weight
                let shut = smooth((t - (span.start_s - CLOSE_LEAD_S)) / CLOSE_LEAD_S) as f32;
                out.closure = out.closure.max(shut);
            }
        }
        for v in &mut out.visemes {
            *v *= 1.0 - out.closure;
        }
        out
    }

    /// The viseme of the vowel nearest span `index`, looking both ways.
    fn nearest_vowel(&self, index: usize) -> Option<Viseme> {
        let vowel = |s: &PhoneSpan| match s.phone {
            Phone::Vowel(v) => Some(v),
            _ => None,
        };
        let before = self.spans[..index].iter().rev().take(3).enumerate().find_map(|(d, s)| vowel(s).map(|v| (d, v)));
        let after = self
            .spans
            .get(index + 1..)
            .unwrap_or_default()
            .iter()
            .take(3)
            .enumerate()
            .find_map(|(d, s)| vowel(s).map(|v| (d, v)));
        match (before, after) {
            (Some((a, va)), Some((b, vb))) => Some(if a <= b { va } else { vb }),
            (Some((_, v)), None) | (None, Some((_, v))) => Some(v),
            (None, None) => None,
        }
    }
}

/// A mouth frame into channels through the character's own viseme expressions: the jaw, lips and corners the visemes
/// move, and the lips pressed while shut. Cold to build, allocation-free to use.
#[derive(Clone, Debug)]
pub struct VisemeShapes {
    deltas: [ChannelBuf; 5],
    curves: [IntensityCurve; 5],
    mask: ChannelMask,
}

impl VisemeShapes {
    /// The shapes of `expressions`' visemes (one the set lacks moves nothing).
    pub fn new(expressions: &ExpressionSet) -> Self {
        let mut deltas = [ChannelBuf::NEUTRAL; 5];
        let mut curves = [IntensityCurve::Linear; 5];
        let mut mask = ChannelMask::EMPTY;
        for viseme in Viseme::ALL {
            let Some(expression) = ExpressionId::new(viseme.id()).ok().and_then(|id| expressions.0.get(&id)) else {
                continue;
            };
            curves[viseme.index()] = expression.curve;
            for (&channel, &value) in &expression.deltas.0 {
                #[allow(clippy::cast_possible_truncation)] // channel values are small; the bus is f32
                let value = value as f32;
                deltas[viseme.index()][channel] = value;
                mask.insert(channel);
            }
        }
        if !mask.is_empty() {
            mask.insert(Channel::MOUTH_PRESS_LEFT);
            mask.insert(Channel::MOUTH_PRESS_RIGHT);
        }
        Self { deltas, curves, mask }
    }

    /// The channels the shapes move.
    pub fn mask(&self) -> ChannelMask {
        self.mask
    }

    /// `rest` with `frame` on the mouth channels.
    pub fn write(&self, frame: &MouthFrame, rest: &ChannelBuf, out: &mut ChannelBuf) {
        let shaped: [f32; 5] = std::array::from_fn(|i| self.curves[i].eval(frame.visemes[i]));
        for channel in self.mask {
            let mut moved: f32 = shaped.iter().zip(&self.deltas).map(|(w, d)| w * d[channel]).sum();
            if matches!(channel, Channel::MOUTH_PRESS_LEFT | Channel::MOUTH_PRESS_RIGHT) {
                moved += PRESS * frame.closure;
            }
            out[channel] = (rest[channel] + moved).clamp(channel.info().min, channel.info().max);
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn misaki_phonemes_read_as_phones() {
        use Phone::{Bilabial, Consonant, Silence, Vowel};
        // "my bad, man": mˈI bˈæd, mˈæn
        let got = phones("mˈI bˈæd, mˈæn");
        assert_eq!(
            got,
            [
                Bilabial,
                Vowel(Viseme::Aa),
                Vowel(Viseme::Ih),
                Silence,
                Bilabial,
                Vowel(Viseme::Aa),
                Consonant,
                Silence,
                Bilabial,
                Vowel(Viseme::Aa),
                Consonant
            ]
        );
        assert_eq!(phones("  "), []);
        assert_eq!(
            phones("fˈIv wˈʌn"),
            [
                Phone::Labiodental,
                Vowel(Viseme::Aa),
                Vowel(Viseme::Ih),
                Phone::Labiodental,
                Silence,
                Phone::Round,
                Vowel(Viseme::Aa),
                Consonant
            ]
        );
    }

    fn span(phone: Phone, start_s: f64, end_s: f64) -> PhoneSpan {
        PhoneSpan { phone, start_s, end_s }
    }

    #[test]
    fn the_lips_shut_through_a_closure_and_a_vowel_shows_its_own_shape() {
        let mouth = PhonemeMouth::new(vec![
            span(Phone::Vowel(Viseme::Aa), 0.0, 0.2),
            span(Phone::Bilabial, 0.2, 0.28),
            span(Phone::Vowel(Viseme::Oh), 0.28, 0.5),
        ]);
        let mid_a = mouth.frame(0.1);
        assert_eq!((mid_a.viseme(), mid_a.closure), (Some(Viseme::Aa), 0.0));
        assert_eq!(mid_a.visemes[0], 1.0);
        let shut = mouth.frame(0.24);
        assert_eq!(shut.closure, 1.0);
        assert!(shut.openness() < 1e-6, "{shut:?}");
        assert_eq!(mouth.frame(0.2).closure, 1.0, "already shut when the closure starts");
        assert_eq!(mouth.frame(0.4).viseme(), Some(Viseme::Oh));
        assert_eq!(mouth.frame(0.7), MouthFrame::default(), "after the last span the mouth is shut");
    }
}
