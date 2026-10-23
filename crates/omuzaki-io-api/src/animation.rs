//! One humanoid animation, the motion half of the io contract (`docs/design/io-api.md` D11, EX-16).
//!
//! An [`IoAnimation`] is a standalone type, not a field of [`IoScene`](crate::IoScene): a `.vrma` has no mesh, no
//! material and no model meta, so the model's checks and preflight do not apply to it. It is written by an
//! [`AnimationExporter`](crate::AnimationExporter) through [`run_export_animation`](crate::run_export_animation), and a
//! future motion importer yields one.
//!
//! **Space.** VRM normalised space (`docs/design/vrma-export.md` V3): parent-local rotations where identity is the VRM
//! T-pose, glTF axes (+Y up, the model facing +Z), metres, unit quaternions. The rest tree in [`RestHumanoid`] holds
//! only bone *positions*; every rest rotation is identity by definition.
//!
//! **Tracks** pair a target with its value type ([`Track`]), so a rotation on an expression or a translation on an arm
//! cannot be built. [`IoAnimation::check`] holds every other invariant as `anim.*` rules (the table in D11).
//!
//! Like `IoScene`, none of these types derive serde: they hold large float arrays. The op returns the
//! [`AnimationSummary`], which does.

use std::collections::BTreeSet;

use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_store_api::Provenance;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ExpressionName, Finding, HUMAN_BONES, HumanBoneName, PresetExpression, Report};

/// How far a quaternion's length may be from 1 before `anim.rotation_not_unit` fires; the same tolerance as the
/// scene's `scene.rotation_not_unit`.
pub const UNIT_TOLERANCE: f32 = 1e-3;

/// The rest hips must stand higher than this (metres): players divide the hips motion by it (vrma-export.md V4).
pub const MIN_REST_HIPS_HEIGHT: f32 = 1e-3;

/// More custom expression tracks than this do not all play: `UniVRM` reads 100 slots (vrma-export.md V6).
pub const MAX_CUSTOM_EXPRESSIONS: usize = 100;

/// The gaze origin a file gets when the animation names none: the VRM specification's `lookAt` example value.
pub const DEFAULT_LOOK_AT_OFFSET: Vec3 = Vec3::new(0.0, 0.06, 0.0);

/// One humanoid animation, in VRM normalised space. `#[non_exhaustive]`; build with [`IoAnimation::new`] and set the
/// public fields.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct IoAnimation {
    /// Names the glTF animation. Not unique, not an id.
    pub name: String,
    /// The T-pose node tree written with the animation: where each humanoid bone sits, rotations all identity.
    pub rest: RestHumanoid,
    /// The tracks, in any order. An exporter writes them in one canonical order ([`TrackTarget`]'s), so equal
    /// animations give equal bytes whatever order they were built in.
    pub tracks: Vec<Track>,
    /// Gaze origin relative to the head bone (`lookAt.offsetFromHeadBone`); `None` writes
    /// [`DEFAULT_LOOK_AT_OFFSET`].
    pub look_at_offset: Option<Vec3>,
    /// A hint that the last key meets the first (a loop); carried in `asset.extras`, not a VRMA feature.
    pub looped: bool,
    /// What Omuzaki records about the motion's rights (the clip's own record). `None` is refused by
    /// [`run_export_animation`](crate::run_export_animation).
    pub provenance: Option<Provenance>,
}

/// The T-pose hierarchy, as local translations. Rotations are identity by definition: the animation is normalised.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct RestHumanoid {
    /// The bones the character has (never the eyes), each once. Any order; [`RestHumanoid::sorted`] gives the table's.
    pub bones: Vec<RestBone>,
}

/// One bone of the rest tree.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct RestBone {
    /// Which bone.
    pub bone: HumanBoneName,
    /// Metres, relative to the nearest present humanoid ancestor (hips: relative to the file origin, so its y is the
    /// hips height).
    pub translation: Vec3,
}

impl RestBone {
    /// A rest bone.
    pub fn new(bone: HumanBoneName, translation: Vec3) -> Self {
        Self { bone, translation }
    }
}

