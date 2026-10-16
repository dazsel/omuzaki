//! The node conformance suite against the `null` kind and against deliberately broken fixtures, each of which must
//! fail the one check it breaks, with a message that says why.

#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

use std::hint::black_box;

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, ConfigValue, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo,
    ParamIndex, ParamSpec, Unit,
};
use omuzaki_audio_mock::NullKind;
use omuzaki_audio_mock::conformance::{
    Check, Config, CountingAlloc, Outcome, Report, Setting, check, check_all, check_with,
};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// How a fixture is broken.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Flaw {
    /// Not broken: a delay and a gain smoothed per sample.
    None,
    /// Declares 7 more samples of latency than it has.
    LyingLatency,
    /// Smooths its gain once per block, so the block size shows in the output.
    BlockDependent,
    /// Allocates in `process`.
    Allocates,
    /// Outputs NaN once it has run for 100 000 samples.
    Nan,
    /// Leaks a constant into its output, so silence never decays.
    NoDecay,
    /// `reset` does nothing.
    IgnoresReset,
    /// Declares 5 ms of smoothing and applies the gain at once.
    Clicks,
    /// Allocates in `set_param`.
    AllocatesInSetParam,
    /// Panics in `process` after a while.
    Panics,
    /// Frees a buffer it made at creation, in `process`.
    Frees,
    /// Leaks into the right channel only, so silence decays on a mono bus and not on a stereo one.
    RightLeaks,
    /// Allocates in `process`, but only while the gain is at its minimum.
    AllocatesAtMinimum,
}

const SMOOTHED: &[ParamSpec] =
    &[ParamSpec { name: "gain_db", unit: Unit::Db, min: -24.0, max: 12.0, default: 0.0, smooth_ms: 5.0 }];

const DELAY: usize = 20;

struct Fixture {
    flaw: Flaw,
    /// The delay in samples (the declared latency, before a lie is added).
    delay: usize,
    /// Hard-clip at 0.3: a kind that is not linear.
    clip: bool,
}

impl NodeKind for Fixture {
    fn info(&self) -> NodeKindInfo {
        NodeKindInfo {
            id: "test.fixture",
            summary: "a test fixture",
            channels: ChannelSupport::Any,
            params: SMOOTHED,
            latency: LatencyRange { min: 0, max: 100_000 },
            sidechain: false,
        }
    }

    fn create(&self, _config: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError> {
        Ok(Box::new(FixtureNode {
            delay: self.delay,
            flaw: self.flaw,
            clip: self.clip,
            history: vec![vec![0.0; self.delay]; channels.count()],
            position: 0,
            gain: 1.0,
            target: 1.0,
            processed: 0,
            spare: Some(Box::new([0; 16])),
        }))
    }
}

struct FixtureNode {
    flaw: Flaw,
    delay: usize,
    clip: bool,
    history: Vec<Vec<f32>>,
    position: usize,
    gain: f32,
    target: f32,
    processed: u64,
    spare: Option<Box<[u8; 16]>>,
}

impl Node for FixtureNode {
    fn latency(&self) -> u32 {
        let delay = u32::try_from(self.delay).unwrap_or(u32::MAX);
        if self.flaw == Flaw::LyingLatency { delay + 7 } else { delay }
    }

    fn set_param(&mut self, _param: ParamIndex, value: f32) {
        if self.flaw == Flaw::AllocatesInSetParam {
            black_box(vec![0u8; 8]);
        }
        self.target = 10f32.powf(value / 20.0);
        if self.flaw == Flaw::Clicks {
            self.gain = self.target;
        }
    }

