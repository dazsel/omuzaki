//! A pose clip or an emote as an [`IoAnimation`] (`docs/design/vrma-export.md` V3, V4, V7 to V10; `io-api.md` D11).
//!
//! The clip is anything that can be sampled ([`ClipSampler`]): the pose library's clips live in `omuzaki-anim`, which
//! this crate does not depend on, so the caller adapts one (the export op does). Rotations go through unchanged:
//! [`HumanoidPose`] is already VRM normalised space (V3).
//!
//! **Sampling** (V8): `t_k = k / fps` in `f64` for `k = 0..=floor(duration * fps)`; the exact end is added when its
//! `f32` time is later than the last grid time's and replaces it otherwise; times are rounded to `f32` once and then
//! coalesced (a time not strictly after the previous one is dropped, the later sample winning), so the converter never
//! writes two keys at one time.
//!
//! **Tracks:** a bone that turns by more than the tolerance at some sample (or every rest bone with
//! [`BoneSelection::All`]), the hips when the pose moves them, the gaze when either eye turns (the mean of the two eyes'
//! gaze directions, as yaw and pitch, V7), and for an emote its expression weight. Every track keeps a key at 0 and at
//! the end.
//!
//! **Continuity:** the first key of a rotation has its first non-zero component of `(w, x, y, z)` positive, and every
//! later key is negated when its dot with the previous is negative. It changes no rotation.
//!
//! **Reduction** (Ramer-Douglas-Peucker per track, lossless to the [`Tolerance`]): keep the first and last key; keep
//! the interior key with the largest error against the interpolation between the kept neighbours if that error is
//! above the tolerance, and recurse; ties keep the lowest index. The error is the angle for a rotation (against a
//! spherical interpolation, in `f64`), the distance for the hips, the difference for a weight.
//!
//! **Findings** (`anim_export.*`): `bone_not_in_rest` (warning: the clip turns a bone the character lacks; dropped),
//! `eyes_to_look_at` (info), `eye_asymmetry` (warning: the eyes look more than 2 degrees apart; VRM has one gaze),
//! `sampling_error` (info: the largest angle between the clip and its linear playback at 4x the frame rate),
//! `expression_reserved` (warning: an emote's expression is a `look*` preset; dropped), `expression_custom` (info: an
//! `omuzaki_` name plays only on a model that defines it).

use omuzaki_anim_api::{Ease, ExpressionId, HumanoidBone, HumanoidPose, NotHumanoid, Skeleton};
use omuzaki_core::glam::{DQuat, EulerRot, Quat, Vec3};
use omuzaki_io_api::{
    ExpressionName, Finding, HUMAN_BONES, HumanBoneName, IoAnimation, Keys, PresetExpression, Report, RestBone,
    RestHumanoid, Track,
};
use omuzaki_store_api::Provenance;

use super::channels::named_expression;

/// The longest clip the converter takes, seconds (V8).
pub const MAX_DURATION_S: f32 = 600.0;

/// The frame rates the converter takes.
pub const FPS_RANGE: std::ops::RangeInclusive<f32> = 1.0..=120.0;

/// The longest fade an emote takes, seconds.
pub const MAX_FADE_S: f32 = 10.0;

/// Eyes that look further apart than this (degrees) cannot be one VRM gaze (V7).
pub const EYE_ASYMMETRY_DEG: f32 = 2.0;

/// What the converter samples: a clip's length, its loop flag and its pose at a time.
pub trait ClipSampler {
    /// Seconds a play takes.
    fn duration(&self) -> f32;
    /// Whether the clip repeats.
    fn looped(&self) -> bool;
    /// The pose `t` seconds in, `0 <= t <= duration`.
    fn sample(&self, t: f32) -> HumanoidPose;
}

/// Which bones get a track.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum BoneSelection {
    /// The bones the clip turns: an app's own idle keeps the rest.
    #[default]
    Driven,
    /// Every bone of the rest tree (a full-body clip such as a walk).
    All,
}

