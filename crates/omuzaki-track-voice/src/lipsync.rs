//! [`LipSync`]: voice frames into the performer's **audio layer** and the life layer's [`Cues`] (FE-08, PF-08).
//!
//! The mouth shapes are the character's own viseme expressions (`aa` … `oh`, `omuzaki-face-api`), baked into channel
//! deltas once, so the mouth a voice opens is the mouth the character was designed with, and a VRM export agrees. Each
//! step the five weights are smoothed (a fast attack so the mouth opens on the syllable, a slower release so it does
//! not chatter) and written as `rest + Σ weight × delta` over the mouth channels. The layer stack blends the audio
//! layer with `BlendMode::Max` by default (anim-api), so with a tracker the mouth opens if either the camera or the
//! voice says so, and with no tracker the voice alone moves it.
//!
//! **Closures** (#2351). Lips shut on `p`, `b` and `m`, and a vowel-only mouth never does. The analyser marks a frame
//! that is a closure (a stop's gap in the middle of speech, or a nasal's murmur: `VoiceFrame::closure`); on one the
//! closure weight rises within [`CLOSE_S`] and the vowel weights fall as fast, so the mouth snaps shut, then opens
//! again on the next vowel at the usual attack; while shut the lips press a little ([`PRESS`]).

use omuzaki_anim_api::{Channel, ChannelBuf, ChannelMask, Cues, Layer};
use omuzaki_face_api::expression::{ExpressionId, ExpressionSet, IntensityCurve};

use crate::analyzer::VoiceFrame;
use crate::viseme::Viseme;

/// Seconds for the mouth to cover most (63%) of a close: lips shut faster than a jaw drops.
pub const CLOSE_S: f32 = 0.012;
/// Seconds for the closure to let go once the next sound starts.
const OPEN_S: f32 = 0.03;
/// How far the lips press while shut (`mouthPressLeft` and `mouthPressRight`).
pub const PRESS: f32 = 0.35;

/// How the mouth follows the voice.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct LipSyncConfig {
    /// Seconds for a weight to cover most (63%) of a rise.
    pub attack_s: f32,
    /// Seconds for a weight to cover most (63%) of a fall.
    pub release_s: f32,
    /// Multiplies the weights before smoothing (a quiet speaker's mouth can open fully), clamped to `0..1` after.
    pub gain: f32,
    /// Whether the lips shut on closures (see the module docs).
    pub closure: bool,
}

impl Default for LipSyncConfig {
    /// 30 ms attack, 80 ms release, gain 1.3, closures on.
    fn default() -> Self {
        Self { attack_s: 0.03, release_s: 0.08, gain: 1.3, closure: true }
    }
}

impl LipSyncConfig {
    /// With these attack and release times, seconds (clamped to at least a millisecond).
    #[must_use]
    pub fn with_times(mut self, attack_s: f32, release_s: f32) -> Self {
        self.attack_s = attack_s.max(1e-3);
        self.release_s = release_s.max(1e-3);
        self
    }

    /// With this gain.
    #[must_use]
    pub fn with_gain(mut self, gain: f32) -> Self {
        self.gain = gain.max(0.0);
        self
    }

    /// With closures on or off (off is the vowel-only mouth).
    #[must_use]
    pub fn with_closure(mut self, closure: bool) -> Self {
        self.closure = closure;
        self
    }
}

/// Turns voice frames into the audio layer and the cues. One per performer, on the perform thread.
#[derive(Clone, Debug)]
pub struct LipSync {
    config: LipSyncConfig,
    /// Each viseme's channel deltas at full weight, and its intensity curve.
    deltas: [ChannelBuf; 5],
    curves: [IntensityCurve; 5],
    /// The channels any viseme moves: what the audio layer claims an opinion on.
    mask: ChannelMask,
    /// The newest frame taken.
    voice: VoiceFrame,
    /// The smoothed weights and loudness.
    weights: [f32; 5],
    loudness: f32,
    /// How shut the lips are for a closure, `0..1`.
    closure: f32,
    /// The onset count already reported.
    onsets: u32,
}

impl LipSync {
    /// Lip sync through `expressions`' visemes. A viseme the set lacks moves nothing (the mouth then never takes that
    /// shape). Cold: reads the set once.
    pub fn new(expressions: &ExpressionSet, config: LipSyncConfig) -> Self {
        let mut deltas = [ChannelBuf::NEUTRAL; 5];
        let mut curves = [IntensityCurve::Linear; 5];
        let mut mask = ChannelMask::EMPTY;
        for viseme in Viseme::ALL {
            let Some(expression) = ExpressionId::new(viseme.id()).ok().and_then(|id| expressions.0.get(&id)) else {
                continue;
            };
            curves[viseme.index()] = expression.curve;
            let delta = &mut deltas[viseme.index()];
            for (&channel, &value) in &expression.deltas.0 {
                #[allow(clippy::cast_possible_truncation)] // channel values are small; the bus is f32
                let value = value as f32;
                delta[channel] = value;
                mask.insert(channel);
            }
        }
        // A closure presses the lips of a mouth the visemes move; a set without visemes moves nothing at all.
        if config.closure && !mask.is_empty() {
            mask.insert(Channel::MOUTH_PRESS_LEFT);
            mask.insert(Channel::MOUTH_PRESS_RIGHT);
        }
        Self {
            config,
            deltas,
            curves,
            mask,
            voice: VoiceFrame::default(),
            weights: [0.0; 5],
            loudness: 0.0,
            closure: 0.0,
            onsets: 0,
        }
    }

    /// How shut the lips are for a closure now, `0..1`.
    pub fn closure(&self) -> f32 {
        self.closure
    }