    fn process(&mut self, block: &mut BlockMut<'_>) {
        if self.flaw == Flaw::Allocates || (self.flaw == Flaw::AllocatesAtMinimum && self.target < 0.07) {
            black_box(Box::new([0u8; 16]));
        }
        assert!(self.flaw != Flaw::Panics || self.processed <= 20_000, "the fixture gave up");
        if self.flaw == Flaw::Frees {
            black_box(self.spare.take());
        }
        let frames = block.frames();
        // 5 ms at 48 kHz is a time constant of 240 samples.
        let step = 1.0 - (-1.0f32 / 240.0).exp();
        let block_gain_step = if self.flaw == Flaw::BlockDependent { 0.02 } else { 0.0 };
        if self.flaw == Flaw::BlockDependent {
            self.gain += (self.target - self.gain) * block_gain_step;
        }
        let (position, gain0) = (self.position, self.gain);
        let mut channel = 0;
        let (flaw, clip, processed) = (self.flaw, self.clip, self.processed);
        let (target, history, delay) = (self.target, &mut self.history, self.delay);
        let mut end_gain = gain0;
        block.for_each_channel(|samples| {
            let mut at = position;
            let mut gain = gain0;
            for (n, sample) in samples.iter_mut().enumerate() {
                std::mem::swap(sample, &mut history[channel][at]);
                at = (at + 1) % delay;
                if matches!(
                    flaw,
                    Flaw::None | Flaw::LyingLatency | Flaw::NoDecay | Flaw::RightLeaks | Flaw::AllocatesAtMinimum
                ) || clip
                {
                    gain += (target - gain) * step;
                }
                *sample *= gain;
                if clip {
                    *sample = sample.clamp(-0.3, 0.3);
                }
                if flaw == Flaw::NoDecay || (flaw == Flaw::RightLeaks && channel == 1) {
                    *sample += 1e-3;
                }
                if flaw == Flaw::Nan && processed + n as u64 >= 100_000 {
                    *sample = f32::NAN;
                }
            }
            end_gain = gain;
            channel += 1;
        });
        if matches!(
            self.flaw,
            Flaw::None | Flaw::LyingLatency | Flaw::NoDecay | Flaw::RightLeaks | Flaw::AllocatesAtMinimum
        ) || self.clip
        {
            self.gain = end_gain;
        }
        self.position = (position + frames) % self.delay;
        self.processed += frames as u64;
    }