impl RestHumanoid {
    /// A rest tree of these bones.
    pub fn new(bones: Vec<RestBone>) -> Self {
        Self { bones }
    }

    /// The rest bone for `bone`, if the tree has it.
    pub fn get(&self, bone: HumanBoneName) -> Option<&RestBone> {
        self.bones.iter().find(|b| b.bone == bone)
    }

    /// The bones in [`HUMAN_BONES`] order (parents before children).
    pub fn sorted(&self) -> Vec<RestBone> {
        let mut bones = self.bones.clone();
        bones.sort_by_key(|b| b.bone);
        bones
    }

    /// The nearest ancestor of `bone` in the humanoid table that this tree has; `None` for the hips or when no
    /// ancestor is present.
    pub fn parent_of(&self, bone: HumanBoneName) -> Option<HumanBoneName> {
        let mut parent = bone.spec().parent;
        while let Some(name) = parent {
            let ancestor = HumanBoneName::new(name).ok()?;
            if self.get(ancestor).is_some() {
                return Some(ancestor);
            }
            parent = ancestor.spec().parent;
        }
        None
    }

    /// The bone's rest position in the file's space: its translation plus its present ancestors'.
    pub fn world_position(&self, bone: HumanBoneName) -> Option<Vec3> {
        let mut at = self.get(bone)?.translation;
        let mut current = bone;
        while let Some(parent) = self.parent_of(current) {
            at += self.get(parent)?.translation;
            current = parent;
        }
        Some(at)
    }

    /// The hips' rest height, the `y_src` a player scales hips motion by.
    pub fn hips_height(&self) -> Option<f32> {
        let hips = HumanBoneName::new("hips").ok()?;
        self.get(hips).map(|b| b.translation.y)
    }
}

/// How a track's value changes between keys.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Interpolation {
    /// Straight lines; spherical (shortest arc) for rotations.
    #[default]
    Linear,
    /// Each value holds until the next key.
    Step,
}

/// The keys of one track. `#[non_exhaustive]`; build with [`Keys::linear`] or [`Keys::step`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Keys<T> {
    /// How the value moves between keys.
    pub interpolation: Interpolation,
    /// Seconds from the start, strictly increasing, from 0.
    pub times: Vec<f32>,
    /// One value per time.
    pub values: Vec<T>,
}

impl<T> Keys<T> {
    /// Linear keys.
    pub fn linear(times: Vec<f32>, values: Vec<T>) -> Self {
        Self { interpolation: Interpolation::Linear, times, values }
    }

    /// Step keys: each value holds until the next.
    pub fn step(times: Vec<f32>, values: Vec<T>) -> Self {
        Self { interpolation: Interpolation::Step, times, values }
    }

    /// The last key time, or 0 with no keys.
    pub fn end(&self) -> f32 {
        self.times.last().copied().unwrap_or(0.0)
    }

    /// The number of keys.
    pub fn len(&self) -> usize {
        self.times.len()
    }

    /// No keys.
    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }
}

/// A track's target and its value type are one thing, so a rotation on an expression cannot be built. Build with the
/// constructors ([`Track::rotation`], [`Track::hips`], [`Track::expression`], [`Track::look_at`]).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Track {
    /// Parent-local rotation of a humanoid bone (never an eye: gaze is [`Track::LookAt`]).
    #[non_exhaustive]
    Rotation {
        /// The bone.
        bone: HumanBoneName,
        /// Its rotations.
        keys: Keys<Quat>,
    },
    /// The hips node's local translation, metres, absolute (rest hips plus the pose's offset).
    Hips(Keys<Vec3>),
    /// An expression weight, 0..=1. A preset (not the four `look*` ones) or a custom name.
    #[non_exhaustive]
    Expression {
        /// The expression.
        name: ExpressionName,
        /// Its weights.
        keys: Keys<f32>,
    },
    /// The gaze direction as a rotation (yaw about Y, pitch about X; vrma-export.md V7).
    LookAt(Keys<Quat>),
}

