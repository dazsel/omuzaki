//! Switching systems off, frame rates, wild settings, and the recorded golden.

use omuzaki_anim_api::{Bus, Channel, ChannelMask, Cues, LifeConfig, Tick};

use crate::life::Life;

use super::harness::{bus_of, busy, laughing, minus, peak, series, simulate, still};

/// Every system switched off, so only the springs are left.
fn all_systems_off() -> LifeConfig {
    let mut config = LifeConfig::new(7);
    let s = &mut config.systems;
    (s.blink, s.gaze, s.gaze_lead, s.coupling, s.laugh, s.speech, s.breathing, s.texture) =
        (false, false, false, false, false, false, false, false);
    config
}

#[test]
fn with_every_system_off_only_the_springs_remain_and_they_settle_on_the_source() {
    let mut life = Life::new(all_systems_off());
    let mut worst = 0.0_f32;
    simulate(&mut life, 30.0, 900, &mut busy, |tick, source, bus| {
        if tick.index >= 450 && (tick.index < 270 || tick.index > 330) {
            for c in ChannelMask::ALL {
                // Springs lag a moving source, so compare only where the busy scene holds still for a while.
                let steady = (5.5..7.5).contains(&(tick.time.0 as f32)) || tick.time.0 as f32 >= 20.0;
                if steady {
                    worst = worst.max((bus.values[c] - source.values[c]).abs());
                }
            }
        }
    });
    assert!(worst < 1e-3, "off means off: {worst}");
}

#[test]
fn each_system_can_be_switched_off_on_its_own() {
    let moved = |config: LifeConfig, scene: fn(&Tick) -> (Bus, Cues), channels: ChannelMask, from_frame: u64| {
        let mut life = Life::new(config);
        let mut any = false;
        simulate(&mut life, 30.0, 900, &mut { scene }, |tick, source, bus| {
            if tick.index >= from_frame {
                any |= channels.into_iter().any(|c| (bus.values[c] - source.values[c]).abs() > 1e-3);
            }
        });
        any
    };
    let off = |f: fn(&mut omuzaki_anim_api::LifeSystems)| {
        let mut config = LifeConfig::new(7);
        f(&mut config.systems);
        config
    };
    let blink = ChannelMask::of(Channel::EYE_BLINK_LEFT) | ChannelMask::of(Channel::EYE_BLINK_RIGHT);
    assert!(moved(LifeConfig::new(7), still, blink, 0));
    assert!(!moved(off(|s| s.blink = false), still, blink, 0), "blink");

    assert!(moved(LifeConfig::new(7), still, ChannelMask::GAZE, 0));
    assert!(!moved(off(|s| s.gaze = false), still, ChannelMask::GAZE, 0), "gaze");

    let breath = ChannelMask::of(Channel::BREATH);
    assert!(!moved(off(|s| s.breathing = false), still, breath, 0), "breathing");

    let texture = ChannelMask::BROWS | ChannelMask::of(Channel::HEAD_YAW) | ChannelMask::of(Channel::EYE_WIDE_LEFT);
    assert!(moved(LifeConfig::new(7), still, texture, 0));
    assert!(!moved(off(|s| s.texture = false), still, texture, 0), "texture");

    // Scenes that need the behaviour to have something to react to.
    let laugh = |t: &Tick| laughing(0.9, 0.6)(t);
    let squint = ChannelMask::of(Channel::EYE_SQUINT_LEFT) | ChannelMask::of(Channel::EYE_ARC_LEFT);
    let mut quiet = off(|s| s.laugh = false);
    quiet.systems.texture = false;
    assert!(!moved(quiet, laugh, squint, 60), "laugh");

    let turning =
        |tick: &Tick| (bus_of(|v| v[Channel::HEAD_YAW] = if tick.time.0 >= 2.0 { 0.35 } else { 0.0 }), Cues::default());
    let mut no_lead = off(|s| s.gaze_lead = false);
    no_lead.systems.gaze = false;
    assert!(!moved(no_lead, turning, ChannelMask::GAZE, 0), "gaze_lead");
    let mut lead = LifeConfig::new(7);
    lead.systems.gaze = false;
    assert!(moved(lead, turning, ChannelMask::GAZE, 0), "the eyes still lead with the other gaze behaviours off");
}

#[test]
fn a_non_finite_eye_look_input_never_comes_out_of_the_gaze_nudge() {
    // A -inf or NaN on an eyeLook channel nobody drives used to become NaN inside the nudge that moves its pair.
    for channel in ChannelMask::GAZE {
        for bad in [f32::NEG_INFINITY, f32::INFINITY, f32::NAN] {
            let mut scene = move |_: &Tick| (bus_of(|v| v[channel] = bad), Cues::default());
            simulate(&mut Life::new(LifeConfig::new(3)), 30.0, 150, &mut scene, |tick, _, bus| {
                for c in ChannelMask::ALL {
                    let v = bus.values[c];
                    assert!(v.is_finite(), "frame {}: {c} = {v} with {channel} = {bad} coming in", tick.index);
                }
            });
        }
    }
}

