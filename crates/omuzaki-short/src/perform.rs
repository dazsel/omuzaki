//! The offline perform loop: the live performer (retarget, life layer, anime grammar, animator) stepped at its fixed
//! 60 Hz on the short's clock instead of the wall clock, with the score layer showing each line's face and the audio
//! layer the voice's mouth. No tracker drives it, so the life layer's blinks, saccades, breathing and speech nods give
//! the body its life, as they do for an AI performer.
//!
//! The same timeline and seed give the same frames on every run: the performer's steps are a function of the step
//! index alone.

use omuzaki_anim::BusAnimator;
use omuzaki_anim::grammar::{GrammarConfig, GrammarStage};
use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim_api::{
    BlendMode, ChannelBuf, ChannelMask, Cues, DeviceProfile, HumanoidPose, Layer, LifeConfig, Tick,
};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{ExpressionId, ExpressionSet};
use omuzaki_stage::frame::FrameInput;
use omuzaki_stage::perform::input::NoSource;
use omuzaki_stage::perform::{Performer, PerformerParts, STEP_HZ};
use omuzaki_track_voice::{LipSync, LipSyncConfig};

use crate::error::ShortError;
use crate::timeline::{FaceSpan, Timeline};
use crate::voice::VoiceTrack;

/// A face of the timeline as a score layer: the expressions blended over the resting face, the channels set on top,
/// claiming every channel any of them moves.
#[derive(Clone, Copy, Debug)]
struct ScoreFace {
    values: ChannelBuf,
    mask: ChannelMask,
}

/// The performer and the lip sync, stepped on the short's clock.
pub struct OfflinePerformer {
    performer: Performer,
    lip: LipSync,
    rest: ChannelBuf,
    faces: Vec<ScoreFace>,
    next_step: u64,
    cues: Cues,
    input: FrameInput,
}

impl std::fmt::Debug for OfflinePerformer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfflinePerformer").field("next_step", &self.next_step).finish_non_exhaustive()
    }
}

fn score_face(span: &FaceSpan, expressions: &ExpressionSet, rest: &ChannelBuf) -> Result<ScoreFace, ShortError> {
    let mut ids = Vec::with_capacity(span.ids.len());
    let mut mask = ChannelMask::EMPTY;
    for name in &span.ids {
        let id = ExpressionId::new(name).map_err(|e| ShortError::invalid("face", format!("'{name}': {e}")))?;
        let expression = expressions.0.get(&id).ok_or_else(|| {
            let known: Vec<String> = expressions.0.keys().map(ToString::to_string).collect();
            ShortError::invalid(
                "face",
                format!("the character has no expression '{name}'; it has {}", known.join(", ")),
            )
        })?;
        for channel in expression.deltas.0.keys() {
            mask.insert(*channel);
        }
        ids.push(id);
    }
    let weights: Vec<(&ExpressionId, f32)> = ids.iter().map(|id| (id, 1.0)).collect();
    let mut values = expressions.blend(rest, &weights).map_err(|e| ShortError::invalid("face", e.0.to_string()))?;
    for (channel, value) in &span.channels {
        values[*channel] = value.clamp(channel.info().min, channel.info().max);
        mask.insert(*channel);
    }
    Ok(ScoreFace { values, mask })
}

impl OfflinePerformer {
    /// A performer for a character with `expressions`, resting at `rest` on `base`, with every face of `timeline`
    /// resolved up front, its life seeded by `seed`.
    ///
    /// # Errors
    /// [`ShortError::Invalid`] for a face that names an expression the character lacks.
    pub fn new(
        timeline: &Timeline,
        expressions: &ExpressionSet,
        rest: ChannelBuf,
        base: HumanoidPose,
        seed: u64,
    ) -> Result<Self, ShortError> {
        let faces = timeline.faces.iter().map(|span| score_face(span, expressions, &rest)).collect::<Result<_, _>>()?;
        let parts = PerformerParts {
            source: Box::new(NoSource),
            retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
            life: Box::new(Life::new(LifeConfig::new(seed))),
            animator: Box::new(BusAnimator::default()),
        };
        let mut performer = Performer::new(parts, rest, base);
        if let Ok(grammar) = GrammarStage::new(&GrammarConfig::default()) {
            performer = performer.with_grammar(grammar);
        }
        Ok(Self {
            performer,
            lip: LipSync::new(expressions, LipSyncConfig::default()),
            rest,
            faces,
            next_step: 0,
            cues: Cues::default(),
            input: FrameInput { pose: base, ..FrameInput::default() },
        })
    }

    /// Runs every step due by `t` seconds and returns the frame input of the last one.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn advance(&mut self, timeline: &Timeline, voice: &VoiceTrack, t: f32) -> &FrameInput {
        let due = (f64::from(t) * f64::from(STEP_HZ)).floor().max(0.0) as u64;
        while self.next_step <= due {
            let tick = Tick::at_rate(self.next_step, STEP_HZ);
            let now = tick.time.0 as f32;
            let face = timeline.faces.partition_point(|f| f.start <= now).saturating_sub(1);
            if let Some(face) = self.faces.get(face) {
                let (layer, _) = self.performer.score_mut();
                *layer = Layer { values: face.values, mask: face.mask, weight: 1.0, mode: BlendMode::Override };
            }
            self.lip.take(&voice.frame_at(now));
            self.lip.step(tick.dt, &self.rest, self.performer.audio_layer_mut(), &mut self.cues);
            self.performer.set_cues(self.cues);
            let out = self.performer.step(Tick { time: Seconds(f64::from(now)), ..tick });
            self.input.pose = *out.pose;
            self.input.channels = out.bus.values;
            self.next_step += 1;
        }
        &self.input
    }
}
