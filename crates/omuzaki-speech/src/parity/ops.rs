//! `voice.compare`: our audio against a reference recording of the same words, measured (see [`super`]).
//!
//! | Op | Kind | Scope | Undo |
//! |---|---|---|---|
//! | `voice.compare` | job | `render` | auto (the overlay is a file; the document never changes) |
//!
//! The answer has the shape every track's parity op shares (`track`, `done`, `made_with`, `metrics`, `worst`,
//! `evidence`), with the voice's own `measures`, `floor` and `segments` beside it. The targets are the **two-takes
//! floor** when `takes` are given (other recordings of the reference speaker saying the same line): per measure, the
//! widest any of those takes is from the reference, so ours is held to be no further off than the speaker's own other
//! takes. Without them, documented defaults stand in and the answer says so.
//!
//! **Recreate, never copy.** The reference is only measured against: it must never feed what made ours (no voice
//! cloned or resynthesised from it). The op asks how ours was made (`made_with`), and refuses to score a file that is
//! the reference itself or measures as a near-copy of it.

use omuzaki_ops_api::{CostHint, FsMode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Check, Measures, Parity, analyse, compare, overlay};
use crate::slots::clips::decode_file_16k;

/// Measure our audio against a reference recording of the same words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "ours": "takes/line-1.wav", "reference": "refs/line-1.wav", "made_with": "kokoro-82m/af_heart, speed 1.0" },
    { "ours": "takes/line-1.wav", "reference": "refs/line-1.wav", "takes": ["refs/line-1-take2.wav", "refs/line-1-take3.wav"],
      "made_with": "kokoro-82m/af_heart, speed 0.95, pitch +1", "out": "parity/line-1.png" },
], "x-omuzaki-fixture" = "demo"))]
pub struct VoiceCompare {
    /// Our audio (WAV, FLAC, Ogg Vorbis or MP3), in the project or a granted folder.
    pub ours: String,
    /// The reference recording of the same words.
    pub reference: String,
    /// Other takes of the reference speaker saying the same line: the floor every measure is held to (at most 8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub takes: Vec<String>,
    /// How ours was made (the voice, its settings, any edits). Kept in the answer; the reference must never feed it.
    pub made_with: String,
    /// Where to write the spectrogram overlay (PNG). Nothing is written when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
}

register_op!(VoiceCompare);

/// Targets when no other takes are given: a close match to a careful reading, never the floor itself.
const DEFAULTS: Targets = Targets {
    mcd_db: 4.5,
    spectral_db: 5.0,
    level_db: 1.0,
    f0_bias_cents: 50.0,
    f0_shape_cents: 120.0,
    voicing_error: 0.1,
    duration_pct: 5.0,
    timing_rms_ms: 40.0,
    onset_ms: 40.0,
};

/// A near-copy: every one of these under the line means ours was made from the reference.
const COPY_MCD_DB: f64 = 0.3;
const COPY_CENTS: f64 = 2.0;
const COPY_TIMING_MS: f64 = 1.0;

/// The most takes a floor is measured from.
pub const MAX_TAKES: usize = 8;

/// How many of the worst places the answer lists.
const WORST: usize = 8;

#[derive(Clone, Copy, Debug)]
struct Targets {
    mcd_db: f64,
    spectral_db: f64,
    level_db: f64,
    f0_bias_cents: f64,
    f0_shape_cents: f64,
    voicing_error: f64,
    duration_pct: f64,
    timing_rms_ms: f64,
    onset_ms: f64,
}

impl Targets {
    fn of_floor(floor: &Measures, onset: f64) -> Self {
        Self {
            mcd_db: floor.mcd_db,
            spectral_db: floor.spectral_db,
            level_db: floor.level_db.abs(),
            f0_bias_cents: floor.f0_bias_cents.abs(),
            f0_shape_cents: floor.f0_shape_cents,
            voicing_error: floor.voicing_error,
            duration_pct: (floor.duration_ratio - 1.0).abs() * 100.0,
            timing_rms_ms: floor.timing_rms_ms,
            onset_ms: onset,
        }
    }
}

