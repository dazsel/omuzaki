//! Seeded synthetic signals: a speech-like source, a room impulse response and a loudspeaker. No recordings of
//! people are used anywhere in this spike.

/// The rate every Omuzaki audio budget is gated at (`docs/design/audio-api.md` section 3).
pub const RATE: usize = 48_000;
/// One block: 10 ms, the AEC3 frame.
pub const FRAME: usize = RATE / 100;

/// xorshift64*: deterministic, dependency-free.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }

    /// Uniform in [-1, 1).
    pub fn uniform(&mut self) -> f32 {
        self.next_u32() as f32 / 2_147_483_648.0 - 1.0
    }

    /// Roughly Gaussian (sum of four uniforms, scaled to unit variance).
    pub fn gauss(&mut self) -> f32 {
        (self.uniform() + self.uniform() + self.uniform() + self.uniform()) * 0.866
    }

    /// Uniform in [lo, hi).
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (self.uniform() * 0.5 + 0.5) * (hi - lo)
    }
}

/// A two-pole resonator (a formant).
struct Resonator {
    b0: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Resonator {
    fn new(freq: f32, bandwidth: f32) -> Self {
        let r = (-std::f32::consts::PI * bandwidth / RATE as f32).exp();
        let w = 2.0 * std::f32::consts::PI * freq / RATE as f32;
        Self { b0: 1.0 - r, a1: 2.0 * r * w.cos(), a2: -r * r, z1: 0.0, z2: 0.0 }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.a1 * self.z1 + self.a2 * self.z2;
        self.z2 = self.z1;
        self.z1 = y;
        y
    }
}

/// A speech-like signal: syllables of voiced sound (a gliding pitch through two moving formants) separated by pauses,
/// with some fricative noise. It is not speech, but it is non-stationary, harmonic and bursty like it, which is what
/// an echo canceller's adaptation and double-talk logic react to. Peak-normalised to `peak`.
pub fn speech_like(seed: u64, seconds: usize, peak: f32) -> Vec<f32> {
    let n = seconds * RATE;
    let mut rng = Rng::new(seed);
    let mut out = vec![0.0f32; n];
    let mut pos = 0;
    let mut phase = 0.0f32;
    while pos < n {
        // 150 to 450 ms of syllable, then a 40 to 300 ms pause.
        let len = (rng.range(0.15, 0.45) * RATE as f32) as usize;
        let pause = (rng.range(0.04, 0.30) * RATE as f32) as usize;
        let f0_start = rng.range(95.0, 230.0);
        let f0_end = f0_start * rng.range(0.8, 1.25);
        let mut f1 = Resonator::new(rng.range(300.0, 800.0), 90.0);
        let mut f2 = Resonator::new(rng.range(900.0, 2400.0), 140.0);
        let fricative = rng.uniform() > 0.55;
        for i in 0..len.min(n - pos) {
            let t = i as f32 / len as f32;
            let env = (std::f32::consts::PI * t).sin().powf(0.6);
            let f0 = f0_start + (f0_end - f0_start) * t;
            phase += f0 / RATE as f32;
            phase -= phase.floor();
            // A band-limited-ish glottal pulse: a falling sawtooth, smoothed by the formants.
            let glottal = 1.0 - 2.0 * phase;
            let excite = if fricative { rng.gauss() * 0.7 } else { glottal };
            out[pos + i] = env * (f1.run(excite) + 0.6 * f2.run(excite));
        }
        pos += len + pause;
    }
    let max = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if max > 0.0 {
        let g = peak / max;
        for v in &mut out {
            *v *= g;
        }
    }
    out
}

/// A synthetic room: a speaker-to-mic bulk delay, a direct path and an exponentially decaying diffuse tail.
#[derive(Clone, Copy)]
pub struct Room {
    pub name: &'static str,
    /// Device and acoustic delay between the reference and the mic, in ms.
    pub delay_ms: f32,
    /// Time for the tail to fall 60 dB, in ms.
    pub rt60_ms: f32,
    /// Length of the response after the direct path, in ms (the tail is truncated here).
    pub tail_ms: f32,
    /// Soft-clip the loudspeaker (a cheap speaker driven hard) before the room.
    pub nonlinear: bool,
}

/// The rooms the bench runs. Delays of 12 to 180 ms bracket a wired device (a few ms of buffer plus about 3 ms of air)
/// and a Bluetooth or monitored-through-OBS path.
pub const ROOMS: [Room; 5] = [
    Room { name: "small", delay_ms: 12.0, rt60_ms: 150.0, tail_ms: 150.0, nonlinear: false },
    Room { name: "medium", delay_ms: 40.0, rt60_ms: 300.0, tail_ms: 250.0, nonlinear: false },
    Room { name: "large", delay_ms: 80.0, rt60_ms: 450.0, tail_ms: 300.0, nonlinear: false },
    Room { name: "bt-delay", delay_ms: 180.0, rt60_ms: 300.0, tail_ms: 250.0, nonlinear: false },
    Room { name: "medium-nl", delay_ms: 40.0, rt60_ms: 300.0, tail_ms: 250.0, nonlinear: true },
];

impl Room {
    /// The impulse response, delay included, from a seed.
    pub fn impulse_response(&self, seed: u64) -> Vec<f32> {
        let mut rng = Rng::new(seed);
        let lead = (self.delay_ms * 1e-3 * RATE as f32) as usize;
        let tail = (self.tail_ms * 1e-3 * RATE as f32) as usize;
        let slope = 6.908 / (self.rt60_ms * 1e-3 * RATE as f32); // ln(1000) over rt60
        let mut h = vec![0.0f32; lead + tail + 1];
        h[lead] = 1.0;
        // Early reflections (-6 dB at 3 ms and -10 dB at 7 ms), then the diffuse tail, 18 dB under the direct path.
        for (ms, g) in [(3.0f32, 0.5f32), (7.0, 0.32)] {
            let at = lead + (ms * 1e-3 * RATE as f32) as usize;
            if at < h.len() {
                h[at] += g * rng.uniform().signum();
            }
        }
        let tail_gain = 0.125;
        for (i, v) in h.iter_mut().enumerate().skip(lead + 1) {
            let t = (i - lead) as f32;
            *v += tail_gain * rng.gauss() * (-slope * t).exp();
        }
        h
    }

    /// The echo of `far` at the mic: optional loudspeaker soft clipping, then the room, at `echo_gain`.
    pub fn echo(&self, far: &[f32], seed: u64, echo_gain: f32) -> Vec<f32> {
        let h = self.impulse_response(seed);
        let drive: Vec<f32> =
            if self.nonlinear { far.iter().map(|v| (3.0 * v).tanh() / 3.0 * 1.4).collect() } else { far.to_vec() };
        let mut out = vec![0.0f32; far.len()];
        // Direct convolution: the signals are short, the filter is sparse in its early part, and this runs once.
        for (k, &hk) in h.iter().enumerate() {
            if hk == 0.0 {
                continue;
            }
            let g = hk * echo_gain;
            for (o, &x) in out[k..].iter_mut().zip(drive.iter()) {
                *o += g * x;
            }
        }
        out
    }
}
