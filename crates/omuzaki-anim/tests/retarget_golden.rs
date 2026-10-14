//! The retarget against the prototype it was ported from.
//!
//! `golden/retarget.json` holds synthetic tracker tracks and the output of the prototype's own `arkit.js` run under
//! Node, with its default options, for ten configurations (made by the prototype's generator script): calibrated and
//! uncalibrated, mirrored, smoothed and not, degenerate ranges, one-sided absent channels, NaN / infinities / 3e38 in the
//! track and in the head pose, odd `dt`, and a blink sweep from 0 to 1 and back. Each case feeds its track through the
//! Rust retarget and compares every channel of every frame, within 1e-5.
//!
//! What is compared is what the contract keeps from the prototype: the 52 per-channel values after calibration,
//! response, exaggeration, mirroring and smoothing (`arkit.js`'s `raw`), the head angles, the head shift and the lean.
//! The prototype's mouth/eye/brow formulas moved to the face painter and are not part of this stage.
//!
//! One deliberate difference: the contract promises every channel inside its range, and the prototype's head channels
//! are unbounded (a yaw of 3 rad at gain 1.4 gives 4.2). The prototype's head columns are clamped to the channel's
//! range before comparing, and the test insists that this really happens in the golden.
//!
//! Units: the prototype's head angles are degrees, its shift centimetres and its lean "degrees" (`leanPerCm` 0.9 per
//! centimetre toward the camera, which is 90 degrees per metre). The Rust channels are radians and metres, and the 1e-5
//! bound is applied in them. The golden's own rounding (6 decimals) leaves 5e-7 of that bound at most.

// A panic with a message is the right failure for a malformed golden; JSON numbers are f64 and the pipeline is f32.
#![allow(clippy::expect_used, clippy::cast_possible_truncation)]

use omuzaki_anim::retarget::{ArkitRetarget, default_calibration};
use omuzaki_anim_api::{
    ARKIT_COUNT, BlendMode, Calibration, Channel, ChannelMask, ChannelRange, DeviceProfile, HeadPose, Layer, Retarget,
    RetargetConfig, TrackFrame,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;
use serde_json::Value;

/// The acceptance bound: every channel, every frame (the requirement is 1e-4; it is held to 1e-5).
const TOLERANCE: f64 = 1e-5;

const GOLDEN: &str = include_str!("golden/retarget.json");

/// A number from the file. `null` spells NaN and the strings `"inf"` and `"-inf"` the infinities (JSON has neither).
fn number(v: &Value) -> f32 {
    match v.as_str() {
        Some("inf") => f32::INFINITY,
        Some("-inf") => f32::NEG_INFINITY,
        Some(other) => panic!("not a number: {other}"),
        None => v.as_f64().map_or(f32::NAN, |x| x as f32),
    }
}

fn channel(name: &str) -> Channel {
    Channel::from_name(name).expect("a real channel name")
}

fn numbers(v: &Value) -> Vec<f32> {
    v.as_array().expect("an array of numbers").iter().map(number).collect()
}

fn head_pose(v: &[f32]) -> HeadPose {
    HeadPose { yaw: v[0], pitch: v[1], roll: v[2], position: Some(Vec3::new(v[3], v[4], v[5])) }
}

/// The calibration a case describes, built the way a performer gets there: the webcam default, "calibrate neutral" for
/// the channels with a `neutral` value (the prototype's `calibrate()`), then a fitted range for the channels with one
/// (the prototype's `setRange()`), plus the rest head pose. Every other channel stays on its default gain.
fn calibration(config: &Value) -> Calibration {
    let mut cal = default_calibration(DeviceProfile::Webcam);
    if let Some(neutral) = config["neutral"].as_object() {
        let mut rest = TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Webcam);
        for (name, v) in neutral {
            rest.set(channel(name), number(v));
        }
        cal.rest_at(&rest);
    }
    if let Some(ranges) = config["ranges"].as_object() {
        for (name, range) in ranges {
            let range = numbers(range);
            cal.set_range(channel(name), ChannelRange { lo: range[0], hi: range[1] });
        }
    }
    cal.head_rest = head_pose(&numbers(&config["head_rest"]));
    cal
}

fn config(case: &Value) -> RetargetConfig {
    RetargetConfig {
        mirror: case["mirror"].as_bool().expect("mirror"),
        exaggeration: number(&case["exaggeration"]),
        smoothing: number(&case["smoothing"]),
        head_gain: number(&case["head_gain"]),
        ..RetargetConfig::default()
    }
}

fn track_frame(index: usize, frame: &Value, absent: &[Channel]) -> TrackFrame {
    let mut out = TrackFrame::empty(index as u64, Seconds(0.0), DeviceProfile::Webcam);
    for (info, v) in Channel::arkit().iter().zip(frame["blend"].as_array().expect("52 values")) {
        if !absent.contains(&info.channel) {
            out.set(info.channel, number(v));
        }
    }
    out.head = Some(head_pose(&numbers(&frame["head"])));
    out
}

