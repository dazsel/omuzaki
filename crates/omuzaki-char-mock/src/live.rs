//! The walking skeleton's character system: a fixed pose and a small frame loop that steps the simulations, paints the
//! face and says which face layers changed (spec §8: `... -> physics -> face paint -> render`).

use omuzaki_anim_api::ChannelBuf;
use omuzaki_char_api::{BuiltCharacter, DynamicMesh, JointRef, PoseSource};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{FaceDrawing, FaceError, FaceInput, FaceLayerId, FacePainter, FaceView};

/// A pose that never moves: every known bone at its bind position.
#[derive(Debug, Clone, Copy, Default)]
pub struct RestPose;

impl PoseSource for RestPose {
    fn skin_matrix(&self, joint: &JointRef) -> Option<Mat4> {
        [JointRef::HIPS, JointRef::CHEST, JointRef::NECK, JointRef::HEAD].contains(joint).then_some(Mat4::IDENTITY)
    }

    fn root(&self) -> Mat4 {
        Mat4::IDENTITY
    }
}

/// What one tick did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickReport {
    /// Which face layers changed since the previous tick, in [`FaceLayerId::ALL`] order. The stage repaints only these
    /// layers' textures.
    pub face_changed: [bool; FaceLayerId::COUNT],
    /// The revision of every dynamic mesh, so the stage re-uploads only those that moved.
    pub revisions: Vec<u64>,
}

/// A built character, a face painter and the state a frame loop keeps between ticks.
pub struct MockLive {
    /// The character being driven.
    pub built: BuiltCharacter,
    painter: Box<dyn FacePainter>,
    drawing: FaceDrawing,
    last: Option<[u64; FaceLayerId::COUNT]>,
    clock: f64,
}

impl MockLive {
    /// Drives `built` with `painter` (the caller chooses the painter, so this crate depends on no face implementation).
    pub fn new(built: BuiltCharacter, painter: Box<dyn FacePainter>) -> Self {
        Self { built, painter, drawing: FaceDrawing::new(), last: None, clock: 0.0 }
    }

    /// The face picture as of the last tick.
    pub fn drawing(&self) -> &FaceDrawing {
        &self.drawing
    }

    /// Advances `dt` seconds: steps every simulation under `pose`, then maps `channels` through the character's face
    /// mapping and paints them.
    ///
    /// # Errors
    /// [`FaceError`] if the painter can't paint the frame; the simulations have already stepped.
    pub fn tick(&mut self, dt: f32, pose: &dyn PoseSource, channels: &ChannelBuf) -> Result<TickReport, FaceError> {
        self.clock += f64::from(dt);
        let ctx = omuzaki_char_api::StepCtx::new(dt, Seconds(self.clock), pose, Vec3::ZERO);
        for slot in &mut self.built.runtimes {
            slot.runtime.step(&ctx);
        }
        if let Some(face) = &self.built.face {
            let params = face.profile.mapping.apply(channels);
            let input = FaceInput {
                profile: &face.profile,
                params: &params,
                time: Seconds(self.clock),
                view: FaceView::default(),
            };
            self.painter.paint(&input, &mut self.drawing)?;
        }
        let now = self.drawing.fingerprints();
        let face_changed = std::array::from_fn(|i| self.last.is_none_or(|last| last[i] != now[i]));
        self.last = Some(now);
        let revisions = self
            .built
            .runtimes
            .iter()
            .flat_map(|s| (0..s.runtime.dynamic_count()).filter_map(|i| s.runtime.dynamic_mesh(i)))
            .map(|m: DynamicMesh<'_>| m.revision)
            .collect();
        Ok(TickReport { face_changed, revisions })
    }
}
