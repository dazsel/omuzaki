//! The lip-sync evaluation (#2351): a mouth track scored against the phones that were really said, with the measures
//! open-annie used, so a change to the mouth is a number, not an impression.
//!
//! The mouth is sampled every [`STEP_S`] and each sample is read as *shut* (openness under [`SHUT_BELOW`]) or open
//! with its strongest viseme. Against the ground-truth spans:
//!
//! | Measure | What it counts |
//! |---|---|
//! | `vowel_accuracy` | vowel samples shown open with the vowel's own viseme (chance is 20%) |
//! | `vowels_shut` | vowel samples shown shut (lower is better) |
//! | `silence` | samples in a pause shown shut |
//! | `closure_recall` | lip closures (`p b m`) during which the mouth shuts at least once |
//! | `offset_ms` | the lag that best lines the mouth's openness up with the truth's (positive: the mouth is late) |
//!
//! Vowel samples leave out [`EDGE_S`] at each end of a vowel, where any mouth is between shapes, and pause samples
//! leave out [`EDGE_S`] after speech, where any mouth is still closing; closures are given [`EDGE_S`] either side.

use crate::analyzer::{HOP_S, VoiceAnalyzer, VoiceFrame, WINDOW_S};
use crate::lipsync::{LipSync, LipSyncConfig};
use crate::phoneme::{MouthFrame, Phone, PhoneSpan};
use omuzaki_anim_api::{BlendMode, ChannelBuf, Cues, Layer};
use omuzaki_face_api::expression::ExpressionSet;

/// The sampling step, seconds.
pub const STEP_S: f64 = 0.01;
/// A mouth this little open counts as shut.
pub const SHUT_BELOW: f32 = 0.1;
/// The margin at the edges of a span, seconds.
pub const EDGE_S: f64 = 0.02;
/// The A/V offsets tried, seconds either way.
const MAX_OFFSET_S: f64 = 0.2;

/// The scores of one mouth track (see the module docs). Shares are `0..1`.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct LipSyncScore {
    /// Vowel samples open with the right viseme.
    pub vowel_accuracy: f32,
    /// Vowel samples shown shut.
    pub vowels_shut: f32,
    /// Pause samples shown shut.
    pub silence: f32,
    /// Lip closures during which the mouth shut.
    pub closure_recall: f32,
    /// The lag that best lines the mouth up with the truth, milliseconds; positive is late.
    pub offset_ms: f32,
}

/// The truth's phone at `t`: the span sounding then, else silence.
fn truth_at(truth: &[PhoneSpan], t: f64) -> (Phone, Option<&PhoneSpan>) {
    truth.iter().find(|s| s.start_s <= t && t < s.end_s).map_or((Phone::Silence, None), |s| (s.phone, Some(s)))
}

/// How open the truth is at `t`: a vowel 1, a consonant that isn't a closure a little, a closure or a pause 0.
fn truth_open(truth: &[PhoneSpan], t: f64) -> f32 {
    match truth_at(truth, t).0 {
        Phone::Vowel(_) => 1.0,
        Phone::Consonant | Phone::Round => 0.4,
        Phone::Labiodental => 0.2,
        Phone::Bilabial | Phone::Silence => 0.0,
    }
}

/// Scores `mouth` (the mouth at a time, seconds) over `0..end_s` against `truth`.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // sample counts
pub fn score(truth: &[PhoneSpan], end_s: f64, mouth: &dyn Fn(f64) -> MouthFrame) -> LipSyncScore {
    let samples = (end_s / STEP_S).floor() as usize;
    let times: Vec<f64> = (0..samples).map(|i| i as f64 * STEP_S).collect();
    let frames: Vec<MouthFrame> = times.iter().map(|&t| mouth(t)).collect();
    let shut = |m: &MouthFrame| m.openness() < SHUT_BELOW;
    let (mut vowels, mut right, mut shut_vowels, mut pauses, mut quiet) = (0u32, 0u32, 0u32, 0u32, 0u32);
    for (&t, m) in times.iter().zip(&frames) {
        match truth_at(truth, t) {
            (Phone::Vowel(v), Some(span)) if t >= span.start_s + EDGE_S && t < span.end_s - EDGE_S => {
                vowels += 1;
                if shut(m) {
                    shut_vowels += 1;
                } else if m.viseme() == Some(v) {
                    right += 1;
                }
            }
            (Phone::Silence, _) => {
                let after_speech =
                    truth.iter().any(|s| s.phone != Phone::Silence && s.end_s <= t && t < s.end_s + EDGE_S.max(0.06));
                if !after_speech {
                    pauses += 1;
                    quiet += u32::from(shut(m));
                }
            }
            _ => {}
        }
    }
    let closures: Vec<&PhoneSpan> = truth.iter().filter(|s| s.phone.is_closure()).collect();
    let caught = closures
        .iter()
        .filter(|s| {
            times
                .iter()
                .zip(&frames)
                .any(|(&t, m)| t >= s.start_s - EDGE_S && t < s.end_s + EDGE_S && (shut(m) || m.closure > 0.5))
        })
        .count();
    let share = |a: u32, b: u32| if b == 0 { 0.0 } else { a as f32 / b as f32 };
    LipSyncScore {
        vowel_accuracy: share(right, vowels),
        vowels_shut: share(shut_vowels, vowels),
        silence: share(quiet, pauses),
        closure_recall: share(caught as u32, closures.len() as u32),
        offset_ms: (offset(truth, &times, &frames) * 1000.0) as f32,
    }
}

