//! Forced alignment (#2351): the times of known phones in a voice, from the analyser's frames alone. A TTS engine knows
//! what it said but Kokoro's graph reports no durations, so the line's phones are laid onto its audio here.
//!
//! Each phone takes a run of 10 ms frames, in order, between a shortest and a longest length for its class; the runs
//! are chosen by dynamic programming to cost least, where a frame's cost says how unlike the phone it sounds: a vowel
//! wants a voiced frame whose formants match its viseme, a closure a stop's quiet gap or a nasal's low murmur, a
//! silence no voice at all. A pause may fall between any two phones (a TTS voice often runs words together, and pauses
//! where the text has no mark), and the line may begin and end with silence; the text's own pause marks are not needed. Classic signal processing, no model: a 10-second line aligns in a few milliseconds.

use crate::analyzer::{HOP_S, VoiceFrame, WINDOW_S};
use crate::phoneme::{Phone, PhoneSpan};

/// Shortest and longest runs, frames.
fn bounds(phone: Phone) -> (usize, usize) {
    match phone {
        Phone::Vowel(_) => (3, 45),
        Phone::Bilabial | Phone::Labiodental | Phone::Round | Phone::Consonant => (2, 25),
        Phone::Silence => (0, 300),
    }
}

/// How unlike `phone` the frame sounds, `0` (just like it) up.
fn cost(phone: Phone, f: &VoiceFrame) -> f32 {
    let share = |i: usize| {
        let total: f32 = f.visemes.iter().sum();
        if total > 0.0 { f.visemes[i] / total } else { 0.0 }
    };
    let shut = f.closure > 0.5;
    let silent = !f.voiced && f.loudness <= 0.0;
    match phone {
        Phone::Vowel(v) if f.voiced => 1.0 - share(v.index()) + if shut { 0.6 } else { 0.0 },
        Phone::Vowel(_) => 3.0,
        Phone::Bilabial if shut => 0.1,
        Phone::Bilabial if silent => 0.5,
        Phone::Bilabial => 1.4,
        Phone::Round if f.voiced => 1.0 - share(crate::Viseme::Ou.index()),
        Phone::Labiodental | Phone::Consonant | Phone::Round if !f.voiced => 0.3,
        Phone::Labiodental | Phone::Consonant | Phone::Round => 0.9,
        Phone::Silence if silent => 0.0,
        Phone::Silence if f.voiced => 3.0,
        Phone::Silence => 0.7,
    }
}

/// The time between frames `k - 1` and `k`. The first frame is the first full window, and each is centred on its
/// window, so frame `k` is heard at `WINDOW_S / 2 + k · HOP_S`.
#[allow(clippy::cast_precision_loss)] // frame counts are small
fn boundary(k: usize) -> f64 {
    (f64::from(WINDOW_S) / 2.0 + f64::from(HOP_S) * (k as f64 - 0.5)).max(0.0)
}

/// The spans of `phones` in the voice the `frames` were heard from (one per 10 ms hop, from the start), with a silence
/// allowed before and after. When the frames are too few for the phones, each gets an equal share instead.
pub fn align(phones: &[Phone], frames: &[VoiceFrame]) -> Vec<PhoneSpan> {
    // A silence may fall before, after and between any two phones, whether the text marks a pause there or not.
    let mut seq = Vec::with_capacity(2 * phones.len() + 1);
    seq.push(Phone::Silence);
    for &phone in phones.iter().filter(|p| **p != Phone::Silence) {
        seq.push(phone);
        seq.push(Phone::Silence);
    }
    let (n, f) = (seq.len(), frames.len());
    // Prefix sums of each phone's cost over the frames.
    let mut pre = vec![0.0f32; n * (f + 1)];
    for (p, &phone) in seq.iter().enumerate() {
        for (k, frame) in frames.iter().enumerate() {
            pre[p * (f + 1) + k + 1] = pre[p * (f + 1) + k] + cost(phone, frame);
        }
    }
    // best[p][k]: the least cost of the first p phones over the first k frames; from[p][k]: the run the p-th took.
    let inf = f32::INFINITY;
    let mut best = vec![inf; (n + 1) * (f + 1)];
    let mut from = vec![0u16; (n + 1) * (f + 1)];
    best[0] = 0.0;
    for p in 0..n {
        let (lo, hi) = bounds(seq[p]);
        for k in 0..=f {
            let here = best[p * (f + 1) + k];
            if !here.is_finite() {
                continue;
            }
            for d in lo..=hi.min(f - k) {
                let c = here + pre[p * (f + 1) + k + d] - pre[p * (f + 1) + k];
                let slot = (p + 1) * (f + 1) + k + d;
                if c < best[slot] {
                    best[slot] = c;
                    #[allow(clippy::cast_possible_truncation)] // runs are at most 300 frames
                    let run = d as u16;
                    from[slot] = run;
                }
            }
        }
    }
    let mut runs = vec![0usize; n];
    if best[n * (f + 1) + f].is_finite() {
        let mut k = f;
        for p in (0..n).rev() {
            let d = usize::from(from[(p + 1) * (f + 1) + k]);
            runs[p] = d;
            k -= d;
        }
    } else {
        // Too few frames: an equal share each for the phones, none for the silences.
        let count = n / 2;
        for (i, run) in runs.iter_mut().skip(1).step_by(2).enumerate() {
            *run = f * (i + 1) / count.max(1) - f * i / count.max(1);
        }
    }
    let mut spans = Vec::with_capacity(phones.len());
    let mut k = 0;
    for (p, &phone) in seq.iter().enumerate() {
        let run = runs[p];
        if run > 0 && !(phone == Phone::Silence && (p == 0 || p == n - 1)) {
            spans.push(PhoneSpan { phone, start_s: boundary(k), end_s: boundary(k + run) });
        }
        k += run;
    }
    spans
}