#[test]
fn every_frame_rate_is_stable_and_keeps_every_channel_legal() {
    for fps in [15.0, 30.0, 60.0, 120.0] {
        let mut life = Life::new(LifeConfig::new(2));
        simulate(&mut life, fps, (fps * 30.0) as u64, &mut busy, |tick, _, bus| {
            for c in ChannelMask::ALL {
                let (lo, hi) = c.range();
                let v = bus.values[c];
                assert!(v.is_finite() && v >= lo - 1e-6 && v <= hi + 1e-6, "fps {fps} frame {}: {c} = {v}", tick.index);
            }
        });
    }
}

#[test]
fn the_head_step_response_does_not_depend_on_the_frame_rate() {
    for fps in [15.0_f32, 30.0, 60.0, 120.0] {
        let run = |turn: bool| {
            let mut scene = move |tick: &Tick| {
                (
                    bus_of(|v| v[Channel::HEAD_YAW] = if turn && tick.time.0 >= 9.0 { 0.349 } else { 0.0 }),
                    Cues::default(),
                )
            };
            series(LifeConfig::new(1), fps, 13.0, &mut scene, |b| b.values[Channel::HEAD_YAW])
        };
        let (with, without) = (run(true), run(false));
        let response = minus(&with, &without);
        let over = (peak(&response[(9.0 * fps) as usize..]) - 0.349) / 0.349 * 100.0;
        let settled = response[(12.0 * fps) as usize];
        assert!((4.0..=22.0).contains(&over), "fps {fps}: overshoot {over} %");
        assert!((settled - 0.349).abs() < 0.008, "fps {fps}: settled at {settled}");
    }
}

#[test]
fn wild_settings_cannot_destabilise_the_springs() {
    let mut config = LifeConfig::new(4);
    let wild = omuzaki_anim_api::Dynamics { freq_hz: 400.0, damping: 9.0 };
    let springs = &mut config.params.springs;
    (springs.eye, springs.mouth, springs.brow, springs.head, springs.lean, springs.gaze, springs.slow) =
        (wild, wild, wild, wild, wild, wild, wild);
    simulate(&mut Life::new(config), 15.0, 450, &mut busy, |_, _, bus| {
        assert!(ChannelMask::ALL.into_iter().all(|c| bus.values[c].is_finite()));
    });
}

/// The golden: a busy 20 s at 30 fps with seed 1, sampled at a few frames and channels. Recorded on x86-64 Linux with
/// Rust 1.99; decision 0009 allows 1e-5 per channel across platforms and toolchains (libm differs in the last bits
/// of `sin`, `exp` and `ln`). If a deliberate change moves these, print them with the ignored `print_golden` test below
/// (`cargo test -p omuzaki-anim print_golden -- --ignored --nocapture`) and paste the new numbers in the same PR.
const GOLDEN_FRAMES: [u64; 6] = [0, 129, 219, 300, 418, 599];
const GOLDEN_CHANNELS: [Channel; 8] = [
    Channel::EYE_BLINK_LEFT,
    Channel::EYE_WIDE_LEFT,
    Channel::CHEEK_SQUINT_RIGHT,
    Channel::BROW_OUTER_UP_LEFT,
    Channel::MOUTH_SMILE_RIGHT,
    Channel::HEAD_PITCH,
    Channel::EYE_LOOK_OUT_LEFT,
    Channel::BREATH,
];
const GOLDEN: [[f32; 8]; 6] = [
    [0.0, 0.0, 0.0525, 0.031836, 0.130439, -0.014064, 0.009969, 0.948029],
    [0.977964, 0.003194, 0.0525, 0.0, 0.148697, -0.011721, 0.0, 0.811429],
    [0.772165, 0.0, 0.35, 0.001842, 1.0, -0.005533, 0.0, 0.967284],
    [0.0, 0.013058, 0.0525, 0.011155, 0.17167, 0.010769, 0.0, 0.242226],
    [0.904405, 0.0, 0.35, 0.047429, 1.0, -0.013535, 0.0, 0.12858],
    [0.0, 0.0, 0.0525, 0.07079, 0.195573, -0.023589, 0.0, 0.889004],
];

fn record_golden() -> Vec<[f32; 8]> {
    let mut rows = Vec::new();
    simulate(&mut Life::new(LifeConfig::new(1)), 30.0, 600, &mut busy, |tick, _, bus| {
        if GOLDEN_FRAMES.contains(&tick.index) {
            rows.push(GOLDEN_CHANNELS.map(|c| bus.values[c]));
        }
    });
    rows
}

#[test]
fn the_layer_matches_recorded_values_within_the_cross_platform_tolerance() {
    for (row, expected) in record_golden().iter().zip(GOLDEN) {
        for ((value, want), channel) in row.iter().zip(expected).zip(GOLDEN_CHANNELS) {
            assert!((value - want).abs() <= 1e-5, "{channel}: {value} against the recorded {want}");
        }
    }
}

#[test]
#[ignore = "prints the golden table to paste above"]
fn print_golden() {
    let mut blink_frames = Vec::new();
    simulate(&mut Life::new(LifeConfig::new(1)), 30.0, 600, &mut busy, |tick, _, bus| {
        if bus.values[Channel::EYE_BLINK_LEFT] > 0.3 {
            blink_frames.push(tick.index);
        }
    });
    println!("blinking at frames {blink_frames:?}");
    println!("{:#?}", record_golden());
}
