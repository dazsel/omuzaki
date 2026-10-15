//! Eyes: lid springs, the cheek push of a smile, the squint of a laugh, the widening of surprise, and lid drift.
//! (Blinks and ^^ arcs are in `blink`, because they are timed events rather than springs.)

use omuzaki_anim_api::{Channel, ChannelBuf, FaceParams, Span};

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
/// How slowly the lids drift, seconds per cycle.
const DRIFT_PERIOD_S: f32 = 1.0;

/// `src` raised by `add`, to at most `cap` ([`FaceParams::coupled_lid_cap`]), and never below `src`.
fn raised(src: f32, add: f32, cap: f32) -> f32 {
    src.max((src + add).min(cap))
}

/// A big open mouth under raised brows: `0..=1`.
fn surprise(face: &FaceParams, src: &ChannelBuf) -> f32 {
    let ramp = |value: f32, span: Span| smoothstep((value - span.lo) / (span.hi - span.lo));
    ramp(src[Channel::JAW_OPEN], face.surprise_jaw) * ramp(brows::height(src), face.surprise_brows)
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
        let widen = if coupling { face.surprise_widen * surprise(face, src) } else { 0.0 };
        let squint = if frame.systems.laugh { face.laugh_squint * laugh.weight } else { 0.0 };
        let pos = self.bank.track(frame, frame.params.springs.eye, frame.gate.eyes, |i, c| {
            let smile = if coupling { face.smile_to_lids * mouth.smile[i % 2].max(0.0) } else { 0.0 };
            match i / 2 {
                0 => (src[c] + widen).min(1.0),
                1 => raised(src[c], squint, face.coupled_lid_cap),
                _ => raised(src[c], smile, face.coupled_lid_cap),
            }
        });
        if !frame.gate.eyes {
            return;
        }
        self.bank.write(out, &pos);
        if frame.systems.texture {
            let phase = frame.traits.drift_phase[2];
            let wobble = frame.noise(Texture::Lid, DRIFT_PERIOD_S, 0.0) + face.lid_drift_sine * frame.slow_sine(face.lid_drift_hz, phase);
            let drift = face.lid_drift * wobble;
            out.nudge(Channel::EYE_WIDE_LEFT, Channel::EYE_SQUINT_LEFT, drift);
            out.nudge(Channel::EYE_WIDE_RIGHT, Channel::EYE_SQUINT_RIGHT, drift);
        }
    }
}