/// The golden's columns after the 52 channels: the head channels, each with how many prototype units make one Rust unit
/// (degrees per radian, centimetres per metre; lean is in degrees).
const HEAD_COLUMNS: [(Channel, f64); 6] = [
    (Channel::HEAD_YAW, 180.0 / std::f64::consts::PI),
    (Channel::HEAD_PITCH, 180.0 / std::f64::consts::PI),
    (Channel::HEAD_ROLL, 180.0 / std::f64::consts::PI),
    (Channel::HEAD_SHIFT_X, 100.0),
    (Channel::HEAD_SHIFT_Y, 100.0),
    (Channel::LEAN, 180.0 / std::f64::consts::PI),
];

/// The Rust output for one frame in the golden's column order, in Rust units.
fn rust_row(layer: &Layer) -> Vec<f64> {
    let mut row: Vec<f64> = layer.values.arkit().iter().map(|&v| f64::from(v)).collect();
    row.extend(HEAD_COLUMNS.iter().map(|&(c, _)| f64::from(layer.values[c])));
    row
}

/// The prototype's row for one frame, converted to Rust units and held to each channel's range (the contract's side of
/// the bargain; the prototype's head columns are unbounded). The bound is stated in Rust units, so converting this side
/// keeps the conversion from loosening it. Also returns how many cells the range clamp changed.
fn prototype_row(row: &Value) -> (Vec<f64>, usize) {
    let mut cells: Vec<f64> = row.as_array().expect("a row").iter().map(|v| v.as_f64().expect("a number")).collect();
    let mut clamped = 0;
    for (cell, (channel, per_rust_unit)) in cells[ARKIT_COUNT..].iter_mut().zip(HEAD_COLUMNS) {
        let (min, max) = channel.range();
        let (converted, legal) = (*cell / per_rust_unit, f64::from(min)..=f64::from(max));
        clamped += usize::from(!legal.contains(&converted));
        *cell = converted.clamp(f64::from(min), f64::from(max));
    }
    (cells, clamped)
}

/// The channels a case's tracker frames supply, as the layer's mask should claim them: the absent ones are left alone,
/// and mirroring moves a missing left channel to the right (and the head's sign flips, not its presence).
fn claimed(absent: &[Channel], mirror: bool) -> ChannelMask {
    let mut tracker = ChannelMask::ARKIT;
    for c in absent {
        tracker.remove(*c);
    }
    let head = ChannelMask::HEAD | ChannelMask::of(Channel::LEAN);
    (if mirror { tracker.mirrored() } else { tracker }) | head
}

#[test]
fn the_rust_retarget_matches_the_prototype_channel_by_channel_within_1e_5() {
    let golden: Value = serde_json::from_str(GOLDEN).expect("the golden is valid JSON");
    let names = golden["names"].as_array().expect("names");
    for (info, name) in Channel::arkit().iter().zip(names) {
        assert_eq!(name.as_str(), Some(info.name), "the golden's channel order is the table's");
    }

    let (mut overall, mut clamped_cells) = (0.0_f64, 0);
    for case in golden["cases"].as_array().expect("cases") {
        let setup = &case["config"];
        let label = setup["name"].as_str().expect("a case name");
        let frames = golden["tracks"][setup["track"].as_str().expect("a track name")].as_array().expect("frames");
        let absent: Vec<Channel> =
            setup["absent"].as_array().expect("absent").iter().map(|n| channel(n.as_str().expect("a name"))).collect();
        let mirror = setup["mirror"].as_bool().expect("mirror");
        let mut retarget = ArkitRetarget::with(calibration(setup), config(setup));
        let expected = case["expected"].as_array().expect("expected rows");
        assert_eq!(expected.len(), frames.len(), "{label}");

        let mut worst = (0.0_f64, 0, 0);
        let mut out = Layer::absent(BlendMode::Override);
        for (k, (frame, want)) in frames.iter().zip(expected).enumerate() {
            retarget.apply(&track_frame(k, frame, &absent), number(&frame["dt"]), &mut out);
            let got = rust_row(&out);
            let (want, clamped) = prototype_row(want);
            clamped_cells += clamped;
            assert_eq!(got.len(), want.len());
            for (column, (g, w)) in got.iter().zip(&want).enumerate() {
                let error = (g - w).abs();
                assert!(
                    error <= TOLERANCE,
                    "{label}: frame {k}, column {column} ({}): rust {g}, prototype {w}",
                    names.get(column).and_then(Value::as_str).unwrap_or("head/lean")
                );
                if error > worst.0 {
                    worst = (error, k, column);
                }
            }
            // Whatever the input, the layer stays inside the contract.
            assert!(out.values.as_slice().iter().all(|v| v.is_finite()), "{label}: frame {k} is not finite");
            assert_eq!(out.mask, claimed(&absent, mirror), "{label}: frame {k}: tracked channels, head and lean");
        }
        println!("{label}: worst error {:.2e} (frame {}, column {})", worst.0, worst.1, worst.2);
        overall = overall.max(worst.0);
        assert_eq!((out.weight, out.mode), (1.0, BlendMode::Override), "{label}");
    }
    println!("all cases: worst error {overall:.2e}; {clamped_cells} prototype head cells clamped to their ranges");
    assert!(clamped_cells > 0, "the golden must reach past the head channels' ranges, or the clamp is untested");
}