/// The lag, seconds, that best correlates the mouth's openness with the truth's.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // a few tens of steps
fn offset(truth: &[PhoneSpan], times: &[f64], frames: &[MouthFrame]) -> f64 {
    let want: Vec<f32> = times.iter().map(|&t| truth_open(truth, t)).collect();
    let got: Vec<f32> = frames.iter().map(MouthFrame::openness).collect();
    let max = (MAX_OFFSET_S / STEP_S).round() as usize;
    // The mouth `late` steps behind the truth (or ahead, when not `late`).
    let correlation = |steps: usize, late: bool| {
        let pairs: Vec<(f32, f32)> = if late {
            want.iter().zip(got.iter().skip(steps)).map(|(a, b)| (*a, *b)).collect()
        } else {
            want.iter().skip(steps).zip(&got).map(|(a, b)| (*a, *b)).collect()
        };
        let n = pairs.len().max(1) as f32;
        let (ma, mb) = (pairs.iter().map(|p| p.0).sum::<f32>() / n, pairs.iter().map(|p| p.1).sum::<f32>() / n);
        let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
        for (a, b) in pairs {
            ab += (a - ma) * (b - mb);
            aa += (a - ma) * (a - ma);
            bb += (b - mb) * (b - mb);
        }
        if aa > 0.0 && bb > 0.0 { ab / (aa * bb).sqrt() } else { f32::MIN }
    };
    let lags = (0..=max).map(|s| (s, true)).chain((1..=max).map(|s| (s, false)));
    let (steps, late) =
        lags.max_by(|a, b| correlation(a.0, a.1).total_cmp(&correlation(b.0, b.1))).unwrap_or((0, true));
    let seconds = steps as f64 * STEP_S;
    if late { seconds } else { -seconds }
}

/// The analyser's frames of `pcm` (mono, `rate` Hz), one per hop.
///
/// # Errors
/// The analyser refuses the rate.
pub fn frames_of(pcm: &[f32], rate: u32) -> Result<Vec<VoiceFrame>, crate::VoiceError> {
    let mut analyzer = VoiceAnalyzer::new(rate, crate::VowelProfile::default())?;
    let mut frames = Vec::new();
    analyzer.push(pcm, |f| frames.push(*f));
    Ok(frames)
}

/// The microphone mouth ([`LipSync`] over `expressions`' visemes with `config`) of `frames`, as a mouth track: the
/// frame of each hop is shown from the moment it is ready (the end of its hop), so its latency is part of the score.
pub fn mic_mouth(frames: &[VoiceFrame], expressions: &ExpressionSet, config: LipSyncConfig) -> Vec<MouthFrame> {
    let mut lip = LipSync::new(expressions, config);
    let (rest, mut layer, mut cues) = (ChannelBuf::NEUTRAL, Layer::absent(BlendMode::Max), Cues::default());
    frames
        .iter()
        .map(|frame| {
            lip.take(frame);
            lip.step(HOP_S, &rest, &mut layer, &mut cues);
            MouthFrame { visemes: lip.weights(), closure: lip.closure() }
        })
        .collect()
}

/// A track of one mouth frame per hop, as a mouth at any time: the newest frame whose window has ended by then (the
/// first frame is the first full window, and one follows every hop).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // hop counts
pub fn per_hop(track: &[MouthFrame]) -> impl Fn(f64) -> MouthFrame + '_ {
    move |t| {
        let since = t - f64::from(WINDOW_S);
        if since < 0.0 {
            return MouthFrame::default();
        }
        track.get((since / f64::from(HOP_S)).floor() as usize).copied().unwrap_or_default()
    }
}
