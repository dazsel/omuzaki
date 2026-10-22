//! `fx.gate`: the zero-latency noise gate (issue #913).
//!
//! The detector hears the signal through a high-pass at `hpf_hz` (so rumble never opens the gate) and follows its peak
//! with an instant attack and a 10 ms release, in dB. The gate opens above `open_db` and starts to close only after the
//! level has stayed under `close_db` for `hold_ms`; the gap between the two thresholds is the hysteresis that keeps a
//! level hovering near one threshold from chattering.
//!
//! The gain moves to full within `attack_ms` (a one-pole whose time constant is `attack_ms / ln 1000`, so it is within
//! 0.01 dB of full at `attack_ms` and then lands on exactly 1) and falls to `range_db` with the one-pole time constant
//! `release_ms`.

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo, ParamIndex, ParamSpec,
    Unit,
};
use omuzaki_audio_api::register_node_kind;

use crate::biquad::{Biquad, Coefs, Shape};
use crate::dsp::{clamp, db, gain, one_pole, samples};

/// The detector's release, in milliseconds.
pub const DETECTOR_RELEASE_MS: f64 = 10.0;

/// The fraction of full gain at which the attack lands on exactly 1.
const ATTACK_LANDING: f64 = 0.999;

/// The parameters of `fx.gate`. The thresholds and times apply at once; the gain they steer is what moves smoothly.
pub static PARAMS: [ParamSpec; 7] = [
    ParamSpec { name: "open_db", unit: Unit::Db, min: -80.0, max: 0.0, default: -45.0, smooth_ms: 0.0 },
    ParamSpec { name: "close_db", unit: Unit::Db, min: -80.0, max: 0.0, default: -50.0, smooth_ms: 0.0 },
    ParamSpec { name: "hold_ms", unit: Unit::Ms, min: 0.0, max: 500.0, default: 80.0, smooth_ms: 0.0 },
    ParamSpec { name: "attack_ms", unit: Unit::Ms, min: 0.1, max: 50.0, default: 1.0, smooth_ms: 0.0 },
    ParamSpec { name: "release_ms", unit: Unit::Ms, min: 5.0, max: 1000.0, default: 120.0, smooth_ms: 0.0 },
    ParamSpec { name: "range_db", unit: Unit::Db, min: -80.0, max: 0.0, default: -80.0, smooth_ms: 0.0 },
    ParamSpec { name: "hpf_hz", unit: Unit::Hz, min: 20.0, max: 400.0, default: 100.0, smooth_ms: 0.0 },
];

/// The `fx.gate` kind. Mono.
#[derive(Debug)]
pub struct GateKind;

register_node_kind!(GateKind);

impl NodeKind for GateKind {
    fn info(&self) -> NodeKindInfo {
        NodeKindInfo {
            id: "fx.gate",
            summary: "a noise gate: silences the microphone between words, with hysteresis and hold",
            channels: ChannelSupport::Mono,
            params: &PARAMS,
            latency: LatencyRange { min: 0, max: 0 },
            sidechain: false,
        }
    }

    fn create(&self, _: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError> {
        if channels != Channels::Mono {
            return Err(NodeError::UnsupportedChannels(channels));
        }
        Ok(Box::new(Gate::new()))
    }
}

/// Where the gate is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GateState {
    /// Shut: the gain is at (or falling to) `range_db`.
    Closed,
    /// Open: the gain is at (or rising to) 1.
    Open,
    /// The level fell under `close_db`; the gate stays open for `hold_ms` more.
    Hold,
    /// The hold ran out; the gain is falling to `range_db`.
    Closing,
}

/// A running `fx.gate`.
#[derive(Clone, Debug)]
pub struct Gate {
    open_db: f64,
    close_db: f64,
    hold: usize,
    attack: f64,
    release: f64,
    range: f64,
    hpf_hz: f64,
    detector_release: f64,
    hpf: Biquad,
    env: f64,
    state: GateState,
    held: usize,
    gain: f64,
    changes: u64,
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    /// A closed gate with the default parameters.
    #[must_use]
    pub fn new() -> Self {
        let p = |i: usize| f64::from(PARAMS[i].default);
        let mut gate = Self {
            open_db: p(0),
            close_db: p(1),
            hold: samples(p(2)),
            attack: 0.0,
            release: 0.0,
            range: gain(p(5)),
            hpf_hz: p(6),
            detector_release: one_pole(DETECTOR_RELEASE_MS),
            hpf: Biquad::default(),
            env: 0.0,
            state: GateState::Closed,
            held: 0,
            gain: gain(p(5)),
            changes: 0,
        };
        gate.set_attack(p(3));
        gate.release = one_pole(p(4));
        gate.tune_hpf();
        gate
    }