impl Op for VoiceCompare {
    fn meta() -> OpMeta {
        OpMeta {
            name: "voice.compare",
            summary: "Measure our audio against a reference recording of the same words: aligned frame by frame, it \
                      reports the timbre (mel-cepstral distortion), spectrum, level, pitch offset and intonation (in \
                      cents), voicing, speed and rhythm, each phrase's timing, the worst places with what to change, \
                      and a spectrogram overlay. With a second take of the reference, every target is the two-takes \
                      floor.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // The overlay is a file; the document never changes.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.check_args()?;
        let ours = ctx.fs_access(&self.ours, FsMode::Read)?;
        let reference = ctx.fs_access(&self.reference, FsMode::Read)?;
        if ours == reference {
            return Err(OpError::invalid("ours", "is the reference itself: score a take that was made, not copied"));
        }
        for take in &self.takes {
            ctx.fs_access(take, FsMode::Read)?;
        }
        if let Some(out) = &self.out {
            ctx.fs_access(out, FsMode::Write)?;
        }
        Ok(Plan { cost: Some(CostHint { seconds: Some(2.0), ..CostHint::default() }), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        self.check_args()?;
        job.checkpoint("reading", 0.0)?;
        let read = |field: &str, path: &str| -> Result<Vec<f32>, OpError> {
            let file = job.fs_access(path, FsMode::Read)?.open_read()?;
            let extension = std::path::Path::new(path).extension().and_then(|e| e.to_str());
            decode_file_16k(file, extension).map_err(|e| OpError::invalid(field, format!("doesn't decode: {e}")))
        };
        let ours = read("ours", &self.ours)?;
        let reference = read("reference", &self.reference)?;
        let takes = self.takes.iter().map(|path| read("takes", path)).collect::<Result<Vec<_>, _>>()?;
        let takes: Vec<&[f32]> = takes.iter().map(Vec::as_slice).collect();
        job.checkpoint("measuring", 0.3)?;
        let scored = score(&ours, &reference, &takes)?;
        let mut evidence = Vec::new();
        if let Some(out) = &self.out {
            job.checkpoint("drawing", 0.9)?;
            write_png(job, out, &scored.parity)?;
            evidence.push(json!({
                "kind": "image",
                "path": out,
                "what": "spectrogram overlay: ours warped onto the reference's time, the reference, the difference \
                         (red: ours louder; blue: quieter), and both pitch tracks (white: reference; cyan: ours)",
            }));
        }
        Ok(scored.answer(&self.made_with, &evidence))
    }
}

/// A scored comparison, before it is written up.
#[derive(Debug, Clone)]
pub struct Scored {
    /// Ours against the reference.
    pub parity: Parity,
    /// Each of the reference speaker's other takes against the reference.
    pub floors: Vec<Parity>,
}

impl Scored {
    /// The answer, in the shape every track's parity op shares, with `evidence` listed.
    #[must_use]
    pub fn answer(&self, made_with: &str, evidence: &[Value]) -> Value {
        match widest(&self.floors) {
            Some((floor, onset)) => {
                let targets = Targets::of_floor(&floor, onset);
                let checks = self.parity.within(&floor);
                answer(made_with, &self.parity, &targets, Some(&floor), Some(&checks), evidence)
            }
            None => answer(made_with, &self.parity, &DEFAULTS, None, None, evidence),
        }
    }
}

/// Scores 16 kHz mono `ours` against `reference`, with `takes` (the reference speaker's other takes) as the floor.
/// At most [`MAX_TAKES`] takes. The work of `voice.compare`, without the files.
///
/// # Errors
/// A side with no speech or too long, and ours measuring as a near-copy of the reference.
pub fn score(ours: &[f32], reference: &[f32], takes: &[&[f32]]) -> Result<Scored, OpError> {
    if takes.len() > MAX_TAKES {
        return Err(OpError::invalid("takes", format!("at most {MAX_TAKES} takes")));
    }
    let reference = analyse(reference);
    let parity = compare(&analyse(ours), &reference).map_err(|e| OpError::invalid("ours", e.to_string()))?;
    let m = &parity.measures;
    if m.mcd_db < COPY_MCD_DB && m.f0_rmse_cents < COPY_CENTS && m.timing_rms_ms < COPY_TIMING_MS {
        return Err(OpError::invalid(
            "ours",
            "measures as a near-copy of the reference: the reference is only scored against, never used to make ours \
             (no voice cloned or resynthesised from it)",
        ));
    }
    let floors = takes
        .iter()
        .map(|take| compare(&analyse(take), &reference).map_err(|e| OpError::invalid("takes", e.to_string())))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Scored { parity, floors })
}

/// Per measure, the widest any take is from the reference (a ratio by its distance from 1), with the latest phrase
/// onset; `None` without takes.
fn widest(floors: &[Parity]) -> Option<(Measures, f64)> {
    let first = floors.first()?.measures.clone();
    let wider = |a: f64, b: f64| if b.abs() > a.abs() { b } else { a };
    let measures = floors.iter().map(|f| &f.measures).fold(first, |w, m| Measures {
        mcd_db: w.mcd_db.max(m.mcd_db),
        spectral_db: w.spectral_db.max(m.spectral_db),
        level_db: wider(w.level_db, m.level_db),
        f0_bias_cents: wider(w.f0_bias_cents, m.f0_bias_cents),
        f0_rmse_cents: w.f0_rmse_cents.max(m.f0_rmse_cents),
        f0_shape_cents: w.f0_shape_cents.max(m.f0_shape_cents),
        voicing_error: w.voicing_error.max(m.voicing_error),
        duration_ratio: if m.duration_ratio.ln().abs() > w.duration_ratio.ln().abs() {
            m.duration_ratio
        } else {
            w.duration_ratio
        },
        timing_rms_ms: w.timing_rms_ms.max(m.timing_rms_ms),
        frames: w.frames.min(m.frames),
    });
    let onset = floors.iter().flat_map(|f| &f.segments).map(|s| s.offset_ms[0].abs()).fold(0.0, f64::max);
    Some((measures, onset))
}

impl VoiceCompare {
    fn check_args(&self) -> Result<(), OpError> {
        if self.made_with.trim().is_empty() {
            return Err(OpError::invalid("made_with", "say how ours was made: the voice and its settings"));
        }
        if self.ours == self.reference {
            return Err(OpError::invalid("ours", "is the reference itself: score a take that was made, not copied"));
        }
        if self.out.as_deref().is_some_and(|o| !o.to_ascii_lowercase().ends_with(".png")) {
            return Err(OpError::invalid("out", "the overlay is a PNG: end the path in .png"));
        }
        Ok(())
    }
}

fn write_png(job: &JobCtx<'_>, out: &str, parity: &Parity) -> Result<(), OpError> {
    let image = overlay(parity);
    let picture = image::RgbImage::from_raw(image.width, image.height, image.rgb)
        .ok_or_else(|| OpError::internal("the overlay's size doesn't match its pixels"))?;
    let target = job.fs_access(out, FsMode::Write)?;
    if let Some(parent) = target.parent() {
        parent.create_dir_all()?;
    }
    let mut replacement = target.replace()?;
    picture
        .write_to(&mut std::io::BufWriter::new(replacement.file()), image::ImageFormat::Png)
        .map_err(|e| OpError::internal(format!("writing the overlay: {e}")))?;
    replacement.commit()
}

/// One metric of the shared shape.
fn metric(name: &str, value: f64, unit: &str, target: f64) -> Value {
    json!({
        "name": name,
        "value": round(value),
        "unit": unit,
        "target": round(target),
        "better": "lower",
        "pass": value <= target + 1e-9,
        "scope": "all",
    })
}

fn round(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// The answer, in the shape every track's parity op shares.
fn answer(
    made_with: &str,
    parity: &Parity,
    t: &Targets,
    floor: Option<&Measures>,
    checks: Option<&[Check]>,
    evidence: &[Value],
) -> Value {
    let m = &parity.measures;
    let onset = parity.segments.iter().map(|s| s.offset_ms[0].abs()).fold(0.0, f64::max);
    let metrics = vec![
        metric("mcd_db", m.mcd_db, "dB", t.mcd_db),
        metric("spectral_db", m.spectral_db, "dB", t.spectral_db),
        metric("level_error_db", m.level_db.abs(), "dB", t.level_db),
        metric("f0_bias_cents", m.f0_bias_cents.abs(), "cents", t.f0_bias_cents),
        metric("f0_shape_cents", m.f0_shape_cents, "cents", t.f0_shape_cents),
        metric("voicing_error", m.voicing_error, "share", t.voicing_error),
        metric("duration_error_pct", (m.duration_ratio - 1.0).abs() * 100.0, "%", t.duration_pct),
        metric("timing_rms_ms", m.timing_rms_ms, "ms", t.timing_rms_ms),
        metric("phrase_onset_ms", onset, "ms", t.onset_ms),
    ];
    let done = metrics.iter().all(|x| x["pass"] == true);
    json!({
        "track": "voice",
        "done": done,
        "made_with": made_with,
        "metrics": metrics,
        "worst": worst(parity, t),
        "evidence": evidence,
        "target_source": if floor.is_some() {
            "the takes floor: per measure, the widest the reference speaker's other takes are from the reference"
        } else {
            "defaults: give `takes` to hold every measure to the measured floor"
        },
        "measures": m,
        "floor": floor,
        "within_floor": checks,
        "segments": parity.segments,
    })
}

/// The worst phrases and measures, worst first, each with what to change.
fn worst(parity: &Parity, t: &Targets) -> Vec<Value> {
    let bias = parity.measures.f0_bias_cents;
    let mut items: Vec<(f64, Value)> = Vec::new();
    for (k, s) in parity.segments.iter().enumerate() {
        let scope = format!("phrase {} ({:.2}-{:.2} s)", k + 1, s.reference_ms[0] / 1000.0, s.reference_ms[1] / 1000.0);
        let weight = s.frames.max(1) as f64;
        let late = s.offset_ms[0];
        let candidates = [
            (
                "mcd_db",
                s.mcd_db,
                t.mcd_db,
                "the timbre or the vowels differ here: another voice, or its pitch".to_owned(),
            ),
            (
                "f0_rmse_cents",
                s.f0_rmse_cents,
                t.f0_shape_cents.max(t.f0_bias_cents),
                if bias.abs() > t.f0_bias_cents {
                    format!("the pitch sits {bias:+.0} cents off overall: shift it by {:+.1} semitones", -bias / 100.0)
                } else {
                    "the intonation differs here: the emphasis or the punctuation of the line".to_owned()
                },
            ),
            (
                "voicing_error",
                s.voicing_error,
                t.voicing_error,
                "voiced and unvoiced sounds don't line up: the words or their pronunciation".to_owned(),
            ),
            (
                "phrase_onset_ms",
                late.abs(),
                t.onset_ms,
                format!(
                    "the phrase starts {:.0} ms {} than the reference: the pause before it, or the speed",
                    late.abs(),
                    if late > 0.0 { "later" } else { "earlier" }
                ),
            ),
        ];
        for (metric, value, target, hint) in candidates {
            if value > target + 1e-9 {
                let excess = (value - target) / target.max(1e-6) * weight;
                items.push((
                    excess,
                    json!({ "scope": scope, "metric": metric, "value": round(value), "target": round(target),
                            "hint": hint }),
                ));
            }
        }
    }
    items.extend(whole_line(parity, t));
    let total: f64 = items.iter().map(|(e, _)| e).sum();
    items.sort_by(|a, b| b.0.total_cmp(&a.0));
    items
        .into_iter()
        .take(WORST)
        .map(|(e, mut v)| {
            v["share_of_error"] = json!(round(e / total.max(1e-12)));
            v
        })
        .collect()
}

/// The measures of the whole line that fail, with what to change, weighted like the phrases' entries.
fn whole_line(parity: &Parity, t: &Targets) -> Vec<(f64, Value)> {
    let mut out = Vec::new();
    let m = &parity.measures;
    let all = m.frames.max(1) as f64;
    let ratio = m.duration_ratio;
    let whole = [
        (
            "duration_error_pct",
            (ratio - 1.0).abs() * 100.0,
            t.duration_pct,
            format!(
                "ours is {:.1} % {}: set the speed to {:.2} of what it is",
                (ratio - 1.0).abs() * 100.0,
                if ratio > 1.0 { "longer" } else { "shorter" },
                ratio
            ),
        ),
        (
            "timing_rms_ms",
            m.timing_rms_ms,
            t.timing_rms_ms,
            "the rhythm inside the phrases differs: the length of single words and syllables, not the overall speed"
                .to_owned(),
        ),
        (
            "level_error_db",
            m.level_db.abs(),
            t.level_db,
            format!("ours is {:+.1} dB against the reference: change its gain by {:+.1} dB", m.level_db, -m.level_db),
        ),
        (
            "spectral_db",
            m.spectral_db,
            t.spectral_db,
            "the spectrum's balance differs (brightness, breath, room): the voice's EQ or the recording chain"
                .to_owned(),
        ),
    ];
    for (metric, value, target, hint) in whole {
        if value > target + 1e-9 {
            let excess = (value - target) / target.max(1e-6) * all;
            out.push((
                excess,
                json!({ "scope": "all", "metric": metric, "value": round(value), "target": round(target),
                        "hint": hint }),
            ));
        }
    }
    out
}