/// How far the reduced keys may stray from the samples.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Tolerance {
    /// Radians.
    pub rotation_rad: f32,
    /// Metres.
    pub translation_m: f32,
    /// Expression weight.
    pub weight: f32,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self { rotation_rad: 1e-6, translation_m: 1e-6, weight: 1e-6 }
    }
}

/// How a clip is exported.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ClipExportOptions {
    /// Samples a second, 1 to 120 (30 by default: the VRMA guideline, and players interpolate linearly).
    pub fps: f32,
    /// Which bones get a track.
    pub bones: BoneSelection,
    /// The reduction's tolerances.
    pub tolerance: Tolerance,
}

impl Default for ClipExportOptions {
    fn default() -> Self {
        Self { fps: 30.0, bones: BoneSelection::Driven, tolerance: Tolerance::default() }
    }
}

impl ClipExportOptions {
    /// These options at `fps` samples a second (builder style).
    #[must_use]
    pub fn at_fps(mut self, fps: f32) -> Self {
        self.fps = fps;
        self
    }

    /// These options with `bones` (builder style).
    #[must_use]
    pub fn with_bones(mut self, bones: BoneSelection) -> Self {
        self.bones = bones;
        self
    }
}

/// An emote's timing: fade in, hold, fade out (V9).
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct EmoteTiming {
    /// Seconds of each fade (0.2: the expression cue's default).
    pub fade_s: f32,
    /// Seconds at full weight (1.6: reads on a sticker-length loop).
    pub hold_s: f32,
    /// The expression's weight while held, 0 to 1.
    pub intensity: f32,
}

impl Default for EmoteTiming {
    fn default() -> Self {
        Self { fade_s: 0.2, hold_s: 1.6, intensity: 1.0 }
    }
}

impl EmoteTiming {
    /// A fade of `fade_s` seconds each way around a hold of `hold_s` seconds at `intensity`.
    #[must_use]
    pub fn new(fade_s: f32, hold_s: f32, intensity: f32) -> Self {
        Self { fade_s, hold_s, intensity }
    }
}

/// An emote as the converter takes it: plain data, so it does not depend on where emote records live.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct EmoteMotionInput {
    /// The animation's name.
    pub name: String,
    /// The character's expression.
    pub expression: ExpressionId,
    /// The pose held, if any.
    pub pose: Option<HumanoidPose>,
    /// Fades and hold.
    pub timing: EmoteTiming,
}

impl EmoteMotionInput {
    /// An emote with the default timing.
    pub fn new(name: impl Into<String>, expression: ExpressionId, pose: Option<HumanoidPose>) -> Self {
        Self { name: name.into(), expression, pose, timing: EmoteTiming::default() }
    }
}

/// Why a clip or an emote cannot be converted.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ConvertError {
    /// The character's skeleton has no humanoid (`anim_export.not_humanoid`).
    #[error("the character's skeleton is not a VRM humanoid, so its motion cannot be a VRM Animation")]
    NotHumanoid,
    /// The frame rate is outside [`FPS_RANGE`].
    #[error("fps {0} is outside 1 to 120")]
    Fps(f32),
    /// The duration is not above zero or is over [`MAX_DURATION_S`].
    #[error("a duration of {0} s is not above 0 and at most 600 s")]
    Duration(f32),
    /// The rest tree breaks an `anim.*` rule (a skeleton with no standing hips, say).
    #[error("the character's rest tree cannot be written: {0}")]
    Rest(String),
    /// An emote's fades, hold or intensity are out of range.
    #[error("the emote's timing {0}")]
    Timing(String),
}

impl ConvertError {
    /// The rule id the op reports.
    pub fn rule(&self) -> &'static str {
        match self {
            Self::NotHumanoid => "anim_export.not_humanoid",
            Self::Fps(_) => "anim_export.fps",
            Self::Duration(_) => "anim_export.duration",
            Self::Rest(_) => "anim_export.rest",
            Self::Timing(_) => "anim_export.timing",
        }
    }
}

impl From<NotHumanoid> for ConvertError {
    fn from(_: NotHumanoid) -> Self {
        Self::NotHumanoid
    }
}