/// What a track drives. Ordered as an exporter writes them: hips, bones in [`HUMAN_BONES`] order, expressions by
/// name ([`ExpressionName`]'s order), look-at.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TrackTarget {
    /// The hips translation.
    Hips,
    /// A bone's rotation.
    Bone(HumanBoneName),
    /// An expression's weight.
    Expression(ExpressionName),
    /// The gaze.
    LookAt,
}

impl Track {
    /// A bone's rotation track.
    pub fn rotation(bone: HumanBoneName, keys: Keys<Quat>) -> Self {
        Self::Rotation { bone, keys }
    }

    /// The hips translation track.
    pub fn hips(keys: Keys<Vec3>) -> Self {
        Self::Hips(keys)
    }

    /// An expression weight track.
    pub fn expression(name: ExpressionName, keys: Keys<f32>) -> Self {
        Self::Expression { name, keys }
    }

    /// The gaze track.
    pub fn look_at(keys: Keys<Quat>) -> Self {
        Self::LookAt(keys)
    }

    /// What it drives.
    pub fn target(&self) -> TrackTarget {
        match self {
            Self::Rotation { bone, .. } => TrackTarget::Bone(*bone),
            Self::Hips(_) => TrackTarget::Hips,
            Self::Expression { name, .. } => TrackTarget::Expression(name.clone()),
            Self::LookAt(_) => TrackTarget::LookAt,
        }
    }

    /// Its key times.
    pub fn times(&self) -> &[f32] {
        match self {
            Self::Rotation { keys, .. } | Self::LookAt(keys) => &keys.times,
            Self::Hips(keys) => &keys.times,
            Self::Expression { keys, .. } => &keys.times,
        }
    }

    /// How its value moves between keys.
    pub fn interpolation(&self) -> Interpolation {
        match self {
            Self::Rotation { keys, .. } | Self::LookAt(keys) => keys.interpolation,
            Self::Hips(keys) => keys.interpolation,
            Self::Expression { keys, .. } => keys.interpolation,
        }
    }

    fn value_count(&self) -> usize {
        match self {
            Self::Rotation { keys, .. } | Self::LookAt(keys) => keys.values.len(),
            Self::Hips(keys) => keys.values.len(),
            Self::Expression { keys, .. } => keys.values.len(),
        }
    }
}

/// What an animation holds, for op output and the export dialog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AnimationSummary {
    /// The animation's name.
    pub name: String,
    /// Seconds, the last key over all tracks.
    pub duration: f32,
    /// Bones in the rest tree.
    pub rest_bones: usize,
    /// Bone rotation tracks.
    pub bone_tracks: usize,
    /// 1 with a hips translation track.
    pub hips_tracks: usize,
    /// Expression weight tracks.
    pub expression_tracks: usize,
    /// 1 with a gaze track.
    pub look_at_tracks: usize,
    /// Keys over all tracks.
    pub keys: usize,
    /// The loop hint.
    pub looped: bool,
}

impl IoAnimation {
    /// An animation with no tracks, no provenance and the default look-at offset.
    pub fn new(name: impl Into<String>, rest: RestHumanoid) -> Self {
        Self { name: name.into(), rest, tracks: Vec::new(), look_at_offset: None, looped: false, provenance: None }
    }

    /// The last key time over all tracks.
    pub fn duration(&self) -> f32 {
        self.tracks.iter().map(|t| t.times().last().copied().unwrap_or(0.0)).fold(0.0, f32::max)
    }

    /// The tracks in canonical order (see [`TrackTarget`]).
    pub fn canonical_tracks(&self) -> Vec<&Track> {
        let mut tracks: Vec<&Track> = self.tracks.iter().collect();
        tracks.sort_by_key(|t| t.target());
        tracks
    }

