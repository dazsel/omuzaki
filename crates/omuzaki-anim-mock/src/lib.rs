//! Deterministic stand-ins for `omuzaki-anim-api` (spec v3 §39 rule 6), so anything that consumes the animation contract
//! builds and tests without a camera, a recording or a real implementation:
//!
//! - [`track`]: a [`RecordedTrack`](track::RecordedTrack), a [`TrackReplay`](track::TrackReplay) that plays it back as a
//!   `FrameSource` bit-identically, and [`synthetic_track`](track::synthetic_track) to make one from a seed.
//! - [`retarget`]: [`MockRetarget`](retarget::MockRetarget) and [`MockCalibrator`](retarget::MockCalibrator).
//! - [`life`]: [`MockLife`](life::MockLife), a seeded, ownership-respecting subset of the life layer.
//! - [`animator`]: [`MockAnimator`](animator::MockAnimator), bus channels to a humanoid pose.
//! - [`fixtures`]: a valid humanoid skeleton, spring set and score, and two generic creatures (a quadruped and a blob).
//! - [`ik`]: [`MockIk`](ik::MockIk), a two-bone limb solver and a head-and-eyes look-at on a humanoid.
//! - [`performer`]: [`MockPerformer`](performer::MockPerformer), all of it wired in the order of spec §8.
//!
//! Determinism follows decision 0009: replays are bit-identical on one machine and toolchain, and tests that compare
//! against numbers recorded elsewhere state a tolerance (channels: 1e-5). The mocks also avoid `sin`, `exp` and `ln`,
//! so in practice they are exact across platforms too.
#![deny(missing_docs)]

pub mod animator;
pub mod fixtures;
pub mod ik;
pub mod life;
pub mod performer;
pub mod retarget;
pub mod track;
