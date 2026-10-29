//! Contract for animation and rig (spec v3 §8, §20, §22): everything between "a tracker produced numbers" and "a
//! posed skeleton and a set of face channels go to the renderer".
//!
//! ## The pipeline
//!
//! Spec §8 fixes the order `input → track → retarget → life → anim blend → IK → physics → face paint → render`. This
//! crate defines the data and the traits for the middle of that chain; each arrow is a plain function call on `Copy`,
//! fixed-size data, so a frame allocates nothing:
//!
//! ```text
//!  FrameSource ──▶ Retarget ──▶ ┐
//!  CompiledScore::sample ─────▶ │ LayerStack::resolve ─▶ Bus ─▶ LifeLayer ─▶ (grammar) ─▶ ChannelBuf ─▶ face painter
//!  lip sync / triggers ───────▶ ┘      (rest, score, tracker,        │
//!                                       audio, trigger)              └▶ Animator ─▶ HumanoidPose ─▶ Rig ─▶ Pose ─▶ renderer
//!                                                                                   (IK, SpringSolver)
//! ```
//!
//! | Module | What it fixes |
//! |---|---|
//! | [`channel`] | the channel table, [`ChannelBuf`] (the hot value buffer) and [`ChannelMask`] |
//! | [`frame`] | [`Tick`], the normalised tracker [`TrackFrame`], the [`FrameSource`] input port |
//! | [`retarget`] | [`Calibration`] ranges, [`RetargetConfig`], the [`Retarget`] and [`Calibrator`] traits |
//! | [`playout`] | the A/V delay for the performance stream: [`PlayoutDelay`] and [`DelayRamp`] |
//! | [`blend`] | the blending order ([`LayerStack`]), ownership ([`Ownership`]), hand-off ([`Fade`]) |
//! | [`life`] | the [`LifeLayer`] trait, seeds ([`LifeStreams`]) and every tuning number ([`LifeParams`]) |
//! | [`skeleton`] | VRM humanoid bones, [`Skeleton`] (a humanoid, or generic for creatures), [`HumanoidPose`], [`Pose`], the [`Rig`] and [`Animator`] traits |
//! | [`humanoid_frames`] | [`HumanoidFrames`]: the normalised-pose maths, [`HumanoidPose`] to and from a skeleton's own bone axes |
//! | [`ik`] | [`IkGoals`] (look-at and hand/foot targets) and the [`IkSolver`] trait |
//! | [`spring`] | VRM-compatible spring bones, colliders and the [`SpringSolver`] trait |
//! | [`constraint`] | VRM node constraints (roll, aim, rotation): [`ConstraintSet`], its checks and run order |
//! | [`score`] | [`Score`] (keys per channel and cues) and its allocation-free [`CompiledScore`] |
//!
//! ## Conventions (all stages obey them)
//!
//! - **Hot versus cold.** *Hot* types (`ChannelBuf`, `Layer`, `Bus`, `TrackFrame`, `HumanoidPose`, `Pose`) are
//!   fixed-size, hold no heap data and never allocate; trait methods that run every frame say "hot path" and may not
//!   allocate or lock. *Cold* types (configs, calibrations, scores, skeletons, spring sets) serialise to the project
//!   file, carry `JsonSchema` for ops, and may allocate.
//! - **Units.** Weights `0..1`; angles in **radians**; lengths in metres; time in seconds. UIs may show degrees.
//! - **Axes.** Right-handed, +Y up, a character faces +Z (VRM 1.0), so its left is +X. Head angles are YXZ Euler:
//!   positive yaw turns toward the character's left, positive pitch nods down, positive roll tilts toward its right
//!   shoulder. A performer facing the camera maps to this directly (unmirrored).
//! - **Sides.** `Left`/`Right` are the character's (or performer's) own sides, as in `ARKit`.
//! - **Determinism.** Same seed, same inputs, same output: all randomness is a [`omuzaki_core::rng::Rng`] forked per
//!   subsystem, all time comes from [`Tick`], and nothing reads a clock. [`ChannelBuf::checksum`] and
//!   [`HumanoidPose::checksum`] digest a result so tests can pin it.
//!
//! Real implementations live in `omuzaki-anim` and `omuzaki-rig`; `omuzaki-anim-mock` provides deterministic stand-ins.
#![deny(missing_docs)]

pub mod blend;
pub mod channel;
pub mod constraint;
pub mod frame;
pub mod humanoid_frames;
pub mod ik;
pub mod life;
pub mod playout;
pub mod pose_layer;
pub mod reach;
pub mod retarget;
pub mod score;
pub mod skeleton;
pub mod spring;

pub use blend::{BlendMode, Bus, Fade, Layer, LayerStack, OWN_THRESHOLD, Ownership, Source};
pub use channel::{
    ARKIT_COUNT, CHANNEL_CAPACITY, CHANNELS, CUSTOM_COUNT, Channel, ChannelBuf, ChannelInfo, ChannelMask, Mirror,
    Region, Unit,
};
pub use constraint::{Axis, BoneConstraint, ConstraintError, ConstraintKind, ConstraintSet, SignedAxis};
pub use frame::{DeviceProfile, EyeAngles, FrameSource, HeadPose, NotArkit, Tick, TrackFrame, TrackStatus};
pub use humanoid_frames::HumanoidFrames;
pub use ik::{IkGoals, IkSolver, Limb, LookAt};
pub use life::{
    BlinkParams, Cues, Dynamics, FaceParams, GazeParams, HeadParams, LaughParams, LifeConfig, LifeDynamics, LifeLayer,
    LifeParams, LifeStreams, LifeSystems, LifeTraits, Personality, Span,
};
pub use playout::{DEFAULT_CAPACITY, DelayRamp, MAX_SLEW, PlayoutDelay, PlayoutError};
pub use pose_layer::{BoneMask, PoseLayer, PoseLayers, PoseMode};
pub use reach::{Reach, Reaches};
pub use retarget::{
    Calibration, CalibrationError, CalibrationOutcome, CalibrationPrompt, Calibrator, ChannelRange, EXAGGERATED, FAST,
    LOOK_FULL_RAD, LostPolicy, MIN_FIT_SPAN, MIN_SPAN, OneEuroParams, RangeSource, Response, Retarget, RetargetConfig,
};
pub use score::{
    ChannelTrack, ClipId, CompiledScore, Cue, CueKind, Ease, ExpressionId, Key, LookTarget, Score, ScoreError,
};
pub use skeleton::{
    Animator, BoneDef, BoneId, HUMANOID_COUNT, HumanoidBone, HumanoidMap, HumanoidPose, MAX_BONES, NotHumanoid, Pose,
    Rig, Skeleton, SkeletonError, Transform,
};
pub use spring::{
    Collider, ColliderGroup, ColliderShape, PHYSICS_HZ, PHYSICS_STEP, SpringChain, SpringError, SpringJoint,
    SpringParams, SpringSet, SpringSolver, SpringStats,
};
