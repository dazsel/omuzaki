//! Brows: springs, a kick on each stressed syllable, and independent wobble per side.

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::rng::Rng;

use super::bank::Bank;
use super::frame::{Frame, Side};
use super::noise::Texture;
use super::write::Out;

const BROWS: [(Channel, Side); 5] = [
    (Channel::BROW_INNER_UP, Side::Centre),
    (Channel::BROW_OUTER_UP_LEFT, Side::Left),
    (Channel::BROW_OUTER_UP_RIGHT, Side::Right),
    (Channel::BROW_DOWN_LEFT, Side::Left),
    (Channel::BROW_DOWN_RIGHT, Side::Right),
];
/// The first three channels raise the brow; a kick adds to them.
const RAISERS: usize = 3;
/// How slowly the brows wander, seconds per cycle.
const WOBBLE_PERIOD_S: f32 = 1.4;

/// How high the brows are held, `-1..=1`: raised (inner or outer) minus lowered. The painter derives the real shape;
/// the life layer only needs "are the brows up" for the surprise look.
pub(super) fn height(src: &ChannelBuf) -> f32 {
    let raised = src[Channel::BROW_INNER_UP]
        .max(f32::midpoint(src[Channel::BROW_OUTER_UP_LEFT], src[Channel::BROW_OUTER_UP_RIGHT]));
    raised - f32::midpoint(src[Channel::BROW_DOWN_LEFT], src[Channel::BROW_DOWN_RIGHT])
}

/// Brow springs and the kick.
#[derive(Clone, Debug)]
pub(super) struct Brows {
    bank: Bank<5>,
    kick: f32,
}

impl Brows {
    pub fn new() -> Self {
        Self { bank: Bank::new(BROWS), kick: 0.0 }
    }

    /// `rng` is the head stream (the kick's jitter).
    pub fn step(&mut self, frame: &Frame<'_>, rng: &mut Rng, out: &mut Out<'_>) {
        self.update_kick(frame, rng);
        let kick = self.kick;
        let src = frame.src;
        let pos = self.bank.track(frame, frame.params.springs.brow, frame.gate.brows, |i, c| {
            src[c] + if i < RAISERS { kick } else { 0.0 }
        });
        if !frame.gate.brows {
            return;
        }
        self.bank.write(out, &pos);
        let amplitude = frame.ease.brows * frame.params.face.brow_noise;
        let sides = [
            (Channel::BROW_OUTER_UP_LEFT, Channel::BROW_DOWN_LEFT, Texture::BrowLeft, Side::Left),
            (Channel::BROW_OUTER_UP_RIGHT, Channel::BROW_DOWN_RIGHT, Texture::BrowRight, Side::Right),
        ];
        for (up, down, texture, side) in sides {
            out.nudge(up, down, amplitude * frame.sided_texture(texture, WOBBLE_PERIOD_S, side));
        }
    }

    /// A stressed syllable raises the brows by `base + per_loud * loudness`, then the kick decays. The brow spring
    /// passes about 70% of it, which lands the visible rise at 0.2 to 0.4.
    fn update_kick(&mut self, frame: &Frame<'_>, rng: &mut Rng) {
        if !frame.gate.brows || !frame.systems.speech {
            self.kick = 0.0;
            return;
        }
        let face = &frame.params.face;
        if frame.onset {
            let strength = face.brow_kick_base + face.brow_kick_per_loud * frame.loud + face.brow_kick_jitter * rng.next_f32();
            self.kick = self.kick.max(strength);
        }
        self.kick *= (-frame.dt / face.brow_kick_decay_s.max(1e-3)).exp();
    }
}