/// The rest tree of `skeleton` (V4): its humanoid bones but the eyes, in [`HUMAN_BONES`] order, each translation the
/// bone's rest position minus that of its nearest present humanoid ancestor (the hips: its own position).
///
/// # Errors
/// [`NotHumanoid`] for a generic skeleton.
pub fn rest_humanoid(skeleton: &Skeleton) -> Result<RestHumanoid, NotHumanoid> {
    let map = skeleton.require_humanoid()?;
    let mut world = omuzaki_anim_api::Pose::IDENTITY;
    skeleton.world_pose(&skeleton.rest_pose(), &mut world);
    let position = |bone: HumanoidBone| map.get(bone).map(|id| world.local[id.index()].translation);
    let mut bones = Vec::new();
    for spec in HUMAN_BONES.iter().filter(|s| !matches!(s.name, "leftEye" | "rightEye")) {
        let (Ok(name), Some(bone)) = (HumanBoneName::new(spec.name), HumanoidBone::from_name(spec.name)) else {
            continue;
        };
        let Some(at) = position(bone) else { continue };
        let mut parent = spec.parent;
        let mut base = Vec3::ZERO;
        while let Some(p) = parent {
            if let Some(found) = HumanoidBone::from_name(p).and_then(position) {
                base = found;
                break;
            }
            parent = HUMAN_BONES.iter().find(|s| s.name == p).and_then(|s| s.parent);
        }
        bones.push(RestBone::new(name, at - base));
    }
    Ok(RestHumanoid::new(bones))
}

/// The sample times of V8, as `f32`, strictly increasing, from 0 to `duration`.
fn sample_times(duration: f32, fps: f32) -> Vec<f32> {
    let (d, rate) = (f64::from(duration), f64::from(fps));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "at most 72,000 samples, checked")]
    let last = (d * rate).floor() as u64;
    let mut times: Vec<f32> = Vec::with_capacity(usize::try_from(last).unwrap_or(0) + 2);
    for k in 0..=last {
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, reason = "the one rounding to f32")]
        let t = (k as f64 / rate) as f32;
        if times.last().is_none_or(|prev| t > *prev) {
            times.push(t);
        }
    }
    match times.last_mut() {
        Some(last) if duration > *last => times.push(duration),
        Some(last) => *last = duration,
        None => times.push(duration),
    }
    times
}

/// The angle between two rotations, radians, precise for small angles.
fn angle(a: Quat, b: Quat) -> f64 {
    let (a, b) = (DQuat::from_array(a.to_array().map(f64::from)), DQuat::from_array(b.to_array().map(f64::from)));
    let d = a.conjugate() * b;
    let v = (d.x * d.x + d.y * d.y + d.z * d.z).sqrt();
    2.0 * v.atan2(d.w.abs())
}

/// Spherical interpolation in `f64`, shortest arc.
fn slerp(a: Quat, b: Quat, u: f64) -> Quat {
    let a = DQuat::from_array(a.to_array().map(f64::from));
    let mut b = DQuat::from_array(b.to_array().map(f64::from));
    let mut dot = a.dot(b);
    if dot < 0.0 {
        b = -b;
        dot = -dot;
    }
    let q = if dot > 1.0 - 1e-12 {
        (a * (1.0 - u) + b * u).normalize()
    } else {
        let theta = dot.clamp(-1.0, 1.0).acos();
        let s = theta.sin();
        a * (((1.0 - u) * theta).sin() / s) + b * ((u * theta).sin() / s)
    };
    #[allow(clippy::cast_possible_truncation, reason = "back to the file's precision")]
    Quat::from_array(q.to_array().map(|c| c as f32))
}

/// Something a track holds: how to interpolate it and how far two values are apart.
trait KeyValue: Copy {
    fn between(a: Self, b: Self, u: f64) -> Self;
    fn distance(a: Self, b: Self) -> f64;
}

impl KeyValue for Quat {
    fn between(a: Self, b: Self, u: f64) -> Self {
        slerp(a, b, u)
    }
    fn distance(a: Self, b: Self) -> f64 {
        angle(a, b)
    }
}

