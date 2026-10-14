//! Behaviour tests for [`ArkitRetarget`]. The numbers against the prototype are in `tests/retarget_golden.rs`.

#![allow(clippy::float_cmp)] // exact values: zeros, ones and copies

use omuzaki_anim_api::{
    Calibration, Channel, ChannelRange, EyeAngles, HeadPose, LostPolicy, RetargetConfig, TrackStatus,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;

use super::*;

const DT: f32 = 1.0 / 30.0;

fn frame(entries: &[(Channel, f32)]) -> TrackFrame {
    let mut f = TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Arkit);
    for &(c, v) in entries {
        f.set(c, v);
    }
    f
}

fn lost() -> TrackFrame {
    TrackFrame::lost(0, Seconds(0.0), DeviceProfile::Arkit)
}

fn unsmoothed() -> RetargetConfig {
    RetargetConfig { smoothing: 0.0, ..RetargetConfig::default() }
}

fn run(r: &mut ArkitRetarget, f: &TrackFrame) -> Layer {
    run_for(r, f, DT)
}

fn run_for(r: &mut ArkitRetarget, f: &TrackFrame, dt: f32) -> Layer {
    let mut out = Layer::absent(BlendMode::Override);
    r.apply(f, dt, &mut out);
    out
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

fn pose(yaw: f32, z: f32) -> HeadPose {
    HeadPose { yaw, pitch: 0.0, roll: 0.0, position: Some(Vec3::new(0.0, 0.0, z)) }
}

#[test]
fn a_tracked_frame_drives_exactly_the_channels_the_source_supplies() {
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    let out = run(&mut r, &frame(&[(Channel::JAW_OPEN, 0.5)]));
    assert_eq!(out.mask, ChannelMask::of(Channel::JAW_OPEN), "an absent channel is never 'measured zero'");
    assert_eq!((out.weight, out.mode), (1.0, BlendMode::Override));
    assert_eq!(out.values[Channel::JAW_OPEN], 0.5, "the first sample passes through the filter");
}

#[test]
fn ranges_normalise_then_responses_shape_then_exaggeration_lifts() {
    let mut cal = Calibration::identity(DeviceProfile::Webcam);
    cal.set_range(Channel::JAW_OPEN, ChannelRange { lo: 0.1, hi: 0.5 });
    cal.set_range(Channel::MOUTH_SMILE_LEFT, ChannelRange { lo: 0.0, hi: 0.5 });
    let mut config = RetargetConfig { exaggeration: 2.0, ..unsmoothed() };
    config.responses.insert(Channel::CHEEK_PUFF, Response { dead: 0.0, sat: 1.0, gamma: 1.0, invert: true });
    let mut r = ArkitRetarget::with(cal, config);
    let out = run(
        &mut r,
        &frame(&[(Channel::JAW_OPEN, 0.3), (Channel::MOUTH_SMILE_LEFT, 0.125), (Channel::CHEEK_PUFF, 0.25)]),
    );
    assert!(near(out.values[Channel::JAW_OPEN], 0.5), "(0.3 - 0.1) / 0.4, and jaw stays literal");
    assert!(near(out.values[Channel::MOUTH_SMILE_LEFT], 0.5), "0.125 / 0.5 = 0.25, then the square root");
    assert!(
        near(out.values[Channel::CHEEK_PUFF], 1.0 - 0.5),
        "a cheek is exaggerated (sqrt 0.25) and the response then inverts it"
    );
}

/// The left blink after one unsmoothed frame on `cal` whose raw value normalises to `fraction` of the range.
/// Exaggeration is on to show that blinks ignore it.
fn blink_at(cal: &Calibration, fraction: f32) -> f32 {
    let range = cal.range(Channel::EYE_BLINK_LEFT);
    let raw = range.lo + fraction * (range.hi - range.lo);
    let mut r = ArkitRetarget::with(*cal, RetargetConfig { exaggeration: 3.0, ..unsmoothed() });
    run(&mut r, &frame(&[(Channel::EYE_BLINK_LEFT, raw)])).values[Channel::EYE_BLINK_LEFT]
}

#[test]
fn an_uncalibrated_blink_has_the_prototypes_gain_path_window_0_28_to_0_8() {
    for cal in [Calibration::identity(DeviceProfile::Arkit), default_calibration(DeviceProfile::Webcam)] {
        assert_eq!(blink_at(&cal, 0.2), 0.0, "looking down is not a half blink");
        assert!(near(blink_at(&cal, 0.28), 0.0) && near(blink_at(&cal, 0.54), 0.5), "linear from 0.28");
        assert!(near(blink_at(&cal, 0.8), 1.0) && blink_at(&cal, 0.9) == 1.0, "and the eye is shut from 0.8");
    }
}

#[test]
fn a_blink_with_a_fitted_range_has_the_wider_window_0_25_to_0_85() {
    let mut cal = Calibration::identity(DeviceProfile::Webcam);
    cal.set_range(Channel::EYE_BLINK_LEFT, ChannelRange { lo: 0.02, hi: 0.9 });
    assert_eq!(blink_at(&cal, 0.2), 0.0);
    assert!(near(blink_at(&cal, 0.25), 0.0) && near(blink_at(&cal, 0.55), 0.5), "linear from 0.25");
    assert!(near(blink_at(&cal, 0.85), 1.0));
}

#[test]
fn calibrating_neutral_keeps_the_gain_path_window_and_a_fitted_range_switches_to_the_other() {
    let mut cal = default_calibration(DeviceProfile::Webcam);
    cal.rest_at(&frame(&[(Channel::EYE_BLINK_LEFT, 0.07)]));
    assert_eq!(blink_at(&cal, 0.26), 0.0, "rest_at moves lo and keeps the span: still the 0.28 window");
    cal.set_range(Channel::EYE_BLINK_LEFT, ChannelRange { lo: 0.07, hi: 0.9 });
    assert!(near(blink_at(&cal, 0.26), 0.01 / 0.6), "a fitted range: the 0.25 window");
}

#[test]
fn the_blink_window_follows_set_calibration_and_is_chosen_per_channel() {
    let mut r = ArkitRetarget::with(Calibration::identity(DeviceProfile::Arkit), unsmoothed());
    let both = frame(&[(Channel::EYE_BLINK_LEFT, 0.265), (Channel::EYE_BLINK_RIGHT, 0.265 * 0.9)]);
    let before = run(&mut r, &both).values;
    assert_eq!((before[Channel::EYE_BLINK_LEFT], before[Channel::EYE_BLINK_RIGHT]), (0.0, 0.0), "below 0.28 and 0.25");
    let mut cal = *r.calibration();
    cal.set_range(Channel::EYE_BLINK_RIGHT, ChannelRange { lo: 0.0, hi: 0.9 });
    r.set_calibration(cal);
    let after = run(&mut r, &both).values;
    assert_eq!(after[Channel::EYE_BLINK_LEFT], 0.0, "the left eye is still on the gain path");
    assert!(near(after[Channel::EYE_BLINK_RIGHT], 0.015 / 0.6), "the right eye has a fitted range now");
}

#[test]
fn one_eye_can_wink() {
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    let out = run(&mut r, &frame(&[(Channel::EYE_BLINK_LEFT, 1.0), (Channel::EYE_BLINK_RIGHT, 0.0)]));
    assert_eq!((out.values[Channel::EYE_BLINK_LEFT], out.values[Channel::EYE_BLINK_RIGHT]), (1.0, 0.0));
}

#[test]
fn a_config_response_replaces_the_built_in_one() {
    let mut config = unsmoothed();
    config.responses.insert(Channel::EYE_BLINK_LEFT, Response::default());
    let mut r = ArkitRetarget::with(Calibration::identity(DeviceProfile::Arkit), config);
    let out = run(&mut r, &frame(&[(Channel::EYE_BLINK_LEFT, 0.2)]));
    assert!(near(out.values[Channel::EYE_BLINK_LEFT], 0.2), "no dead zone any more");
}

#[test]
fn mirroring_applies_the_trackers_own_range_and_then_swaps_sides() {
    let mut cal = Calibration::identity(DeviceProfile::Webcam);
    cal.set_range(Channel::MOUTH_SMILE_LEFT, ChannelRange { lo: 0.0, hi: 0.5 });
    cal.set_range(Channel::MOUTH_SMILE_RIGHT, ChannelRange { lo: 0.0, hi: 1.0 });
    let mut r = ArkitRetarget::with(cal, RetargetConfig { mirror: true, ..unsmoothed() });
    let out = run(&mut r, &frame(&[(Channel::MOUTH_SMILE_LEFT, 0.25)]));
    assert!(near(out.values[Channel::MOUTH_SMILE_RIGHT], 0.5), "normalised by the LEFT range it was measured with");
    assert_eq!(out.values[Channel::MOUTH_SMILE_LEFT], 0.0);
    assert_eq!(out.mask, ChannelMask::of(Channel::MOUTH_SMILE_RIGHT));
}

#[test]
fn smoothing_lags_a_step_and_the_fast_family_lags_less() {
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    let rest = frame(&[(Channel::JAW_OPEN, 0.0), (Channel::BROW_INNER_UP, 0.0)]);
    let open = frame(&[(Channel::JAW_OPEN, 1.0), (Channel::BROW_INNER_UP, 1.0)]);
    run(&mut r, &rest);
    let out = run(&mut r, &open);
    let (jaw, brow) = (out.values[Channel::JAW_OPEN], out.values[Channel::BROW_INNER_UP]);
    assert!(brow > 0.0 && brow < jaw && jaw < 1.0, "jaw {jaw} brow {brow}");
    let mut raw = ArkitRetarget::with(Calibration::identity(DeviceProfile::Arkit), unsmoothed());
    run(&mut raw, &rest);
    assert_eq!(run(&mut raw, &open).values[Channel::JAW_OPEN], 1.0, "smoothing 0 is raw");
}

#[test]
fn head_angles_are_measured_from_rest_scaled_by_gain_and_mirror_flips_yaw_and_roll_only() {
    let mut cal = Calibration::identity(DeviceProfile::Arkit);
    cal.head_rest = HeadPose { yaw: 0.1, pitch: 0.0, roll: 0.0, position: None };
    let mut f = frame(&[]);
    f.head = Some(HeadPose { yaw: 0.3, pitch: 0.1, roll: -0.2, position: None });
    let config = RetargetConfig { head_gain: 2.0, ..unsmoothed() };
    let plain = run(&mut ArkitRetarget::with(cal, config.clone()), &f);
    assert!(near(plain.values[Channel::HEAD_YAW], 0.4) && near(plain.values[Channel::HEAD_PITCH], 0.2));
    assert!(near(plain.values[Channel::HEAD_ROLL], -0.4));
    assert_eq!(plain.mask, ChannelMask::from_channels(&[Channel::HEAD_YAW, Channel::HEAD_PITCH, Channel::HEAD_ROLL]));
    let mirrored = run(&mut ArkitRetarget::with(cal, RetargetConfig { mirror: true, ..config }), &f);
    assert!(near(mirrored.values[Channel::HEAD_YAW], -0.4) && near(mirrored.values[Channel::HEAD_ROLL], 0.4));
    assert!(near(mirrored.values[Channel::HEAD_PITCH], 0.2));
}

#[test]
fn shift_and_lean_need_a_position_in_both_the_frame_and_the_rest_pose() {
    let mut cal = Calibration::identity(DeviceProfile::Arkit);
    let mut f = frame(&[]);
    f.head = Some(HeadPose { position: Some(Vec3::new(0.02, -0.01, 0.6)), ..HeadPose::default() });
    let mut r = ArkitRetarget::with(cal, unsmoothed());
    assert!(!run(&mut r, &f).mask.contains(Channel::LEAN), "no rest position, no zero to measure from");
    cal.head_rest = pose(0.0, 0.5);
    let out = run(&mut ArkitRetarget::with(cal, unsmoothed()), &f);
    assert!(near(out.values[Channel::LEAN], 0.1 * 90.0_f32.to_radians()), "toward the camera is positive");
    assert!(near(out.values[Channel::HEAD_SHIFT_X], 0.02) && near(out.values[Channel::HEAD_SHIFT_Y], -0.01));
    let mut no_depth = f;
    no_depth.head = Some(HeadPose::default());
    assert!(!run(&mut ArkitRetarget::with(cal, unsmoothed()), &no_depth).mask.contains(Channel::LEAN));
}

#[test]
fn head_angles_shift_and_lean_are_clamped_to_their_channels_ranges() {
    let mut cal = Calibration::identity(DeviceProfile::Arkit);
    cal.head_rest = pose(0.0, 0.5);
    let mut f = frame(&[]);
    f.head = Some(HeadPose { yaw: 3.0, pitch: -2.0, roll: 2.0, position: Some(Vec3::new(0.9, -0.9, 2.0)) });
    let limits = [
        (Channel::HEAD_YAW, 1.75),
        (Channel::HEAD_PITCH, -1.2),
        (Channel::HEAD_ROLL, 1.0),
        (Channel::HEAD_SHIFT_X, 0.5),
        (Channel::HEAD_SHIFT_Y, -0.5),
        (Channel::LEAN, 0.6),
    ];
    for config in [unsmoothed(), RetargetConfig::default()] {
        // Gain 1.4 would make the yaw 4.2 and the lean 2.4 radians.
        let config = RetargetConfig { head_gain: 1.4, ..config };
        let plain = run(&mut ArkitRetarget::with(cal, config.clone()), &f);
        let mirrored = run(&mut ArkitRetarget::with(cal, RetargetConfig { mirror: true, ..config }), &f);
        for (channel, limit) in limits {
            assert_eq!(plain.values[channel], limit, "{channel}");
            let flipped = matches!(channel, Channel::HEAD_YAW | Channel::HEAD_ROLL | Channel::HEAD_SHIFT_X);
            assert_eq!(mirrored.values[channel], if flipped { -limit } else { limit }, "{channel} mirrored");
        }
    }
}

#[test]
fn head_motion_is_smoothed_in_the_prototypes_units_not_in_radians() {
    // The speed term of the filter depends on the signal's units; this pins degrees for angles.
    let config = RetargetConfig::default();
    let params = OneEuroParams::for_smoothing(config.smoothing, false).expect("smoothing is on");
    let mut r = ArkitRetarget::with(Calibration::identity(DeviceProfile::Arkit), config);
    let at = |yaw: f32| {
        let mut f = frame(&[]);
        f.head = Some(HeadPose { yaw, ..HeadPose::default() });
        f
    };
    run(&mut r, &at(0.0));
    let got = run(&mut r, &at(0.5)).values[Channel::HEAD_YAW];
    let mut reference = OneEuro::default();
    reference.step(params, 0.0, DT);
    let want = reference.step(params, 0.5_f32.to_degrees(), DT).to_radians();
    assert!(near(got, want), "{got} vs {want}");
    let mut in_radians = OneEuro::default();
    in_radians.step(params, 0.0, DT);
    assert!(!near(got, in_radians.step(params, 0.5, DT)), "and that differs from filtering radians");
}

#[test]
fn gaze_angles_become_look_channels_only_when_the_source_sends_no_look_blendshapes() {
    let mut f = frame(&[]);
    f.eyes = Some(EyeAngles { left_yaw: 0.35, left_pitch: 0.0, right_yaw: 0.35, right_pitch: 0.0 });
    let mut r = ArkitRetarget::with(Calibration::identity(DeviceProfile::Arkit), unsmoothed());
    let out = run(&mut r, &f);
    let (x, y) = out.values.gaze();
    assert!(near(x, 0.35 / LOOK_FULL_RAD) && y == 0.0, "toward image-right is toward the character's left");
    assert_eq!(out.mask, ChannelMask::GAZE);
    f.set(Channel::EYE_LOOK_UP_LEFT, 0.5);
    assert!(!run(&mut r, &f).mask.contains(Channel::EYE_LOOK_OUT_LEFT), "blendshapes win over angles");
}

#[test]
fn gaze_angles_are_smoothed_like_gaze_blendshapes() {
    let at = |yaw: f32| {
        let mut f = frame(&[]);
        f.eyes = Some(EyeAngles { left_yaw: yaw, left_pitch: 0.0, right_yaw: yaw, right_pitch: 0.0 });
        f
    };
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    run(&mut r, &at(0.0));
    let x = run(&mut r, &at(LOOK_FULL_RAD)).values.gaze().0;
    assert!(x > 0.0 && x < 1.0, "{x}");
}

#[test]
fn a_lost_face_holds_through_the_grace_period_then_eases_out_and_hands_over() {
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    run(&mut r, &frame(&[(Channel::JAW_OPEN, 0.8)]));
    let held = run_for(&mut r, &lost(), 0.1);
    assert_eq!((held.values[Channel::JAW_OPEN], held.weight), (0.8, 1.0), "inside the grace period nothing changes");
    let halfway = run_for(&mut r, &lost(), 0.15 + 0.25);
    assert!(near(halfway.values[Channel::JAW_OPEN], 0.4) && near(halfway.weight, 0.5), "{halfway:?}");
    let gone = run_for(&mut r, &lost(), 1.0);
    assert_eq!((gone.weight, gone.values[Channel::JAW_OPEN]), (0.0, 0.0));
    let back = run(&mut r, &frame(&[(Channel::JAW_OPEN, 0.4)]));
    assert!(back.weight > 0.0 && back.weight < 1.0, "fades back in");
    assert_eq!(
        back.values[Channel::JAW_OPEN],
        0.4,
        "and the filter restarted, so the first sample is not dragged to the old pose"
    );
}

#[test]
fn a_lost_face_is_handed_to_the_life_layer_at_half_weight() {
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    run(&mut r, &frame(&[(Channel::JAW_OPEN, 0.8)]));
    let mut owned = true;
    for _ in 0..60 {
        let out = run_for(&mut r, &lost(), 0.05);
        owned = out.claims();
    }
    assert!(!owned, "the layer stops owning its channels long before the end of the fade");
}

#[test]
fn the_lost_policy_is_configurable() {
    let config = RetargetConfig { lost: LostPolicy { grace_s: 0.0, fade_s: 0.0 }, ..RetargetConfig::default() };
    let mut r = ArkitRetarget::with(Calibration::identity(DeviceProfile::Arkit), config);
    run(&mut r, &frame(&[(Channel::JAW_OPEN, 0.8)]));
    let out = run(&mut r, &lost());
    assert_eq!((out.weight, out.values[Channel::JAW_OPEN]), (0.0, 0.0), "no grace and no fade: gone at once");
}

#[test]
fn a_lost_frame_before_any_tracking_claims_nothing() {
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    assert!(run(&mut r, &lost()).mask.is_empty());
}

#[test]
fn nothing_non_finite_ever_leaves_the_retarget() {
    for config in [RetargetConfig::default(), unsmoothed()] {
        let mut r = ArkitRetarget::with(Calibration::identity(DeviceProfile::Arkit), config);
        let mut f = frame(&[(Channel::JAW_OPEN, f32::NAN), (Channel::BROW_INNER_UP, f32::INFINITY)]);
        f.head =
            Some(HeadPose { yaw: f32::NAN, pitch: 0.1, roll: 0.0, position: Some(Vec3::new(0.0, f32::INFINITY, 0.5)) });
        f.eyes = Some(EyeAngles { left_yaw: f32::NAN, ..EyeAngles::default() });
        for _ in 0..3 {
            let out = run(&mut r, &f);
            assert!(out.values.as_slice().iter().all(|v| v.is_finite()), "{out:?}");
            assert_eq!(out.values[Channel::JAW_OPEN], 0.0, "NaN reads as rest");
            assert_eq!(out.values[Channel::BROW_INNER_UP], 1.0, "infinity reads as full");
        }
    }
}

#[test]
fn a_glitched_head_eases_to_rest_instead_of_jumping_there() {
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    let mut f = frame(&[]);
    f.head = Some(HeadPose { yaw: 0.4, ..HeadPose::default() });
    run(&mut r, &f);
    let settled = run(&mut r, &f).values[Channel::HEAD_YAW];
    f.head = Some(HeadPose { yaw: f32::NAN, ..HeadPose::default() });
    let glitch = run(&mut r, &f).values[Channel::HEAD_YAW];
    assert!(glitch > 0.0 && glitch < settled, "{glitch} between rest and {settled}");
}

#[test]
fn changing_the_calibration_or_the_smoothing_keeps_the_filters_so_the_face_does_not_jump() {
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    let open = frame(&[(Channel::BROW_INNER_UP, 1.0)]);
    run(&mut r, &frame(&[(Channel::BROW_INNER_UP, 0.0)]));
    let before = run(&mut r, &open).values[Channel::BROW_INNER_UP];
    r.set_calibration(Calibration::identity(DeviceProfile::Arkit));
    r.set_config(RetargetConfig { smoothing: 0.5, ..RetargetConfig::default() });
    let after = run(&mut r, &open).values[Channel::BROW_INNER_UP];
    assert!(after > before && after < 1.0, "carries on from {before}, not from a fresh filter: {after}");
    assert_eq!(r.config().smoothing, 0.5);
}

#[test]
fn turning_smoothing_off_clears_the_filters_so_turning_it_on_starts_clean() {
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    run(&mut r, &frame(&[(Channel::JAW_OPEN, 1.0)]));
    r.set_config(unsmoothed());
    run(&mut r, &frame(&[(Channel::JAW_OPEN, 0.5)]));
    r.set_config(RetargetConfig::default());
    assert_eq!(run(&mut r, &frame(&[(Channel::JAW_OPEN, 0.2)])).values[Channel::JAW_OPEN], 0.2);
}

#[test]
fn reset_starts_from_rest() {
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    run(&mut r, &frame(&[(Channel::JAW_OPEN, 1.0)]));
    run_for(&mut r, &lost(), 2.0);
    r.reset();
    let out = run(&mut r, &frame(&[(Channel::JAW_OPEN, 0.3)]));
    assert_eq!((out.values[Channel::JAW_OPEN], out.weight), (0.3, 1.0));
}

#[test]
fn a_lost_status_is_what_decides_not_the_stale_values() {
    let mut f = frame(&[(Channel::JAW_OPEN, 1.0)]);
    f.status = TrackStatus::Lost;
    let mut r = ArkitRetarget::new(DeviceProfile::Arkit);
    assert!(run(&mut r, &f).mask.is_empty(), "values in a lost frame are stale and ignored");
}

#[test]
fn the_retarget_is_a_send_trait_object() {
    fn assert_send<T: Send>() {}
    assert_send::<Box<dyn Retarget>>();
    let mut boxed: Box<dyn Retarget> = Box::new(ArkitRetarget::new(DeviceProfile::Webcam));
    let mut out = Layer::absent(BlendMode::Override);
    boxed.apply(&frame(&[(Channel::JAW_OPEN, 0.5)]), DT, &mut out);
    assert!(out.values[Channel::JAW_OPEN] > 0.0);
    assert_eq!(boxed.calibration().profile, DeviceProfile::Webcam);
}
