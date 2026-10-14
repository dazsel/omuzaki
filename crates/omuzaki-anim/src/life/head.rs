//! Head and body: the soft head springs, speech nods, head noise, the laugh bob, lean, and breathing.

use std::f32::consts::TAU;

use omuzaki_anim_api::Channel;
use omuzaki_core::rng::Rng;

use super::frame::Frame;
use super::laugh::LaughOut;
use super::lead::{Lead, LeadOut, Pose};
use super::noise::Texture;
use super::spring::Spring;
use super::write::Out;

/// A nod decays with this time constant, seconds.
const NOD_DECAY_S: f32 = 0.12;
/// Smoothed loudness (which makes the head sway more while talking) follows the speech with this time constant.
const TALK_TAU_S: f32 = 0.4;
/// The breathing rate wanders by this fraction of itself, slowly.
const BREATH_WANDER: f32 = 0.1;
const BREATH_WANDER_PERIOD_S: f32 = 7.0;
/// The laugh bob swings about its mean by this much (0.7 degrees), and the roll jitters by 0.9 degrees of sine and
/// 0.5 degrees of noise. Radians.
const BOB_SWING_RAD: f32 = 0.012_2;
const ROLL_JITTER_RAD: f32 = 0.015_7;
const ROLL_NOISE_RAD: f32 = 0.008_7;
const ROLL_NOISE_PERIOD_S: f32 = 0.5;
/// Loudness at which a laugh's bob starts to deepen, and the loudness range over which it does.
const BOB_LOUD: (f32, f32) = (0.45, 0.5);
/// Head noise periods per axis (yaw, pitch, roll), seconds.
const NOISE_PERIOD_S: [f32; 3] = [2.2, 1.9, 2.6];
const NOISE_TEXTURE: [Texture; 3] = [Texture::HeadYaw, Texture::HeadPitch, Texture::HeadRoll];

/// The head springs and everything that rides on them.
#[derive(Clone, Debug)]
pub(super) struct Head {
    angles: [Spring; 3],
    lean: Spring,
    lead: Lead,
    nod: f32,
    nod_side: f32,
    talk: f32,
    breath: f32,
}

impl Head {
    /// The breathing phase starts anywhere in the cycle, drawn from `rng` (the head stream).
    pub fn new(rng: &mut Rng) -> Self {
        Self {
            angles: [Spring::default(); 3],
            lean: Spring::default(),
            lead: Lead::new(),
            nod: 0.0,
            nod_side: 1.0,
            talk: 0.0,
            breath: rng.next_f32() * TAU,
        }
    }

    /// Runs the head for one frame and writes it. Returns what the eyes-lead logic decided, for the gaze and blinks.
    pub fn step(&mut self, frame: &Frame<'_>, laugh: &LaughOut, rng: &mut Rng, out: &mut Out<'_>) -> LeadOut {
        let src = frame.src;
        let free = frame.gate.head;
        let source: Pose = [src[Channel::HEAD_YAW], src[Channel::HEAD_PITCH], src[Channel::HEAD_ROLL]];
        let rendered = (self.angles[0].pos(source[0]), self.angles[1].pos(source[1]));
        let lead = self.lead.step(frame, source, rendered, rng);

        let mut target = lead.hold.unwrap_or(source);
        self.nod(frame, &mut target);
        let dynamics = frame.params.springs;
        let sub = frame.sub;
        let mut angles = [0.0; 3];
        for (i, spring) in self.angles.iter_mut().enumerate() {
            angles[i] = spring.track(target[i], free, dynamics.head, 1.0, sub);
        }
        // A person driving the lean channel itself (not the head) also keeps the lean spring on their value.
        let lean_follows = free && !frame.taken.contains(Channel::LEAN);
        let leaning = self.lean.track(src[Channel::LEAN], lean_follows, dynamics.lean, 1.0, sub);

        let rate = 1.0 + BREATH_WANDER * frame.noise(Texture::Breath, BREATH_WANDER_PERIOD_S, 0.0);
        self.breath = (self.breath + TAU * frame.params.head.breath_hz * rate * frame.dt) % TAU;

        if free {
            self.write(frame, angles, laugh, out);
            out.put(Channel::LEAN, leaning);
        }
        if frame.systems.breathing {
            out.put(Channel::BREATH, src[Channel::BREATH].max(0.5 + 0.5 * self.breath.sin()));
        }
        lead
    }

    /// A nod on each stressed syllable, tilting to alternate sides, fed through the soft head spring so it rises and
    /// settles naturally. The smoothed loudness follows the speech either way.
    fn nod(&mut self, frame: &Frame<'_>, target: &mut Pose) {
        let head = &frame.params.head;
        self.talk += (frame.loud - self.talk) * (1.0 - (-frame.dt / TALK_TAU_S).exp());
        if frame.gate.head && frame.systems.speech && frame.onset {
            self.nod = self.nod.max(0.5 + 0.5 * frame.loud);
            self.nod_side = -self.nod_side;
        }
        self.nod *= (-frame.dt / NOD_DECAY_S).exp();
        if frame.gate.head {
            target[1] += head.nod_pitch_rad * self.nod;
            target[2] += head.nod_roll_rad * self.nod * self.nod_side;
        }
    }

    /// The springs' angles plus noise (more while talking), the breath's nod, and the laugh's bob and roll jitter.
    fn write(&self, frame: &Frame<'_>, angles: [f32; 3], laugh: &LaughOut, out: &mut Out<'_>) {
        let head = &frame.params.head;
        let talk = 1.0 + head.talk_gain * self.talk;
        let mut offset = [0.0; 3];
        for (i, delta) in offset.iter_mut().enumerate() {
            *delta = head.noise_rad[i] * talk * frame.texture(NOISE_TEXTURE[i], NOISE_PERIOD_S[i]);
        }
        if frame.systems.breathing {
            offset[1] -= head.breath_pitch_rad * self.breath.sin();
        }
        if laugh.weight > 0.0 {
            let (from, over) = BOB_LOUD;
            let mean = head.laugh_bob_rad.sample(((frame.laugh_loud - from) / over).clamp(0.0, 1.0));
            // Back and up: negative pitch.
            offset[1] -= laugh.weight * (mean + BOB_SWING_RAD * laugh.phase.sin());
            let noise = frame.noise(Texture::HeadRoll, ROLL_NOISE_PERIOD_S, 5.0);
            offset[2] += laugh.weight * (ROLL_JITTER_RAD * (0.5 * laugh.phase + 1.3).sin() + ROLL_NOISE_RAD * noise);
        }
        out.put(Channel::HEAD_YAW, angles[0] + offset[0]);
        out.put(Channel::HEAD_PITCH, angles[1] + offset[1]);
        out.put(Channel::HEAD_ROLL, angles[2] + offset[2]);
    }
}