impl KeyValue for Vec3 {
    #[allow(clippy::cast_possible_truncation, reason = "back to the file's precision")]
    fn between(a: Self, b: Self, u: f64) -> Self {
        a.lerp(b, u as f32)
    }
    fn distance(a: Self, b: Self) -> f64 {
        f64::from(a.distance(b))
    }
}

impl KeyValue for f32 {
    #[allow(clippy::cast_possible_truncation, reason = "back to the file's precision")]
    fn between(a: Self, b: Self, u: f64) -> Self {
        (f64::from(a) + (f64::from(b) - f64::from(a)) * u) as f32
    }
    fn distance(a: Self, b: Self) -> f64 {
        (f64::from(a) - f64::from(b)).abs()
    }
}

/// Ramer-Douglas-Peucker: the indices of the keys to keep (first and last always).
fn reduce<T: KeyValue>(times: &[f32], values: &[T], tolerance: f32) -> Vec<usize> {
    let n = times.len();
    if n <= 2 {
        return (0..n).collect();
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut spans = vec![(0, n - 1)];
    while let Some((a, b)) = spans.pop() {
        let (ta, tb) = (f64::from(times[a]), f64::from(times[b]));
        let mut worst: Option<(usize, f64)> = None;
        for i in a + 1..b {
            let u = (f64::from(times[i]) - ta) / (tb - ta);
            let e = T::distance(values[i], T::between(values[a], values[b], u));
            if worst.is_none_or(|(_, w)| e > w) {
                worst = Some((i, e));
            }
        }
        if let Some((i, e)) = worst
            && e > f64::from(tolerance)
        {
            keep[i] = true;
            spans.push((i, b));
            spans.push((a, i));
        }
    }
    (0..n).filter(|i| keep[*i]).collect()
}

/// The reduced keys of one track.
fn reduced<T: KeyValue>(times: &[f32], values: &[T], tolerance: f32) -> Keys<T> {
    let kept = reduce(times, values, tolerance);
    Keys::linear(kept.iter().map(|&i| times[i]).collect(), kept.iter().map(|&i| values[i]).collect())
}

/// Sign continuity (V8 step 3).
fn continuous(values: &mut [Quat]) {
    if let Some(first) = values.first_mut() {
        let lead = [first.w, first.x, first.y, first.z].into_iter().find(|c| *c != 0.0).unwrap_or(1.0);
        if lead < 0.0 {
            *first = -*first;
        }
    }
    for i in 1..values.len() {
        if values[i - 1].dot(values[i]) < 0.0 {
            values[i] = -values[i];
        }
    }
}

/// The value of linear `keys` at `t` (clamped to the ends).
fn play<T: KeyValue>(keys: &Keys<T>, t: f32) -> Option<T> {
    let (first, last) = (*keys.values.first()?, *keys.values.last()?);
    let i = keys.times.partition_point(|k| *k <= t);
    if i == 0 {
        return Some(first);
    }
    if i >= keys.times.len() {
        return Some(last);
    }
    let (ta, tb) = (f64::from(keys.times[i - 1]), f64::from(keys.times[i]));
    Some(T::between(keys.values[i - 1], keys.values[i], (f64::from(t) - ta) / (tb - ta)))
}

/// The gaze of a pose's eyes, if either turns: the mean of the two directions as a yaw-and-pitch rotation, and how far
/// apart the eyes look (radians).
fn gaze(pose: &HumanoidPose) -> Option<(Quat, f32)> {
    let (left, right) = (pose.get(HumanoidBone::LeftEye), pose.get(HumanoidBone::RightEye));
    if left == Quat::IDENTITY && right == Quat::IDENTITY {
        return None;
    }
    let (l, r) = (left * Vec3::Z, right * Vec3::Z);
    let d = (l + r).normalize_or(Vec3::Z);
    let yaw = d.x.atan2(d.z);
    let pitch = (-d.y).atan2(d.x.hypot(d.z));
    Some((Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0), l.angle_between(r)))
}

fn check_input(duration: f32, opts: &ClipExportOptions) -> Result<(), ConvertError> {
    if !FPS_RANGE.contains(&opts.fps) {
        return Err(ConvertError::Fps(opts.fps));
    }
    if !(duration.is_finite() && duration > 0.0 && duration <= MAX_DURATION_S) {
        return Err(ConvertError::Duration(duration));
    }
    Ok(())
}