    /// What it holds.
    pub fn summary(&self) -> AnimationSummary {
        let mut s = AnimationSummary {
            name: self.name.clone(),
            duration: self.duration(),
            rest_bones: self.rest.bones.len(),
            looped: self.looped,
            ..AnimationSummary::default()
        };
        for track in &self.tracks {
            s.keys += track.times().len();
            match track {
                Track::Rotation { .. } => s.bone_tracks += 1,
                Track::Hips(_) => s.hips_tracks += 1,
                Track::Expression { .. } => s.expression_tracks += 1,
                Track::LookAt(_) => s.look_at_tracks += 1,
            }
        }
        s
    }

    /// Every invariant of D11's table, as a report (no side effects). Rules are under `anim.*`; locations are
    /// `rest`, `rest/<bone>` and `tracks/<i>`.
    pub fn check(&self) -> Report {
        let mut r = Report::new();
        check_rest(&self.rest, &mut r);
        if let Some(offset) = self.look_at_offset
            && !offset.is_finite()
        {
            r.push(
                Finding::error("anim.value_non_finite", "the look-at offset holds NaN or infinity")
                    .at("look_at_offset"),
            );
        }
        if self.tracks.is_empty() {
            r.push(Finding::error("anim.no_tracks", "the animation has no tracks").hint("add at least one track"));
        }
        let mut seen: BTreeSet<TrackTarget> = BTreeSet::new();
        let mut custom = 0;
        for (i, track) in self.tracks.iter().enumerate() {
            let at = format!("tracks/{i}");
            if !seen.insert(track.target()) {
                r.push(
                    Finding::error("anim.duplicate_track", format!("{:?} is animated twice", track.target()))
                        .at(at.clone())
                        .hint("glTF uses a node and path once per animation: merge the two tracks"),
                );
            }
            check_times(track, &at, &mut r);
            match track {
                Track::Rotation { bone, keys } => {
                    if matches!(bone.as_str(), "leftEye" | "rightEye") {
                        r.push(
                            Finding::error("anim.eye_bone", format!("{bone} is animated as a bone"))
                                .at(at.clone())
                                .hint("VRM Animation has no eye bones: write the gaze as a LookAt track"),
                        );
                    } else if self.rest.get(*bone).is_none() {
                        r.push(
                            Finding::error("anim.track_without_rest", format!("{bone} has a track but no rest node"))
                                .at(at.clone())
                                .hint("add the bone to the rest tree or drop the track"),
                        );
                    }
                    check_rotations(&keys.values, &at, &mut r);
                }
                Track::LookAt(keys) => check_rotations(&keys.values, &at, &mut r),
                Track::Hips(keys) => {
                    if let Some(k) = keys.values.iter().position(|v| !v.is_finite()) {
                        r.push(non_finite(&at, k));
                    }
                }
                Track::Expression { name, keys } => {
                    if let ExpressionName::Preset(
                        PresetExpression::LookUp
                        | PresetExpression::LookDown
                        | PresetExpression::LookLeft
                        | PresetExpression::LookRight,
                    ) = name
                    {
                        r.push(
                            Finding::error("anim.expression_reserved", format!("{name} cannot be animated"))
                                .at(at.clone())
                                .hint("VRM Animation drives the look presets from the LookAt track"),
                        );
                    }
                    if matches!(name, ExpressionName::Custom(_)) {
                        custom += 1;
                    }
                    if let Some(k) = keys.values.iter().position(|v| !v.is_finite()) {
                        r.push(non_finite(&at, k));
                    } else if let Some(k) = keys.values.iter().position(|v| !(0.0..=1.0).contains(v)) {
                        r.push(
                            Finding::warning(
                                "anim.weight_range",
                                format!("weight {} is outside 0..=1; players clamp it", keys.values[k]),
                            )
                            .at(format!("{at}/values/{k}")),
                        );
                    }
                }
            }
        }
        if custom > MAX_CUSTOM_EXPRESSIONS {
            r.push(Finding::warning(
                "anim.custom_expression_limit",
                format!("{custom} custom expression tracks; some players read only {MAX_CUSTOM_EXPRESSIONS}"),
            ));
        }
        r
    }
}

fn non_finite(at: &str, k: usize) -> Finding {
    Finding::error("anim.value_non_finite", "a value is NaN or infinite").at(format!("{at}/values/{k}"))
}

