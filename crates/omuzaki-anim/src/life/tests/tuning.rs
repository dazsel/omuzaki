//! The constants that moved into `LifeParams` (#78): the defaults keep the old numbers, older files still load, and
//! each group changes the behaviour it names.

use omuzaki_anim_api::{Bus, Channel, Cues, LaughParams, LifeConfig, LifeParams, Span, Tick};

use super::harness::{bus_of, laughing, peak, series, still};

fn config_with(edit: impl FnOnce(&mut LifeParams)) -> LifeConfig {
    let mut config = LifeConfig::new(2);
    edit(&mut config.params);
    config
}

#[test]
fn the_defaults_hold_the_prototypes_numbers() {
    let p = LifeParams::default();
    assert_eq!(p.laugh, LaughParams { start_smile: 0.6, start_jaw: 0.3, start_loud: 0.45, keep_smile: 0.45, keep_jaw: 0.2, keep_loud: 0.3, hold_s: 0.12, ramp_s: 0.15, arc_loud: 0.85, rate_wander: 0.1 });
    assert_eq!((p.gaze.reach, p.gaze.first_glance, p.gaze.steer_spread_rad), (0.8, 0.4, 1.2));
    assert_eq!((p.gaze.drift_hz, p.gaze.drift_sine), ([0.83, 1.07], [0.6, 0.5]));
    assert_eq!((p.gaze.lead_speed_tau_s, p.gaze.lead_calm_s, p.gaze.lead_max_s), (0.05, 0.25, 1.5));
    assert_eq!((p.head.nod_decay_s, p.head.talk_tau_s, p.head.breath_wander), (0.12, 0.4, 0.1));
    assert_eq!((p.face.coupled_lid_cap, p.face.lid_drift_sine, p.face.lid_drift_hz), (0.6, 0.4, 1.21));
    assert_eq!((p.face.surprise_jaw, p.face.surprise_brows), (Span::new(0.6, 0.9), Span::new(0.3, 0.6)));
    assert_eq!((p.blink.settle_s, p.blink.retry_s, p.blink.extra_cooldown_s), (0.3, Span::new(0.3, 0.6), 0.3));
    assert_eq!(p.springs.side_skew, 0.08);
}

#[test]
fn a_params_file_from_before_the_move_still_loads_with_the_new_fields_at_their_defaults() {
    let old = serde_json::json!({
        "blink": { "gap_floor_s": 1.0, "gap_scale_s": 1.6, "gap_cap_s": 6.0, "double_chance": 0.12,
            "double_delay_s": { "lo": 0.28, "hi": 0.4 }, "close_s": { "lo": 0.06, "hi": 0.08 },
            "open_s": { "lo": 0.12, "hi": 0.18 }, "depth": { "lo": 0.93, "hi": 1.0 },
            "eye_lag_s": { "lo": 0.005, "hi": 0.015 }, "on_glance_chance": 0.3, "on_head_turn_chance": 0.4 }
    });
    let p: LifeParams = serde_json::from_value(old).expect("an old file loads");
    assert_eq!(p.blink.gap_floor_s, 1.0);
    assert_eq!(p.blink.settle_s, 0.3);
    assert_eq!(p.laugh, LaughParams::default());
}

#[test]
fn laugh_thresholds_decide_what_counts_as_a_laugh() {
    let squint = |config: LifeConfig| {
        peak(&series(config, 30.0, 3.0, laughing(0.5, 0.35), |b: &Bus| b.values[Channel::EYE_SQUINT_LEFT]))
    };
    // A smile of 1, jaw 0.35 and loudness 0.5 starts a laugh at the defaults, and not once the bar is raised.
    let laughs = squint(LifeConfig::new(2));
    let raised = squint(config_with(|p| p.laugh.start_jaw = 0.5));
    assert!(laughs > raised + 0.1, "{laughs} vs {raised}");
}

#[test]
fn the_arc_loudness_is_a_parameter() {
    let arcs = |bar: f32| {
        let config = config_with(|p| p.laugh.arc_loud = bar);
        peak(&series(config, 30.0, 4.0, laughing(0.8, 0.6), |b: &Bus| b.values[Channel::EYE_ARC_LEFT]))
    };
    assert_eq!(arcs(0.85), 0.0);
    assert!(arcs(0.7) > 0.9);
}

#[test]
fn a_zero_time_constant_is_finite_not_a_division_by_zero() {
    let config = config_with(|p| {
        p.laugh.ramp_s = 0.0;
        p.head.nod_decay_s = 0.0;
        p.head.talk_tau_s = f32::NAN;
        p.gaze.lead_speed_tau_s = 0.0;
    });
    let scene = |tick: &Tick| {
        let t = tick.time.0 as f32;
        (bus_of(|v| v[Channel::HEAD_YAW] = if t > 1.0 { 0.4 } else { 0.0 }), Cues { loudness: 0.9, onset: true, ..Cues::default() })
    };
    let yaw = series(config, 60.0, 4.0, scene, |b: &Bus| b.values[Channel::HEAD_YAW]);
    assert!(yaw.iter().all(|y| y.is_finite()));
}

#[test]
fn the_gaze_reach_and_the_side_skew_change_the_output() {
    let look = |reach: f32| {
        let config = config_with(|p| p.gaze.reach = reach);
        peak(&series(config, 30.0, 30.0, still, |b: &Bus| b.values[Channel::EYE_LOOK_OUT_LEFT]))
    };
    assert!(look(0.0) < look(0.8), "a reach of zero stills the wander");
    // A smile that starts at 1 s: the leading side's spring arrives sooner, so the skew moves the rising edge.
    let rise = |skew: f32| {
        let config = config_with(|p| p.springs.side_skew = skew);
        let scene = |tick: &Tick| {
            let on = tick.time.0 >= 1.0;
            (bus_of(|v| v[Channel::MOUTH_SMILE_LEFT] = if on { 1.0 } else { 0.0 }), Cues::default())
        };
        series(config, 60.0, 2.0, scene, |b: &Bus| b.values[Channel::MOUTH_SMILE_LEFT])
    };
    let (flat, skewed) = (rise(0.0), rise(0.3));
    let gap = flat.iter().zip(&skewed).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
    assert!(gap > 0.01, "the skew should move the rising edge, gap {gap}");
}