/// The animation of `clip` on `skeleton`, with its findings. `name` names it; `provenance` is the clip's own record.
///
/// # Errors
/// [`ConvertError`] for a generic skeleton, a frame rate outside 1 to 120 or a duration outside 0 to 600 s (before any
/// sampling), or a rest tree that breaks an `anim.*` rule.
pub fn clip_to_animation(
    clip: &dyn ClipSampler,
    skeleton: &Skeleton,
    name: &str,
    provenance: Provenance,
    opts: &ClipExportOptions,
) -> Result<(IoAnimation, Report), ConvertError> {
    convert(clip, skeleton, name, provenance, opts, None)
}

/// An expression envelope to add to a clip's tracks.
struct Envelope {
    name: ExpressionName,
    weight: Box<dyn Fn(f32) -> f32>,
}

fn convert(
    clip: &dyn ClipSampler,
    skeleton: &Skeleton,
    name: &str,
    provenance: Provenance,
    opts: &ClipExportOptions,
    envelope: Option<Envelope>,
) -> Result<(IoAnimation, Report), ConvertError> {
    skeleton.require_humanoid()?;
    let duration = clip.duration();
    check_input(duration, opts)?;
    let rest = rest_humanoid(skeleton)?;
    let mut report = Report::new();
    let times = sample_times(duration, opts.fps);
    let poses: Vec<HumanoidPose> = times.iter().map(|t| clip.sample(*t)).collect();
    let tol = opts.tolerance;
    let mut animation = IoAnimation::new(name, rest);
    animation.looped = clip.looped();
    animation.provenance = Some(provenance);

    // The hips, when the pose moves them.
    let rest_hips = animation.rest.bones.first().map_or(Vec3::ZERO, |b| b.translation);
    if poses.iter().any(|p| p.hips_offset.length() > tol.translation_m) {
        let values: Vec<Vec3> = poses.iter().map(|p| rest_hips + p.hips_offset).collect();
        animation.tracks.push(Track::hips(reduced(&times, &values, tol.translation_m)));
    }
    // The bones.
    let mut rotation_tracks: Vec<(HumanoidBone, Keys<Quat>)> = Vec::new();
    for bone in HumanoidBone::ALL {
        if matches!(bone, HumanoidBone::LeftEye | HumanoidBone::RightEye) {
            continue;
        }
        let Ok(file) = HumanBoneName::new(bone.name()) else { continue };
        let turns = poses.iter().any(|p| angle(p.get(bone), Quat::IDENTITY) > f64::from(tol.rotation_rad));
        let in_rest = animation.rest.get(file).is_some();
        if !in_rest {
            if turns {
                report.push(
                    Finding::warning(
                        "anim_export.bone_not_in_rest",
                        format!("the clip turns {bone}, which this character does not have; that motion is dropped"),
                    )
                    .at(format!("tracks/{bone}")),
                );
            }
            continue;
        }
        if !turns && opts.bones == BoneSelection::Driven {
            continue;
        }
        let mut values: Vec<Quat> = poses.iter().map(|p| p.get(bone).normalize()).collect();
        continuous(&mut values);
        let keys = reduced(&times, &values, tol.rotation_rad);
        rotation_tracks.push((bone, keys.clone()));
        animation.tracks.push(Track::rotation(file, keys));
    }
    // The gaze.
    let gazes: Vec<Option<(Quat, f32)>> = poses.iter().map(gaze).collect();
    if gazes.iter().any(Option::is_some) {
        report.push(Finding::info("anim_export.eyes_to_look_at", "the eye bones are written as one gaze (look-at)"));
        let spread = gazes.iter().flatten().map(|(_, s)| *s).fold(0.0, f32::max);
        if spread.to_degrees() > EYE_ASYMMETRY_DEG {
            report.push(Finding::warning(
                "anim_export.eye_asymmetry",
                format!(
                    "the eyes look up to {:.1} degrees apart; VRM Animation has one gaze for both, their mean",
                    spread.to_degrees()
                ),
            ));
        }
        let mut values: Vec<Quat> = gazes.iter().map(|g| g.map_or(Quat::IDENTITY, |(q, _)| q)).collect();
        continuous(&mut values);
        animation.tracks.push(Track::look_at(reduced(&times, &values, tol.rotation_rad)));
    }
    // The expression.
    if let Some(envelope) = envelope {
        let values: Vec<f32> = times.iter().map(|t| (envelope.weight)(*t).clamp(0.0, 1.0)).collect();
        animation.tracks.push(Track::expression(envelope.name, reduced(&times, &values, tol.weight)));
    }
    // What linear playback costs, measured at four times the frame rate.
    let mut worst = 0.0_f64;
    let fine = sample_times(duration, opts.fps * 4.0);
    for t in &fine {
        let exact = clip.sample(*t);
        for (bone, keys) in &rotation_tracks {
            if let Some(played) = play(keys, *t) {
                worst = worst.max(angle(exact.get(*bone).normalize(), played));
            }
        }
    }
    if !rotation_tracks.is_empty() {
        report.push(Finding::info(
            "anim_export.sampling_error",
            format!(
                "at {} fps, linear playback strays at most {:.4} degrees from the clip",
                opts.fps,
                worst.to_degrees()
            ),
        ));
    }
    let rest_report = animation.check();
    if let Some(f) = rest_report.errors().find(|f| f.rule.starts_with("anim.rest")) {
        return Err(ConvertError::Rest(f.message.clone()));
    }
    Ok((animation, report))
}

