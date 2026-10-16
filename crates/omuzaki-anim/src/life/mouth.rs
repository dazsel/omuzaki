//! Mouth: springs on the channels the prototype sprang (jaw, smile, stretch, round, pucker), mouth-corner noise, and
//! the smile that flushes the cheeks.

use omuzaki_anim_api::Channel;

use super::bank::Bank;
use super::frame::{Frame, Side};
use super::noise::Texture;
use super::spring::Spring;
use super::write::Out;

const MOUTH: [(Channel, Side); 11] = [
    (Channel::JAW_OPEN, Side::Centre),
    (Channel::MOUTH_SMILE_LEFT, Side::Left),
    (Channel::MOUTH_SMILE_RIGHT, Side::Right),
    (Channel::MOUTH_FROWN_LEFT, Side::Left),
    (Channel::MOUTH_FROWN_RIGHT, Side::Right),
    (Channel::MOUTH_DIMPLE_LEFT, Side::Left),
    (Channel::MOUTH_DIMPLE_RIGHT, Side::Right),
    (Channel::MOUTH_STRETCH_LEFT, Side::Left),
    (Channel::MOUTH_STRETCH_RIGHT, Side::Right),
    (Channel::MOUTH_FUNNEL, Side::Centre),
    (Channel::MOUTH_PUCKER, Side::Centre),
];
const SMILE: [usize; 2] = [1, 2];
const FROWN: [usize; 2] = [3, 4];
/// How slowly the mouth corners wander, seconds per cycle.
const CORNER_PERIOD_S: f32 = 1.1;

/// What the mouth hands to the eyes: how far each corner smiles after its spring.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(super) struct MouthOut {
    /// Net smile (smile minus frown) of the left and right corner.
    pub smile: [f32; 2],
}

/// Mouth springs and the cheek flush.
#[derive(Clone, Debug)]
pub(super) struct Mouth {
    bank: Bank<11>,
    flush: Spring,
}

impl Mouth {
    pub fn new() -> Self {
        Self { bank: Bank::new(MOUTH), flush: Spring::default() }
    }

    /// Springs run every frame (snapping to the source while a person drives the mouth, so handing back has no jump);
    /// the results are written only while the mouth is free.
    pub fn step(&mut self, frame: &Frame<'_>, out: &mut Out<'_>) -> MouthOut {
        let (params, src) = (frame.params, frame.src);
        let pos = self.bank.track(frame, params.springs.mouth, frame.gate.mouth, |_, c| src[c]);
        let smile = [0, 1].map(|s| pos[SMILE[s]] - pos[FROWN[s]]);
        let result = MouthOut { smile };

        // The flush chases the mean smile even while a person drives the mouth, so it is warm when they let go.
        let flush_target = if frame.systems.coupling {
            params.face.smile_to_blush * 0.5 * (smile[0] + smile[1]).max(0.0)
        } else {
            0.0
        };
        let flush = self.flush.chase(flush_target, params.springs.slow, 1.0, frame.sub);

        if frame.gate.mouth {
            self.bank.write(out, &pos);
            let corners = [
                (Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_FROWN_LEFT, Texture::MouthLeft, Side::Left),
                (Channel::MOUTH_SMILE_RIGHT, Channel::MOUTH_FROWN_RIGHT, Texture::MouthRight, Side::Right),
            ];
            for (smile_channel, frown_channel, texture, side) in corners {
                let wobble = frame.sided_texture(texture, CORNER_PERIOD_S, side);
                out.nudge(smile_channel, frown_channel, frame.ease.mouth * params.face.mouth_noise * wobble);
            }
            if frame.systems.coupling {
                out.put(Channel::BLUSH, (src[Channel::BLUSH] + flush).min(1.0));
            }
        }
        result
    }
}
