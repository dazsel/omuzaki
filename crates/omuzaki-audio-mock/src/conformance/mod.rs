//! The node conformance suite (design section 9.2): the checks every [`NodeKind`] must pass, so the real effects, the
//! test fixtures and the engine can all rely on the rules of `omuzaki_audio_api::node`.
//!
//! [`check`] runs the suite on one kind and returns a [`Report`] that names every check that failed, with the parameter
//! setting, the channel count and (for a sweep) the parameter. [`check_all`] runs it on every registered kind.
//!
//! | Check | What it holds a node to |
//! |---|---|
//! | [`Check::Declaration`] | the kind's description is consistent, and a node of it can be made on every channel count it claims |
//! | [`Check::Latency`] | an impulse (linear kinds) or a click train (the rest) comes out [`Node::latency`](omuzaki_audio_api::node::Node::latency) samples late, within a tolerance, and the latency never changes |
//! | [`Check::BlockSizes`] | blocks of 1, 7, 64, 256, 1024 and random lengths give bit-identical output |
//! | [`Check::Allocation`] | `process`, `set_param`, `reset` and `report` neither allocate nor free |
//! | [`Check::Finite`] | a minute of white noise gives no NaN or infinity |
//! | [`Check::SilenceDecay`] | after the noise, silence decays below -120 dBFS |
//! | [`Check::Reset`] | after `reset` the output does not depend on what the node processed before (a node that ran only silence, then reset, is the reference, so an oscillator's phase is compared like for like) |
//! | [`Check::Sweep`] | moving a parameter across its range mid-signal makes no click bigger than its smoothing allows |
//!
//! Latency, block sizes, allocation, finiteness, decay and reset run at three **parameter settings**: every parameter
//! at its default, at its minimum and at its maximum. A kind that runs on stereo buses is checked on mono and on
//! stereo. If the declaration fails the other checks do not run, because they would all repeat its failure.
//!
//! The allocation check needs the test binary to install [`CountingAlloc`] as its global allocator; without it the
//! check fails, because a zero count would otherwise pass every node. Use [`Config::without_allocation_check`] to run
//! the rest on purpose.

mod alloc;
mod checks;
mod run;

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{ChannelSupport, NodeConfig, NodeKind, NodeKindInfo, node_kinds};

pub use alloc::{CountingAlloc, allocations_during};

/// One of the checks of the suite.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// The kind's description and its factory agree.
    Declaration,
    /// The declared latency is true.
    Latency,
    /// The output does not depend on how the input is cut into blocks.
    BlockSizes,
    /// The audio-thread methods neither allocate nor free.
    Allocation,
    /// The output is finite for any input.
    Finite,
    /// Silence in decays to nothing.
    SilenceDecay,
    /// `reset` forgets everything.
    Reset,
    /// A parameter sweep makes no click.
    Sweep,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Declaration => "declaration",
            Self::Latency => "latency honesty",
            Self::BlockSizes => "block-size invariance",
            Self::Allocation => "no allocation",
            Self::Finite => "finite output",
            Self::SilenceDecay => "silence decay",
            Self::Reset => "reset",
            Self::Sweep => "parameter sweep",
        })
    }
}

/// Where every parameter of the node was put for a check.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Setting {
    /// Every parameter at its default.
    Default,
    /// Every parameter at its minimum.
    Min,
    /// Every parameter at its maximum.
    Max,
}

impl fmt::Display for Setting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Default => "default",
            Self::Min => "min",
            Self::Max => "max",
        })
    }
}

/// What a check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The node meets the rule.
    Pass,
    /// The node breaks the rule; the text says how.
    Fail(String),
    /// The check could not measure anything here; the text says why. A skip is not a failure.
    Skipped(String),
}

/// One check at one setting on one channel count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckResult {
    /// The kind's id.
    pub kind: String,
    /// Which check.
    pub check: Check,
    /// The parameter setting, for the checks that run at three.
    pub setting: Option<Setting>,
    /// The bus the node ran on; `None` for the declaration.
    pub channels: Option<Channels>,
    /// The parameter, for a sweep.
    pub param: Option<String>,
    /// What was found.
    pub outcome: Outcome,
}

impl fmt::Display for CheckResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verb = match &self.outcome {
            Outcome::Pass => "passed",
            Outcome::Fail(_) => "FAILED",
            Outcome::Skipped(_) => "skipped",
        };
        write!(f, "{}: {} {verb}", self.kind, self.check)?;
        if let Some(setting) = self.setting {
            write!(f, " at {setting} settings")?;
        }
        if let Some(channels) = self.channels {
            write!(f, " on {channels:?}")?;
        }
        if let Some(param) = &self.param {
            write!(f, " for parameter `{param}`")?;
        }
        match &self.outcome {
            Outcome::Pass => Ok(()),
            Outcome::Fail(why) | Outcome::Skipped(why) => write!(f, ": {why}"),
        }
    }
}

/// The result of the suite on one kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The kind's id.
    pub kind: String,
    /// Every check that ran, in order.
    pub results: Vec<CheckResult>,
}

