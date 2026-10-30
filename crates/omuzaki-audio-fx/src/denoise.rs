//! `fx.denoise`: noise suppression on the microphone (issue #2587), `RNNoise` (`nnnoiseless`, BSD-3-Clause) with our
//! framing, mix and VAD gate around it.
//!
//! `RNNoise` is a small recurrent network that predicts a gain for each of 22 bands every 10 ms from the band energies,
//! cepstral features and the pitch, plus a voice-activity probability. It takes out steady noise (fans, hum, air
//! conditioning) and much of the transient kind (keyboards, clicks) while keeping the voice, in well under a
//! millisecond of CPU per frame.
//!
//! **Framing.** `RNNoise` works on 480-sample (10 ms) frames and gives back the frame before the one it was handed (its
//! analysis window spans two frames), so the node delays the signal by exactly [`LATENCY`] = 960 samples (20 ms).
//! Frames are aligned to the graph clock (a frame starts at every multiple of 480), so how the engine cuts its blocks
//! never changes the output.
//!
//! **Parameters.** `amount` mixes the cleaned voice with the (equally delayed) dry one; `gate` adds up to 30 dB more
//! attenuation while `RNNoise`'s own voice probability says nobody is talking, which takes the faint residue between
//! words down to nothing (a gentle gate that knows a voice from a fan, unlike `fx.gate`'s level detector).
//!
//! **Real-time rules.** `process` is allocation-free from the second frame on a thread: the first frame `RNNoise` runs on
//! a new thread builds its FFT plan in a thread-local cache (once per thread, about 50 allocations; the node warms
//! the process-wide tables in `create`). `reset` restores a pristine copy of the network's state kept since `create`,
//! which re-clones its buffers (about 20 allocations); the engine never resets a live node (only the offline
//! [`chain`](crate::chain) does), and `tests/denoise.rs` holds both to these counts.

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo, NodeReport,
    ParamIndex, ParamSpec, Unit,
};
use omuzaki_audio_api::register_node_kind;

use nnnoiseless::DenoiseState;

use crate::dsp::{Smoothed, clamp, db, gain};

/// `RNNoise`'s frame: 10 ms at 48 kHz.
pub const FRAME: usize = DenoiseState::FRAME_SIZE;

/// The node's delay: one frame of buffering and `RNNoise`'s one frame of look-back.
pub const LATENCY: usize = 2 * FRAME;

/// `RNNoise` reads 16-bit sample values as floats.
const PCM_SCALE: f32 = 32_768.0;

/// The meter's power follower, per sample (about 40 ms).
const POWER: f64 = 0.999_5;

/// The most the VAD gate takes off when nobody talks, in dB.
pub const GATE_RANGE_DB: f64 = 30.0;

/// The parameters of `fx.denoise`.
pub static PARAMS: [ParamSpec; 2] = [
    ParamSpec { name: "amount", unit: Unit::Percent, min: 0.0, max: 1.0, default: 1.0, smooth_ms: 30.0 },
    ParamSpec { name: "gate", unit: Unit::Percent, min: 0.0, max: 1.0, default: 0.0, smooth_ms: 30.0 },
];

/// The `fx.denoise` kind. Mono (the microphone).
#[derive(Debug)]
pub struct DenoiseKind;

register_node_kind!(DenoiseKind);

impl NodeKind for DenoiseKind {
    fn info(&self) -> NodeKindInfo {
        #[allow(clippy::cast_possible_truncation)] // 960
        let latency = LATENCY as u32;
        NodeKindInfo {
            id: "fx.denoise",
            summary: "noise suppression: takes fans, hum, keyboards and room noise out of the microphone, in 20 ms",
            channels: ChannelSupport::Mono,
            params: &PARAMS,
            latency: LatencyRange { min: latency, max: latency },
            sidechain: false,
        }
    }

    fn create(&self, _: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError> {
        if channels != Channels::Mono {
            return Err(NodeError::UnsupportedChannels(channels));
        }
        Ok(Box::new(Denoise::new()))
    }
}

/// A running `fx.denoise`.
pub struct Denoise {
    /// The network and its buffers.
    state: Box<DenoiseState<'static>>,
    /// The same, untouched since `create`, for `reset`.
    pristine: Box<DenoiseState<'static>>,
    /// The frame being filled, in `RNNoise`'s scale.
    input: [f32; FRAME],
    /// The last frame `RNNoise` gave back, played during the frame after it was made.
    ready: [f32; FRAME],
    /// The dry signal, [`LATENCY`] samples back, for the mix: a ring indexed by the graph clock.
    dry: Box<[f32; LATENCY]>,
    /// The voice probability `RNNoise` gave with `ready`.
    vad: f32,
    params: [Smoothed; 2],
    /// The VAD gate's gain, moving toward its target per sample.
    gate_gain: Smoothed,
    /// Short-term powers of the dry and the output, for the meter.
    power_in: f64,
    power_out: f64,
}

impl std::fmt::Debug for Denoise {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Denoise").field("vad", &self.vad).finish_non_exhaustive()
    }
}

