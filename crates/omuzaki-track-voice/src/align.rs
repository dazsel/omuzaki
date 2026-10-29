//! Forced alignment (#2351): the times of known phones in a voice, from the analyser's frames alone. A TTS engine knows
//! what it said but Kokoro's graph reports no durations, so the line's phones are laid onto its audio here.
//!
//! Each phone takes a run of 10 ms frames, in order, between a shortest and a longest length for its class; the runs
//! are chosen by dynamic programming to cost least, where a frame's cost says how unlike the phone it sounds: a vowel
//! wants a voiced frame whose formants match its viseme, a closure a stop's quiet gap or a nasal's low murmur, a
//! silence no voice at all. Pauses are optional (a TTS voice often runs words together), and the line may begin and
//! end with silence. Classic signal processing, no model: a 10-second line aligns in a few milliseconds.

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

/// How unlike `phone` the frame sounds, `0` (just like it) up; `peak` is the line's loudest voiced frame.
fn cost(phone: Phone, f: &VoiceFrame, peak: f32) -> f32 {
    let share = |i: usize| {
        let total: f32 = f.visemes.iter().sum();
        if total > 0.0 { f.visemes[i] / total } else { 0.0 }
    };
    let quiet_gap = f.speaking && !f.voiced && f.loudness < 0.12;
    let murmur = f.voiced && f.formants[0] > 0.0 && f.formants[0] < 400.0 && f.loudness < 0.6 * peak;
    let silent = !f.voiced && f.loudness <= 0.0;
    match phone {
        Phone::Vowel(v) if f.voiced => 1.0 - share(v.index()) + if murmur { 0.6 } else { 0.0 },
        Phone::Vowel(_) => 3.0,
        Phone::Bilabial if quiet_gap || murmur => 0.1,
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

/// The time between frames `k - 1` and `k`: the frame's window is centred `WINDOW_S / 2` before its end.
#[allow(clippy::cast_precision_loss)] // frame counts are small
fn boundary(k: usize) -> f64 {
    (f64::from(HOP_S) * (k as f64 + 0.5) - f64::from(WINDOW_S) / 2.0).max(0.0)
}

/// The spans of `phones` in the voice the `frames` were heard from (one per 10 ms hop, from the start), with a silence
/// allowed before and after. When the frames are too few for the phones, each gets an equal share instead.
pub fn align(phones: &[Phone], frames: &[VoiceFrame]) -> Vec<PhoneSpan> {
    let mut seq = Vec::with_capacity(phones.len() + 2);
    seq.push(Phone::Silence);
    seq.extend_from_slice(phones);
    seq.push(Phone::Silence);
    let (n, f) = (seq.len(), frames.len());
    let peak = frames.iter().filter(|fr| fr.voiced).fold(0.0f32, |m, fr| m.max(fr.loudness)).max(0.05);
    // Prefix sums of each phone's cost over the frames.
    let mut pre = vec![0.0f32; n * (f + 1)];
    for (p, &phone) in seq.iter().enumerate() {
        for (k, frame) in frames.iter().enumerate() {
            pre[p * (f + 1) + k + 1] = pre[p * (f + 1) + k] + cost(phone, frame, peak);
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
        // Too few frames: an equal share each for the phones, none for the edge silences.
        let inner = n - 2;
        for (p, run) in runs.iter_mut().enumerate().skip(1).take(inner) {
            *run = f * p / inner.max(1) - f * (p - 1) / inner.max(1);
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