    fn set_attack(&mut self, ms: f64) {
        self.attack = one_pole(ms / (1.0 / (1.0 - ATTACK_LANDING)).ln());
    }

    fn tune_hpf(&mut self) {
        self.hpf.coefs = Coefs::rbj(Shape::HighPass, self.hpf_hz, std::f64::consts::FRAC_1_SQRT_2, 0.0);
    }

    /// Where the gate is now.
    #[must_use]
    pub const fn state(&self) -> GateState {
        self.state
    }

    /// How many times the state has changed since the gate was made or reset (for the hysteresis test).
    #[must_use]
    pub const fn changes(&self) -> u64 {
        self.changes
    }

    /// The gain applied to the last sample.
    #[must_use]
    pub const fn gain(&self) -> f64 {
        self.gain
    }

    fn go(&mut self, state: GateState) {
        if self.state != state {
            self.state = state;
            self.changes += 1;
        }
    }

    /// One sample through the detector, the state machine and the gain.
    fn step(&mut self, x: f64) -> f64 {
        let side = self.hpf.run(x).abs();
        self.env = side.max(self.env * self.detector_release);
        let level = db(self.env);
        // `close_db <= open_db`, enforced here so a preset can never make the gate open and close at once.
        let close = self.close_db.min(self.open_db);
        match self.state {
            GateState::Closed | GateState::Closing if level > self.open_db => self.go(GateState::Open),
            GateState::Open if level < close => {
                self.held = 0;
                self.go(GateState::Hold);
                self.hold_tick();
            }
            GateState::Hold if level > self.open_db => self.go(GateState::Open),
            GateState::Hold => self.hold_tick(),
            _ => {}
        }
        let open = matches!(self.state, GateState::Open | GateState::Hold);
        if open {
            self.gain = 1.0 + self.attack * (self.gain - 1.0);
            if self.gain >= ATTACK_LANDING {
                self.gain = 1.0;
            }
        } else {
            self.gain = self.range + self.release * (self.gain - self.range);
            if (self.gain - self.range).abs() < 1e-12 {
                self.gain = self.range;
                if self.state == GateState::Closing {
                    self.go(GateState::Closed);
                }
            }
        }
        x * self.gain
    }

    fn hold_tick(&mut self) {
        if self.held >= self.hold {
            self.go(GateState::Closing);
        } else {
            self.held += 1;
        }
    }
}

impl Node for Gate {
    fn latency(&self) -> u32 {
        0
    }

    fn set_param(&mut self, param: ParamIndex, value: f32) {
        let at = usize::from(param.0);
        let Some(spec) = PARAMS.get(at) else { return };
        let v = clamp(value, spec.min, spec.max);
        match at {
            0 => self.open_db = v,
            1 => self.close_db = v,
            2 => self.hold = samples(v),
            3 => self.set_attack(v),
            4 => self.release = one_pole(v),
            5 => self.range = gain(v),
            _ => {
                self.hpf_hz = v;
                self.tune_hpf();
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)] // the audio is f32
    fn process(&mut self, block: &mut BlockMut<'_>) {
        for s in block.left.iter_mut() {
            *s = self.step(f64::from(*s)) as f32;
        }
    }

    fn reset(&mut self) {
        self.hpf.reset();
        self.env = 0.0;
        self.state = GateState::Closed;
        self.held = 0;
        self.gain = self.range;
        self.changes = 0;
    }

    fn report(&self) -> omuzaki_audio_api::node::NodeReport {
        #[allow(clippy::cast_possible_truncation)] // a meter reading
        omuzaki_audio_api::node::NodeReport { gain_reduction_db: (-db(self.gain)) as f32 }
    }
}