impl Default for Denoise {
    fn default() -> Self {
        Self::new()
    }
}

impl Denoise {
    /// A suppressor at its defaults (full strength, no gate).
    #[must_use]
    pub fn new() -> Self {
        // RNNoise's process-wide tables (window, DCT) are built on first use: build them here, off the audio thread.
        let mut warm = DenoiseState::new();
        let mut out = [0.0f32; FRAME];
        warm.process_frame(&mut out, &[0.0; FRAME]);
        let pristine = DenoiseState::new();
        Self {
            state: pristine.clone(),
            pristine,
            input: [0.0; FRAME],
            ready: [0.0; FRAME],
            dry: Box::new([0.0; LATENCY]),
            vad: 0.0,
            params: PARAMS.map(|p| Smoothed::new(f64::from(p.default), f64::from(p.smooth_ms))),
            gate_gain: Smoothed::new(1.0, 15.0),
            power_in: 0.0,
            power_out: 0.0,
        }
    }

    /// `RNNoise`'s voice probability for the frame playing now, 0 to 1.
    #[must_use]
    pub const fn voice_probability(&self) -> f32 {
        self.vad
    }

    /// One sample in at graph-clock index `n`; the sample [`LATENCY`] earlier out.
    #[allow(clippy::cast_possible_truncation)] // positions inside a frame and a ring
    fn step(&mut self, n: u64, x: f32) -> f32 {
        let pos = (n % FRAME as u64) as usize;
        let ring = (n % LATENCY as u64) as usize;
        let [amount, gate] = self.params.each_mut().map(Smoothed::next);
        let wet = f64::from(self.ready[pos] / PCM_SCALE);
        let dry = f64::from(self.dry[ring]);
        self.dry[ring] = x;
        // The gate's target: down by up to `GATE_RANGE_DB` × `gate` as the voice probability falls under one half.
        let quiet = (1.0 - f64::from(self.vad) * 2.0).clamp(0.0, 1.0);
        self.gate_gain.set(gain(-GATE_RANGE_DB * gate * quiet));
        let out = (amount * (wet - dry) + dry) * self.gate_gain.next();
        self.input[pos] = x * PCM_SCALE;
        if pos == FRAME - 1 {
            self.vad = self.state.process_frame(&mut self.ready, &self.input);
        }
        self.power_in = POWER * self.power_in + (1.0 - POWER) * dry * dry;
        self.power_out = POWER * self.power_out + (1.0 - POWER) * out * out;
        out as f32
    }
}

impl Node for Denoise {
    fn latency(&self) -> u32 {
        #[allow(clippy::cast_possible_truncation)] // 960
        let latency = LATENCY as u32;
        latency
    }

    fn set_param(&mut self, param: ParamIndex, value: f32) {
        let at = usize::from(param.0);
        if let (Some(spec), Some(p)) = (PARAMS.get(at), self.params.get_mut(at)) {
            p.set(clamp(value, spec.min, spec.max));
        }
    }

    fn process(&mut self, block: &mut BlockMut<'_>) {
        let first = block.first_sample;
        for (i, s) in block.left.iter_mut().enumerate() {
            *s = self.step(first + i as u64, *s);
        }
    }

    fn reset(&mut self) {
        self.state.clone_from(&self.pristine);
        self.input = [0.0; FRAME];
        self.ready = [0.0; FRAME];
        self.dry.fill(0.0);
        self.vad = 0.0;
        self.params.iter_mut().for_each(Smoothed::snap);
        self.gate_gain = Smoothed::new(1.0, 15.0);
        self.power_in = 0.0;
        self.power_out = 0.0;
    }

    fn report(&self) -> NodeReport {
        #[allow(clippy::cast_possible_truncation)] // a meter reading
        let reduction = (db(self.power_in.sqrt()) - db(self.power_out.sqrt())).max(0.0) as f32;
        NodeReport { gain_reduction_db: if self.power_in < 1e-10 { 0.0 } else { reduction } }
    }
}
