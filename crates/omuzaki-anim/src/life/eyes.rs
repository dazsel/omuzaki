//! Eyes: lid springs, the cheek push of a smile, the squint of a laugh, the widening of surprise, and lid drift.
//! (Blinks and ^^ arcs are in `blink`, because they are timed events rather than springs.)

use omuzaki_anim_api::{Channel, ChannelBuf};

use super::bank::Bank;
use super::brows;
use super::frame::{Frame, Side, smoothstep};
use super::laugh::LaughOut;
use super::mouth::MouthOut;
use super::noise::Texture;
use super::write::Out;

// Slot order: widen, squint and cheek, each left then right.
const EYES: [(Channel, Side); 6] = [
    (Channel::EYE_WIDE_LEFT, Side::Left),
    (Channel::EYE_WIDE_RIGHT, Side::Right),
    (Channel::EYE_SQUINT_LEFT, Side::Left),
    (Channel::EYE_SQUINT_RIGHT, Side::Right),
    (Channel::CHEEK_SQUINT_LEFT, Side::Left),
    (Channel::CHEEK_SQUINT_RIGHT, Side::Right),
];
/// A coupling (smile, laugh) raises a lid channel at most to this, but never lowers what the source already shows.
const COUPLED_CAP: f32 = 0.6;
/// The lid drift is noise plus a slow sine at this fraction of its amplitude (noise alone parks at its extremes).
const DRIFT_SINE: f32 = 0.4;
const DRIFT_HZ: f32 = 1.21;
const DRIFT_PERIOD_S: f32 = 1.0;
/// Jaw opening and brow height at which the surprise look starts and is complete (the prototype's ranges).
const SURPRISE_JAW: (f32, f32) = (0.6, 0.9);
const SURPRISE_BROWS: (f32, f32) = (0.3, 0.6);

/// `src` raised by `add`, to at most [`COUPLED_CAP`], and never below `src`.
fn raised(src: f32, add: f32) -> f32 {
    src.max((src + add).min(COUPLED_CAP))
}

/// A big open mouth under raised brows: `0..=1`.
fn surprise(src: &ChannelBuf) -> f32 {
    let ramp = |value: f32, (from, to): (f32, f32)| smoothstep((value - from) / (to - from));
    ramp(src[Channel::JAW_OPEN], SURPRISE_JAW) * ramp(brows::height(src), SURPRISE_BROWS)
}

/// Lid springs.
#[derive(Clone, Debug)]
pub(super) struct Eyes {
    bank: Bank<6>,
}

impl Eyes {
    pub fn new() -> Self {
        Self { bank: Bank::new(EYES) }
    }

    pub fn step(&mut self, frame: &Frame<'_>, mouth: MouthOut, laugh: &LaughOut, out: &mut Out<'_>) {
        let (face, src) = (&frame.params.face, frame.src);
        let coupling = frame.systems.coupling;
        let widen = if coupling { face.surprise_widen * surprise(src) } else { 0.0 };
        let squint = if frame.systems.laugh { face.laugh_squint * laugh.weight } else { 0.0 };
        let pos = self.bank.track(frame, frame.params.springs.eye, frame.gate.eyes, |i, c| {
            let smile = if coupling { face.smile_to_lids * mouth.smile[i % 2].max(0.0) } else { 0.0 };
            match i / 2 {
                0 => (src[c] + widen).min(1.0),
                1 => raised(src[c], squint),
                _ => raised(src[c], smile),
            }
        });
        if !frame.gate.eyes {
            return;
        }
        self.bank.write(out, &pos);
        if frame.systems.texture {
            let phase = frame.traits.drift_phase[2];
            let wobble = frame.noise(Texture::Lid, DRIFT_PERIOD_S, 0.0) + DRIFT_SINE * frame.slow_sine(DRIFT_HZ, phase);
            let drift = face.lid_drift * wobble;
            out.nudge(Channel::EYE_WIDE_LEFT, Channel::EYE_SQUINT_LEFT, drift);
            out.nudge(Channel::EYE_WIDE_RIGHT, Channel::EYE_SQUINT_RIGHT, drift);
        }
    }
}