impl Report {
    /// Whether no check failed. Skips do not count against it; see [`Report::skipped`].
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.failures().next().is_none()
    }

    /// The checks that failed.
    pub fn failures(&self) -> impl Iterator<Item = &CheckResult> {
        self.results.iter().filter(|r| matches!(r.outcome, Outcome::Fail(_)))
    }

    /// The checks that could not measure anything.
    pub fn skipped(&self) -> impl Iterator<Item = &CheckResult> {
        self.results.iter().filter(|r| matches!(r.outcome, Outcome::Skipped(_)))
    }
}

impl fmt::Display for Report {
    /// One line per failure, or one line saying the kind passed.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_ok() {
            return write!(f, "{}: all {} checks passed", self.kind, self.results.len());
        }
        for (i, failure) in self.failures().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{failure}")?;
        }
        Ok(())
    }
}

/// Whether the allocation check must run.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AllocationCheck {
    /// Run it; fail if [`CountingAlloc`] is not installed.
    Required,
    /// Do not run it; the report says it was skipped.
    Skip,
}

/// How the suite runs on one kind.
#[derive(Clone, Debug)]
pub struct Config {
    /// The settings that fix a node's shape (an EQ's bands, a model slot), for kinds that cannot be made from nothing.
    pub node_config: NodeConfig,
    /// Whether the kind is linear. A linear kind's latency is measured with an impulse; any other kind (a compressor, a
    /// gate, a pitch shifter) with a click train and onset alignment.
    pub linear: bool,
    /// How many samples the measured latency may differ from the declared one. One by default; a kind whose latency is
    /// only defined to a tolerance (`fx.pitch` is 1 ms) declares its own.
    pub latency_tolerance: u32,
    /// The seed of every random signal and block plan.
    pub seed: u64,
    /// How long the white noise of the finite check runs, in seconds (60 by default).
    pub noise_seconds: f64,
    /// How long the silence after it runs, in seconds (10 by default).
    pub decay_seconds: f64,
    /// Whether to run the allocation check.
    pub allocations: AllocationCheck,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            node_config: NodeConfig::default(),
            linear: true,
            latency_tolerance: 1,
            seed: 0x0123_4567_89AB_CDEF,
            noise_seconds: 60.0,
            decay_seconds: 10.0,
            allocations: AllocationCheck::Required,
        }
    }
}

impl Config {
    /// The same configuration without the allocation check.
    #[must_use]
    pub fn without_allocation_check(mut self) -> Self {
        self.allocations = AllocationCheck::Skip;
        self
    }
}

/// Runs the suite on `kind` with the default [`Config`].
#[must_use]
pub fn check(kind: &dyn NodeKind) -> Report {
    check_with(kind, &Config::default())
}

/// Runs the suite on every registered node kind, with the configuration `config_for` gives each.
#[must_use]
pub fn check_all(config_for: impl Fn(&NodeKindInfo) -> Config) -> Vec<Report> {
    node_kinds().into_iter().map(|kind| check_with(kind, &config_for(&kind.info()))).collect()
}

/// Runs a check, turning a panic in the node into a failure.
fn guarded(run: impl FnOnce() -> Outcome) -> Outcome {
    catch_unwind(AssertUnwindSafe(run)).unwrap_or_else(|panic| {
        let why = panic
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic with no message".into());
        Outcome::Fail(format!("the node panicked: {why}"))
    })
}

/// Runs the suite on `kind`.
#[must_use]
pub fn check_with(kind: &dyn NodeKind, config: &Config) -> Report {
    let info = kind.info();
    let mut report = Report { kind: info.id.to_string(), results: Vec::new() };
    let mut record = |check, setting, channels, param: Option<String>, outcome| {
        report.results.push(CheckResult { kind: info.id.to_string(), check, setting, channels, param, outcome });
    };
    let declared = guarded(|| checks::declaration(kind, &info, config));
    let usable = declared == Outcome::Pass;
    record(Check::Declaration, None, None, None, declared);
    if !usable {
        // Without a valid description or a node to run, every other check would repeat the same failure.
        return report;
    }
    let buses: &[Channels] = match info.channels {
        ChannelSupport::Mono => &[Channels::Mono],
        ChannelSupport::Any => &[Channels::Mono, Channels::Stereo],
    };
    for &channels in buses {
        let cx = checks::Context { kind, info: &info, config, channels };
        for setting in [Setting::Default, Setting::Min, Setting::Max] {
            let at = (Some(setting), Some(channels));
            record(Check::Latency, at.0, at.1, None, guarded(|| checks::latency(&cx, setting)));
            record(Check::BlockSizes, at.0, at.1, None, guarded(|| checks::block_sizes(&cx, setting)));
            record(Check::Allocation, at.0, at.1, None, guarded(|| checks::allocation(&cx, setting)));
            let (finite, decay) = checks::finite_and_decay(&cx, setting);
            record(Check::Finite, at.0, at.1, None, finite);
            record(Check::SilenceDecay, at.0, at.1, None, decay);
            record(Check::Reset, at.0, at.1, None, guarded(|| checks::reset(&cx, setting)));
        }
        for (index, spec) in info.params.iter().enumerate() {
            let outcome = guarded(|| checks::sweep(&cx, index));
            record(Check::Sweep, None, Some(channels), Some(spec.name.to_string()), outcome);
        }
    }
    report
}