/// An emote's pose hold: REST, eased to the pose over the fade, held, eased back.
struct Hold {
    pose: HumanoidPose,
    timing: EmoteTiming,
}

/// `0 -> 1 -> 0` over the emote's fades and hold, eased with [`Ease::OutCubic`].
fn envelope_at(timing: EmoteTiming, t: f32) -> f32 {
    let EmoteTiming { fade_s, hold_s, .. } = timing;
    let total = 2.0 * fade_s + hold_s;
    if t <= 0.0 || t >= total {
        0.0
    } else if t < fade_s {
        Ease::OutCubic.apply(t / fade_s)
    } else if t <= fade_s + hold_s {
        1.0
    } else {
        Ease::OutCubic.apply((total - t) / fade_s)
    }
}

impl ClipSampler for Hold {
    fn duration(&self) -> f32 {
        2.0 * self.timing.fade_s + self.timing.hold_s
    }
    fn looped(&self) -> bool {
        false
    }
    fn sample(&self, t: f32) -> HumanoidPose {
        HumanoidPose::REST.lerp(&self.pose, envelope_at(self.timing, t))
    }
}

/// The animation of an emote (V9): the pose, when there is one, eased in over `fade_s`, held for `hold_s` and eased
/// out, through the clip path; and one weight track `0 -> intensity -> 0` with the same fades and ease, named by
/// [`named_expression`] so the name is the VRM export's.
///
/// # Errors
/// As [`clip_to_animation`].
pub fn emote_to_animation(
    input: &EmoteMotionInput,
    skeleton: &Skeleton,
    provenance: Provenance,
    opts: &ClipExportOptions,
) -> Result<(IoAnimation, Report), ConvertError> {
    let timing = input.timing;
    let EmoteTiming { fade_s, hold_s, intensity } = timing;
    if !(fade_s.is_finite() && fade_s > 0.0 && fade_s <= MAX_FADE_S) {
        return Err(ConvertError::Timing(format!("has a fade of {fade_s} s: give above 0 and at most {MAX_FADE_S} s")));
    }
    if !(hold_s.is_finite() && hold_s >= 0.0 && 2.0 * fade_s + hold_s <= MAX_DURATION_S) {
        return Err(ConvertError::Timing(format!(
            "has a hold of {hold_s} s: give 0 or more, {MAX_DURATION_S} s in all"
        )));
    }
    if !(intensity.is_finite() && (0.0..=1.0).contains(&intensity)) {
        return Err(ConvertError::Timing(format!("has an intensity of {intensity}: give 0 to 1")));
    }
    let hold = Hold { pose: input.pose.unwrap_or(HumanoidPose::REST), timing };
    let mut report = Report::new();
    let envelope = match named_expression(&input.expression) {
        Some(ExpressionName::Preset(
            PresetExpression::LookUp
            | PresetExpression::LookDown
            | PresetExpression::LookLeft
            | PresetExpression::LookRight,
        )) => {
            report.push(
                Finding::warning(
                    "anim_export.expression_reserved",
                    format!(
                        "{} is a look preset, which a VRM Animation drives from the gaze; dropped",
                        input.expression
                    ),
                )
                .at("expression"),
            );
            None
        }
        Some(name) => {
            if matches!(name, ExpressionName::Custom(_)) {
                report.push(
                    Finding::info(
                        "anim_export.expression_custom",
                        format!("{name} plays only on a model that defines it; other apps ignore it"),
                    )
                    .at("expression"),
                );
            }
            Some(Envelope { name, weight: Box::new(move |t| intensity * envelope_at(timing, t)) })
        }
        None => None,
    };
    let (animation, converted) = convert(&hold, skeleton, &input.name, provenance, opts, envelope)?;
    report.extend(converted);
    Ok((animation, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_times_end_exactly_and_never_repeat() {
        assert_eq!(sample_times(1.0, 30.0).len(), 31);
        let t = sample_times(0.1, 30.0);
        assert_eq!(t.len(), 4, "0, 1/30, 2/30 and 0.1: {t:?}");
        assert_eq!(*t.last().unwrap_or(&0.0), 0.1);
        for (d, fps) in [(0.1, 30.0), (1.0 / 30.0, 30.0), (1.0 / 120.0, 30.0), (0.1, 120.0), (1.0 / 120.0, 120.0)] {
            let t = sample_times(d, fps);
            assert!(t.windows(2).all(|w| w[0] < w[1]), "{d} at {fps}: {t:?}");
            assert_eq!((t[0], *t.last().unwrap_or(&0.0)), (0.0, d));
        }
    }

    #[test]
    fn reduction_keeps_ends_and_corners_and_drops_lines() {
        let times = [0.0, 1.0, 2.0, 3.0, 4.0];
        assert_eq!(reduce(&times, &[0.0, 1.0, 2.0, 3.0, 4.0], 1e-6), [0, 4]);
        assert_eq!(reduce(&times, &[0.0, 1.0, 2.0, 1.0, 0.0], 1e-6), [0, 2, 4]);
        assert_eq!(reduce(&times, &[0.0, 0.0, 0.0, 0.0, 0.0], 1e-6), [0, 4]);
    }

    #[test]
    fn continuity_negates_only_what_flips() {
        let q = Quat::from_rotation_y(0.2);
        let mut v = [-Quat::IDENTITY, -q, q];
        continuous(&mut v);
        assert_eq!(v, [Quat::IDENTITY, q, q]);
    }

    #[test]
    fn small_angles_are_measured_precisely() {
        let a = angle(Quat::IDENTITY, Quat::from_rotation_x(2e-6));
        assert!((a - 2e-6).abs() < 2e-8, "{a}");
    }

    #[test]
    fn a_gaze_to_the_left_and_down_is_a_positive_yaw_and_pitch() {
        let mut pose = HumanoidPose::REST;
        let q = Quat::from_euler(EulerRot::YXZ, 0.3, 0.2, 0.0);
        pose.set(HumanoidBone::LeftEye, q);
        pose.set(HumanoidBone::RightEye, q);
        let (g, spread) = gaze(&pose).unwrap_or((Quat::IDENTITY, 1.0));
        let (yaw, pitch, _) = g.to_euler(EulerRot::YXZ);
        assert!((yaw - 0.3).abs() < 1e-5 && (pitch - 0.2).abs() < 1e-5 && spread < 1e-5, "{yaw} {pitch} {spread}");
        let d = g * Vec3::Z;
        assert!(d.x > 0.0 && d.y < 0.0, "toward the model's left (+X) and down: {d}");
    }
}