    /// The channels the visemes move.
    pub fn mask(&self) -> ChannelMask {
        self.mask
    }

    /// The smoothed viseme weights, for meters.
    pub fn weights(&self) -> [f32; 5] {
        self.weights
    }

    /// Takes the newest frame, when there is one; until the next, the last one keeps driving the smoothing.
    pub fn take(&mut self, frame: &VoiceFrame) {
        self.voice = *frame;
    }

    /// Advances the smoothing by `dt` seconds and writes the audio layer over `rest` (the character's resting
    /// expression) and the cues. The layer is cleared once the mouth is shut and the voice silent, so it holds no
    /// opinion between sentences. Hot path: allocates nothing.
    pub fn step(&mut self, dt: f32, rest: &ChannelBuf, layer: &mut Layer, cues: &mut Cues) {
        let dt = if dt.is_finite() { dt.clamp(0.0, 0.25) } else { 0.0 };
        let follow = |current: f32, target: f32, config: &LipSyncConfig| {
            let tau = if target > current { config.attack_s } else { config.release_s };
            current + (target - current) * (1.0 - (-dt / tau).exp())
        };
        let shut = self.config.closure && self.voice.closure > 0.5;
        let toward = |current: f32, target: f32, tau: f32| current + (target - current) * (1.0 - (-dt / tau).exp());
        self.closure = toward(self.closure, if shut { 1.0 } else { 0.0 }, if shut { CLOSE_S } else { OPEN_S });
        for (w, target) in self.weights.iter_mut().zip(self.voice.visemes) {
            *w = if shut {
                toward(*w, 0.0, CLOSE_S)
            } else {
                follow(*w, (target * self.config.gain).clamp(0.0, 1.0), &self.config)
            };
        }
        self.loudness = follow(self.loudness, self.voice.loudness, &self.config);

        cues.loudness = self.loudness;
        cues.onset = self.voice.onsets != self.onsets;
        self.onsets = self.voice.onsets;

        let open: f32 = self.weights.iter().sum();
        if open < 1e-3 && !self.voice.speaking && self.closure < 1e-3 {
            layer.clear();
            return;
        }
        layer.values = *rest;
        let shaped: [f32; 5] = std::array::from_fn(|i| self.curves[i].eval(self.weights[i]));
        for channel in self.mask {
            let mut moved: f32 = shaped.iter().zip(&self.deltas).map(|(w, d)| w * d[channel]).sum();
            if matches!(channel, Channel::MOUTH_PRESS_LEFT | Channel::MOUTH_PRESS_RIGHT) {
                moved += PRESS * self.closure;
            }
            layer.values[channel] = (rest[channel] + moved).clamp(channel.info().min, channel.info().max);
        }
        layer.mask = self.mask;
        layer.weight = 1.0;
    }
}

/// The jaw channel, which every viseme opens: what tests and meters read as "how open is the mouth".
pub const JAW: Channel = Channel::JAW_OPEN;

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::BlendMode;

    use super::*;

    fn voiced(aa: f32, onsets: u32) -> VoiceFrame {
        VoiceFrame {
            seq: 1,
            speaking: true,
            voiced: true,
            loudness: 0.8,
            visemes: [aa, 0.0, 0.0, 0.0, 0.0],
            onsets,
            formants: [800.0, 1300.0],
            closure: 0.0,
        }
    }

    #[test]
    fn the_mouth_opens_with_aa_then_closes_and_the_layer_withdraws() {
        let mut lip = LipSync::new(&ExpressionSet::starter(), LipSyncConfig::default());
        assert!(lip.mask().contains(JAW));
        let (rest, mut layer, mut cues) = (ChannelBuf::NEUTRAL, Layer::absent(BlendMode::Max), Cues::default());
        lip.take(&voiced(0.7, 1));
        for _ in 0..30 {
            lip.step(1.0 / 60.0, &rest, &mut layer, &mut cues);
        }
        assert!(layer.is_active());
        assert_eq!(layer.mode, BlendMode::Max, "the stack's mode is kept");
        // starter `aa` opens the jaw by 0.74; 0.7 × gain 1.3 = 0.91 of it.
        assert!((layer.values[JAW] - 0.74 * 0.91).abs() < 0.01, "{}", layer.values[JAW]);
        assert!(cues.loudness > 0.7);
        lip.take(&VoiceFrame { seq: 2, ..VoiceFrame::default() });
        for _ in 0..60 {
            lip.step(1.0 / 60.0, &rest, &mut layer, &mut cues);
        }
        assert!(!layer.is_active(), "silence withdraws the layer");
        assert!(cues.loudness < 0.01);
    }

    #[test]
    fn an_onset_is_a_one_step_cue() {
        let mut lip = LipSync::new(&ExpressionSet::starter(), LipSyncConfig::default());
        let (rest, mut layer, mut cues) = (ChannelBuf::NEUTRAL, Layer::absent(BlendMode::Max), Cues::default());
        lip.take(&voiced(0.5, 3));
        lip.step(0.016, &rest, &mut layer, &mut cues);
        assert!(cues.onset);
        lip.step(0.016, &rest, &mut layer, &mut cues);
        assert!(!cues.onset, "the same count is not a new onset");
    }

    #[test]
    fn a_set_without_visemes_moves_nothing() {
        let mut lip = LipSync::new(&ExpressionSet::default(), LipSyncConfig::default());
        assert!(lip.mask().is_empty());
        let (rest, mut layer, mut cues) = (ChannelBuf::NEUTRAL, Layer::absent(BlendMode::Max), Cues::default());
        lip.take(&voiced(1.0, 0));
        lip.step(0.1, &rest, &mut layer, &mut cues);
        assert!(!layer.is_active());
    }
}