fn check_times(track: &Track, at: &str, r: &mut Report) {
    let times = track.times();
    if times.is_empty() {
        r.push(Finding::error("anim.no_keys", "the track has no keys").at(at.to_owned()));
        return;
    }
    if times.len() != track.value_count() {
        r.push(
            Finding::error(
                "anim.keys_length_mismatch",
                format!("{} times but {} values", times.len(), track.value_count()),
            )
            .at(at.to_owned()),
        );
    }
    if let Some(k) = times.iter().position(|t| !t.is_finite() || *t < 0.0) {
        r.push(
            Finding::error("anim.time_invalid", format!("time {} is negative or not finite", times[k]))
                .at(format!("{at}/times/{k}")),
        );
    } else if let Some(k) = times.windows(2).position(|w| w[1] <= w[0]) {
        r.push(
            Finding::error("anim.time_order", format!("time {} does not follow {}", times[k + 1], times[k]))
                .at(format!("{at}/times/{}", k + 1))
                .hint("key times must be strictly increasing"),
        );
    }
}

fn check_rotations(values: &[Quat], at: &str, r: &mut Report) {
    if let Some(k) = values.iter().position(|q| !q.is_finite()) {
        r.push(non_finite(at, k));
        return;
    }
    if let Some(k) = values.iter().position(|q| (q.length() - 1.0).abs() > UNIT_TOLERANCE) {
        r.push(
            Finding::error("anim.rotation_not_unit", format!("rotation has length {}", values[k].length()))
                .at(format!("{at}/values/{k}"))
                .hint("normalise the quaternion"),
        );
    }
    if let Some(k) = values.windows(2).position(|w| w[0].dot(w[1]) < 0.0) {
        r.push(
            Finding::warning("anim.quaternion_flip", "two adjacent keys have opposite signs")
                .at(format!("{at}/values/{}", k + 1))
                .hint("negate the later key; players that interpolate componentwise take the long way round"),
        );
    }
}

fn check_rest(rest: &RestHumanoid, r: &mut Report) {
    let mut seen: BTreeSet<HumanBoneName> = BTreeSet::new();
    for b in &rest.bones {
        let at = format!("rest/{}", b.bone);
        if !seen.insert(b.bone) {
            r.push(Finding::error("anim.rest_invalid", format!("{} is in the rest tree twice", b.bone)).at(at.clone()));
        }
        if matches!(b.bone.as_str(), "leftEye" | "rightEye") {
            r.push(
                Finding::error("anim.rest_invalid", format!("{} cannot be in a VRM Animation rest tree", b.bone))
                    .at(at.clone())
                    .hint("the eyes are look-at, not bones"),
            );
        }
        if !b.translation.is_finite() {
            r.push(Finding::error("anim.value_non_finite", "a rest translation is NaN or infinite").at(at.clone()));
        }
        let spec = b.bone.spec();
        if spec.needs_parent
            && let Some(parent) = spec.parent.and_then(|p| HumanBoneName::new(p).ok())
            && rest.get(parent).is_none()
        {
            r.push(
                Finding::error("anim.rest_invalid", format!("{} needs {parent}, which the rest tree lacks", b.bone))
                    .at(at),
            );
        }
    }
    for spec in HUMAN_BONES.iter().filter(|s| s.required) {
        if let Ok(bone) = HumanBoneName::new(spec.name)
            && !seen.contains(&bone)
        {
            r.push(
                Finding::error("anim.rest_invalid", format!("the rest tree lacks the required bone {bone}")).at("rest"),
            );
        }
    }
    if let Some(height) = rest.hips_height()
        && height.partial_cmp(&MIN_REST_HIPS_HEIGHT) != Some(std::cmp::Ordering::Greater)
    {
        r.push(
            Finding::error("anim.rest_hips_height", format!("the rest hips stand at {height} m"))
                .at("rest/hips")
                .hint("players scale hips motion by this height, so it must be the character's standing hips height"),
        );
    }
}
