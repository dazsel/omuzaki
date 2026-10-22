//! Every effect kind passes the node conformance suite of `omuzaki-audio-mock` (latency honesty, block-size invariance,
//! no allocation, finite and stable, silence decay, reset, parameter sweeps without clicks), on every channel count it
//! claims and at its default, minimum and maximum settings.

use omuzaki_audio_api::node::{NodeKind, node_kinds};
use omuzaki_audio_fx::compressor::CompressorKind;
use omuzaki_audio_fx::eq::EqKind;
use omuzaki_audio_fx::gate::GateKind;
use omuzaki_audio_fx::limiter::LimiterKind;
use omuzaki_audio_fx::radio::RadioKind;
use omuzaki_audio_fx::robot::RobotKind;
use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{BlockMut, ConfigValue, NodeConfig};
use omuzaki_audio_mock::conformance::{Check, CheckResult, Config, CountingAlloc, Report, Setting, check_with};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn assert_conforms(kind: &dyn NodeKind, config: &Config) {
    assert_conforms_except(kind, config, |_| false);
}

/// The suite, with the failures `excused` names left out (each excuse is backed by a direct test).
fn assert_conforms_except(kind: &dyn NodeKind, config: &Config, excused: impl Fn(&CheckResult) -> bool) {
    let report: Report = check_with(kind, config);
    let failures: Vec<String> = report
        .failures()
        .filter(|r| !excused(r))
        .map(|r| format!("{} {:?} {:?} {:?}: {:?}", r.check, r.setting, r.channels, r.param, r.outcome))
        .collect();
    assert!(failures.is_empty(), "{} fails conformance:\n{}", report.kind, failures.join("\n"));
    for skipped in report.skipped() {
        eprintln!("{}: skipped {} {:?}: {:?}", report.kind, skipped.check, skipped.setting, skipped.outcome);
    }
}

fn eq_bands(n: i64) -> Config {
    let mut node_config = NodeConfig::default();
    node_config.0.insert("bands".into(), ConfigValue::Int(n));
    Config { node_config, ..Config::default() }
}

#[test]
fn eq_conforms_with_one_band() {
    assert_conforms(&EqKind, &eq_bands(1));
}

/// With eight bands at their maximum (eight +24 dB bells of Q 18 at 20 kHz, +192 dB) the impulse response is a
/// resonance that builds for hundreds of samples, so the suite's 10%-of-peak onset lands on the build-up. That is not
/// delay: the response starts at the impulse, which `eq_at_eight_bands_maximum_answers_at_the_impulse` checks.
#[test]
fn eq_conforms_with_eight_bands() {
    assert_conforms_except(&EqKind, &eq_bands(8), |r| r.check == Check::Latency && r.setting == Some(Setting::Max));
}

#[test]
fn eq_at_eight_bands_maximum_answers_at_the_impulse() {
    for channels in [Channels::Mono, Channels::Stereo] {
        let mut node = EqKind.create(&eq_bands(8).node_config, channels).unwrap();
        for (at, p) in EqKind.info().params.iter().enumerate() {
            node.set_param(omuzaki_audio_api::node::ParamIndex(u16::try_from(at).unwrap()), p.max);
        }
        node.reset();
        let mut x = vec![0.0f32; 64];
        x[0] = 1.0;
        node.process(&mut BlockMut::mono(&mut x, 0));
        // Eight bells' b0 multiplied: well over the impulse itself, on the impulse's own sample.
        assert!(x[0] > 1.0, "{}", x[0]);
    }
}

#[test]
fn gate_conforms() {
    assert_conforms(&GateKind, &Config { linear: false, ..Config::default() });
}

#[test]
fn compressor_conforms() {
    assert_conforms(&CompressorKind, &Config { linear: false, ..Config::default() });
}

#[test]
fn robot_conforms() {
    assert_conforms(&RobotKind, &Config { linear: false, ..Config::default() });
}

/// The telephone band's filters spread a click's onset over their group delay (3 samples), so the onset is measured
/// with 4 samples (0.08 ms) of tolerance; `radio_answers_at_the_click` checks that the response starts on the click.
#[test]
fn radio_conforms() {
    assert_conforms(&RadioKind, &Config { linear: false, latency_tolerance: 4, ..Config::default() });
}

#[test]
fn radio_answers_at_the_click() {
    let mut node = RadioKind.create(&NodeConfig::default(), Channels::Mono).unwrap();
    let mut x = vec![0.0f32; 64];
    x[10] = 0.8;
    node.process(&mut BlockMut::mono(&mut x, 0));
    assert!(x[..10].iter().all(|s| *s == 0.0) && x[10] != 0.0, "{:?}", &x[..16]);
}

#[test]
fn limiter_conforms() {
    assert_conforms(&LimiterKind, &Config { linear: false, ..Config::default() });
}

#[test]
fn every_effect_is_registered_once() {
    omuzaki_audio_fx::link();
    let ids: Vec<&str> = node_kinds().iter().map(|k| k.info().id).filter(|id| id.starts_with("fx.")).collect();
    for id in ["fx.compressor", "fx.eq", "fx.gate", "fx.limiter", "fx.radio", "fx.robot"] {
        assert_eq!(ids.iter().filter(|i| **i == id).count(), 1, "{id} in {ids:?}");
    }
}