    fn reset(&mut self) {
        if self.flaw != Flaw::IgnoresReset {
            self.history.iter_mut().for_each(|h| h.fill(0.0));
            self.position = 0;
        }
    }
}

/// Fast settings for the broken fixtures: a few seconds of noise are enough to show a flaw.
fn quick() -> Config {
    Config { noise_seconds: 4.0, decay_seconds: 2.0, ..Config::default() }
}

fn failed_checks(report: &Report) -> Vec<Check> {
    let mut checks: Vec<Check> = report.failures().map(|r| r.check).collect();
    checks.dedup();
    checks
}

fn assert_only_fails(flaw: Flaw, expected: Check, message: &str) {
    let report = check_with(&Fixture { flaw, clip: false, delay: DELAY }, &quick());
    assert_eq!(failed_checks(&report), [expected], "{report}");
    let text = report.to_string();
    assert!(text.contains(message), "`{message}` is not in:\n{text}");
    assert!(text.contains(&expected.to_string()), "the check is named: {text}");
}

#[test]
fn the_null_kind_passes_with_nothing_skipped() {
    let report = check(&NullKind);
    assert!(report.is_ok(), "{report}");
    assert_eq!(report.skipped().count(), 0, "every check ran");
    // The declaration, then per bus (mono, stereo): 3 settings x 6 checks, and one sweep per parameter.
    assert_eq!(report.results.len(), 1 + 2 * (3 * 6 + 1));
    assert_eq!(report.to_string(), format!("mock.null: all {} checks passed", report.results.len()));
}

#[test]
fn the_null_kind_passes_with_a_configured_latency() {
    for samples in [1, 37, 480] {
        let mut node_config = NodeConfig::default();
        node_config.0.insert("latency".into(), ConfigValue::Int(samples));
        let report = check_with(&NullKind, &Config { node_config, noise_seconds: 2.0, ..Config::default() });
        assert!(report.is_ok(), "latency {samples}: {report}");
    }
}

#[test]
fn a_well_behaved_smoothed_kind_passes_with_an_impulse_and_with_a_click_train() {
    let report = check_with(&Fixture { flaw: Flaw::None, clip: false, delay: DELAY }, &quick());
    assert!(report.is_ok(), "{report}");
    assert_eq!(report.skipped().count(), 0);
    // A clipper is not linear: its latency is measured with a click train and onset alignment.
    let report =
        check_with(&Fixture { flaw: Flaw::None, clip: true, delay: DELAY }, &Config { linear: false, ..quick() });
    assert!(report.is_ok(), "{report}");
}

#[test]
fn a_lying_latency_fails_the_latency_check_with_both_numbers() {
    assert_only_fails(Flaw::LyingLatency, Check::Latency, "declares 27 samples of latency but the impulse at sample");
    let report = check_with(&Fixture { flaw: Flaw::LyingLatency, clip: false, delay: DELAY }, &quick());
    let first = report.failures().next().unwrap();
    assert!(first.to_string().contains("came out 20 samples late"), "{first}");
    assert!(first.to_string().contains("at default settings on Mono"), "{first}");
}

#[test]
fn a_lying_latency_fails_with_a_click_train_too() {
    let report = check_with(
        &Fixture { flaw: Flaw::LyingLatency, clip: true, delay: DELAY },
        &Config { linear: false, ..quick() },
    );
    assert_eq!(failed_checks(&report), [Check::Latency], "{report}");
    assert!(report.to_string().contains("a click at sample"), "{report}");
}

#[test]
fn the_latency_tolerance_is_the_kinds_to_declare() {
    let lying = Fixture { flaw: Flaw::LyingLatency, clip: false, delay: DELAY };
    assert!(!check_with(&lying, &quick()).is_ok());
    // 7 samples off: a tolerance of 7 accepts it, 6 does not.
    assert!(check_with(&lying, &Config { latency_tolerance: 7, ..quick() }).is_ok());
    assert!(!check_with(&lying, &Config { latency_tolerance: 6, ..quick() }).is_ok());
}

#[test]
fn a_block_size_dependent_node_fails_the_block_size_check() {
    assert_only_fails(Flaw::BlockDependent, Check::BlockSizes, "blocks of 1 differ from blocks of 1024 at frame");
}

#[test]
fn an_allocating_node_fails_the_allocation_check() {
    assert_only_fails(Flaw::Allocates, Check::Allocation, "`process or report` called the allocator");
    assert_only_fails(Flaw::AllocatesInSetParam, Check::Allocation, "`set_param` called the allocator");
}

#[test]
fn freeing_memory_on_the_audio_thread_fails_the_allocation_check_too() {
    assert_only_fails(Flaw::Frees, Check::Allocation, "(allocating or freeing)");
}

#[test]
fn a_leak_on_one_channel_only_fails_the_decay_check_on_stereo_and_names_the_channel() {
    let report = check_with(&Fixture { flaw: Flaw::RightLeaks, clip: false, delay: DELAY }, &quick());
    assert_eq!(failed_checks(&report), [Check::SilenceDecay], "{report}");
    assert!(report.failures().all(|r| r.channels == Some(Channels::Stereo)), "{report}");
    assert!(report.to_string().contains("channel 1 still has a peak of"), "{report}");
}

#[test]
fn a_nan_node_fails_the_finite_check_and_the_decay_check_does_not_run() {
    let report = check_with(&Fixture { flaw: Flaw::Nan, clip: false, delay: DELAY }, &quick());
    assert_eq!(failed_checks(&report), [Check::Finite], "{report}");
    assert!(report.to_string().contains("NaN at frame"), "{report}");
    assert!(report.skipped().all(|r| r.check == Check::SilenceDecay), "{report:?}");
    assert!(report.skipped().count() > 0);
}

#[test]
fn a_node_whose_silence_never_decays_fails_the_decay_check() {
    assert_only_fails(Flaw::NoDecay, Check::SilenceDecay, "still has a peak of");
}

#[test]
fn a_node_that_ignores_reset_fails_the_reset_check() {
    assert_only_fails(Flaw::IgnoresReset, Check::Reset, "after reset the output differs from a fresh node's");
}

#[test]
fn a_click_when_a_parameter_moves_fails_the_sweep_naming_the_parameter() {
    assert_only_fails(Flaw::Clicks, Check::Sweep, "made a click");
    let report = check_with(&Fixture { flaw: Flaw::Clicks, clip: false, delay: DELAY }, &quick());
    let first = report.failures().next().unwrap();
    assert_eq!(first.param.as_deref(), Some("gain_db"));
    assert!(first.to_string().contains("for parameter `gain_db`"), "{first}");
}

#[test]
fn a_panic_in_a_node_is_a_failure_not_a_crash() {
    let report = check_with(&Fixture { flaw: Flaw::Panics, clip: false, delay: DELAY }, &quick());
    assert!(!report.is_ok());
    assert!(report.to_string().contains("the node panicked: the fixture gave up"), "{report}");
}

#[test]
fn skipping_the_allocation_check_says_so() {
    let report =
        check_with(&Fixture { flaw: Flaw::Allocates, clip: false, delay: DELAY }, &quick().without_allocation_check());
    assert!(report.is_ok(), "{report}");
    assert!(report.skipped().all(|r| r.check == Check::Allocation && matches!(r.outcome, Outcome::Skipped(_))));
    assert!(report.skipped().count() > 0);
}

/// A kind whose description is inconsistent.
struct BadDescription(&'static str);

impl NodeKind for BadDescription {
    fn info(&self) -> NodeKindInfo {
        const BAD_DEFAULT: &[ParamSpec] =
            &[ParamSpec { name: "x", unit: Unit::None, min: 0.0, max: 1.0, default: 2.0, smooth_ms: 0.0 }];
        NodeKindInfo {
            id: if self.0 == "id" { "Not A Valid Id" } else { "test.bad" },
            summary: "bad",
            channels: if self.0 == "mono" { ChannelSupport::Mono } else { ChannelSupport::Any },
            params: if self.0 == "default" { BAD_DEFAULT } else { &[] },
            latency: LatencyRange { min: 0, max: if self.0 == "latency" { 0 } else { 1000 } },
            sidechain: false,
        }
    }

    fn create(&self, _config: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError> {
        if self.0 == "refuses" {
            return Err(NodeError::BadConfig("needs a model".into()));
        }
        Fixture { flaw: Flaw::None, clip: false, delay: DELAY }.create(&NodeConfig::default(), channels)
    }
}

#[test]
fn a_bad_description_fails_the_declaration_and_stops_there() {
    for (case, message) in [
        ("id", "is not a valid kind id"),
        ("default", "default outside 0 to 1"),
        ("refuses", "cannot be created on a Mono bus it claims to support: bad node config: needs a model"),
        ("mono", "must refuse a stereo bus"),
        ("latency", "declares 20 samples of latency outside the kind's range 0 to 0"),
    ] {
        let report = check_with(&BadDescription(case), &quick());
        assert_eq!(report.results.len(), 1, "{case}: nothing else runs on a kind that cannot run: {report}");
        assert_eq!(failed_checks(&report), [Check::Declaration], "{case}");
        assert!(report.to_string().contains(message), "{case}: {report}");
    }
}

#[test]
fn every_registered_kind_in_this_binary_passes() {
    let reports = check_all(|_| Config { noise_seconds: 2.0, ..Config::default() });
    assert!(reports.iter().any(|r| r.kind == "mock.null"), "the null kind is registered");
    for report in &reports {
        assert!(report.is_ok(), "{report}");
    }
}

/// The delay of the fixtures that hide a flaw behind a second of latency.
const LONG_DELAY: usize = 48_000;

#[test]
fn an_allocation_that_happens_only_at_the_minimum_setting_is_caught() {
    let report = check_with(&Fixture { flaw: Flaw::AllocatesAtMinimum, clip: false, delay: DELAY }, &quick());
    assert_eq!(failed_checks(&report), [Check::Allocation], "{report}");
    assert!(report.failures().all(|r| r.setting == Some(Setting::Min)), "{report}");
    assert!(report.to_string().contains("`process or report` called the allocator"), "{report}");
}

#[test]
fn a_block_size_dependent_node_with_a_second_of_latency_fails_instead_of_hiding_in_the_delay() {
    let report = check_with(&Fixture { flaw: Flaw::BlockDependent, clip: false, delay: LONG_DELAY }, &quick());
    assert!(failed_checks(&report).contains(&Check::BlockSizes), "{report}");
}

#[test]
fn a_node_with_a_second_of_latency_that_ignores_reset_fails() {
    let report = check_with(&Fixture { flaw: Flaw::IgnoresReset, clip: false, delay: LONG_DELAY }, &quick());
    assert_eq!(failed_checks(&report), [Check::Reset], "{report}");
}

#[test]
fn a_node_with_a_second_of_latency_that_clicks_fails_the_sweep() {
    let report = check_with(&Fixture { flaw: Flaw::Clicks, clip: false, delay: LONG_DELAY }, &quick());
    assert_eq!(failed_checks(&report), [Check::Sweep], "{report}");
}

#[test]
fn a_well_behaved_node_with_a_second_of_latency_passes() {
    let report = check_with(&Fixture { flaw: Flaw::None, clip: false, delay: LONG_DELAY }, &quick());
    assert!(report.is_ok(), "{report}");
}

/// A tremolo whose depth follows the input level: `1 + 0.1 sin(phase) (1 + level)`. It has two pieces of state, a phase
/// that advances every sample and a level that remembers the input.
struct Oscillator {
    /// Whether `reset` also forgets the level. The phase is always restored.
    forgets_level: bool,
}

impl NodeKind for Oscillator {
    fn info(&self) -> NodeKindInfo {
        NodeKindInfo {
            id: "test.oscillator",
            summary: "a tremolo fixture",
            channels: ChannelSupport::Any,
            params: &[],
            latency: LatencyRange { min: 0, max: 0 },
            sidechain: false,
        }
    }

    fn create(&self, _config: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError> {
        Ok(Box::new(OscillatorNode {
            forgets_level: self.forgets_level,
            phase: 0.0,
            level: vec![0.0; channels.count()],
        }))
    }
}

struct OscillatorNode {
    forgets_level: bool,
    phase: f64,
    level: Vec<f32>,
}

#[allow(clippy::cast_precision_loss)] // sample counts in a test stay far below 2^52
impl Node for OscillatorNode {
    fn latency(&self) -> u32 {
        0
    }

    fn set_param(&mut self, _param: ParamIndex, _value: f32) {}

    fn process(&mut self, block: &mut BlockMut<'_>) {
        let start = self.phase;
        let frames = block.frames();
        let step = std::f64::consts::TAU * 3.0 / 48_000.0;
        let mut channel = 0;
        let levels = &mut self.level;
        block.for_each_channel(|samples| {
            for (n, sample) in samples.iter_mut().enumerate() {
                #[allow(clippy::cast_possible_truncation)] // a gain near one
                let wobble = (start + step * n as f64).sin() as f32;
                let input = *sample;
                *sample = input * (1.0 + 0.1 * wobble * (1.0 + levels[channel]));
                levels[channel] = 0.999 * levels[channel] + 0.001 * input.abs();
            }
            channel += 1;
        });
        self.phase = (start + step * frames as f64) % std::f64::consts::TAU;
    }

    fn reset(&mut self) {
        // The phase goes back to where a new node starts, which is not where a node that has run is.
        self.phase = 0.0;
        if self.forgets_level {
            self.level.fill(0.0);
        }
    }
}

#[test]
fn an_oscillator_that_restores_its_phase_on_reset_passes() {
    let report = check_with(&Oscillator { forgets_level: true }, &quick());
    assert!(report.is_ok(), "{report}");
}

#[test]
fn an_oscillator_that_resets_its_phase_but_not_its_level_fails_the_reset_check() {
    let report = check_with(&Oscillator { forgets_level: false }, &quick());
    assert_eq!(failed_checks(&report), [Check::Reset], "{report}");
}
