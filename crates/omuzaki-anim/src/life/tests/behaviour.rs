//! What each behaviour does to the face: couplings, laugh, speech, eyes leading the head, breathing, texture.

use omuzaki_anim_api::{Bus, Channel, Cues, LifeConfig, LifeLayer, Tick};

use crate::life::Life;

use super::harness::{bus_of, laughing, minus, peak, series, simulate, still, trough};

/// A smile held on both corners.
fn smiling(amount: f32) -> impl FnMut(&Tick) -> (Bus, Cues) {
    move |_| {
        let bus = bus_of(|v| {
            v[Channel::MOUTH_SMILE_LEFT] = amount;
            v[Channel::MOUTH_SMILE_RIGHT] = amount;
        });
        (bus, Cues::default())
    }
}

#[test]
fn a_smile_pushes_the_cheeks_up_and_flushes_them() {
    let cheek = |c: LifeConfig| series(c, 30.0, 4.0, smiling(1.0), |b| b.values[Channel::CHEEK_SQUINT_LEFT]);
    let blush = |c: LifeConfig| series(c, 30.0, 4.0, smiling(1.0), |b| b.values[Channel::BLUSH]);
    let cheek_on = cheek(LifeConfig::new(1));
    let end = cheek_on[cheek_on.len() - 1];
    assert!((end - 0.35).abs() < 0.01, "smile 1.0 raises the cheek by smile_to_lids: {end}");
    let flush = *blush(LifeConfig::new(1)).last().unwrap_or(&0.0);
    assert!((flush - 0.25).abs() < 0.01, "and flushes by smile_to_blush: {flush}");

    let mut off = LifeConfig::new(1);
    off.systems.coupling = false;
    assert_eq!(peak(&cheek(off)), 0.0);
    assert_eq!(peak(&blush(off)), 0.0);
}

#[test]
fn a_big_open_mouth_under_raised_brows_widens_the_eyes() {
    let surprised = |_: &Tick| {
        (
            bus_of(|v| {
                v[Channel::JAW_OPEN] = 0.9;
                v[Channel::BROW_INNER_UP] = 0.8;
            }),
            Cues::default(),
        )
    };
    let wide = series(LifeConfig::new(1), 30.0, 3.0, surprised, |b| b.values[Channel::EYE_WIDE_LEFT]);
    let end = wide[wide.len() - 1];
    assert!((0.3..=0.4).contains(&end), "widened by about surprise_widen: {end}");
    let calm = series(LifeConfig::new(1), 30.0, 3.0, still, |b| b.values[Channel::EYE_WIDE_LEFT]);
    assert!(peak(&calm) < 0.1, "a relaxed face does not widen");
}

#[test]
fn a_laugh_squints_the_eyes_and_bobs_the_head_back_at_about_four_hertz() {
    let read = |jaw: f32, f: fn(&Bus) -> f32| series(LifeConfig::new(2), 30.0, 5.0, laughing(0.8, jaw), f);
    let squint = |b: &Bus| b.values[Channel::EYE_SQUINT_LEFT];
    let pitch = |b: &Bus| b.values[Channel::HEAD_PITCH];
    let (laugh, calm) = (read(0.6, squint), read(0.0, squint));
    let extra = laugh[120] - calm[120];
    assert!((extra - 0.2).abs() < 0.03, "a laugh adds laugh_squint to the squint: {extra}");

    let response = minus(&read(0.6, pitch), &read(0.0, pitch));
    let bob = &response[60..];
    println!("laugh bob: pitch {:.1} to {:.1} degrees", trough(bob).to_degrees(), peak(bob).to_degrees());
    assert!(peak(bob) < -0.02 && trough(bob) > -0.09, "back and up by 1 to 5 degrees: {} {}", peak(bob), trough(bob));
    let mean = bob.iter().sum::<f32>() / bob.len() as f32;
    let crossings = bob.windows(2).filter(|w| (w[0] - mean) * (w[1] - mean) < 0.0).count();
    let hz = crossings as f32 / 2.0 / (bob.len() as f32 / 30.0);
    assert!((3.0..=5.0).contains(&hz), "about 4 Hz: {hz}");
}

#[test]
fn a_laugh_ramps_in_smoothly() {
    let mut scene = |tick: &Tick| laughing(if tick.time.0 < 1.0 { 0.0 } else { 0.8 }, 0.6)(tick);
    let pitch = series(LifeConfig::new(2), 30.0, 3.0, &mut scene, |b| b.values[Channel::HEAD_PITCH]);
    let max_step = pitch.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0_f32, f32::max);
    assert!(max_step < 0.026, "no pitch jump above 1.5 degrees per frame: {}", max_step.to_degrees());
}

#[test]
fn only_a_really_loud_laugh_closes_the_eyes_into_arcs_and_a_score_can_forbid_it() {
    let arcs = |loudness: f32, allowed: bool| {
        let mut config = LifeConfig::new(2);
        config.laugh_arcs = allowed;
        peak(&series(config, 30.0, 4.0, laughing(loudness, 0.6), |b| b.values[Channel::EYE_ARC_LEFT]))
    };
    assert_eq!(arcs(0.8, true), 0.0, "a normal laugh squints but does not close");
    assert!(arcs(0.95, true) > 0.9);
    assert_eq!(arcs(0.95, false), 0.0);
}

#[test]
fn laughter_heard_in_the_audio_strengthens_the_case() {
    let scene = |_: &Tick| {
        let bus = bus_of(|v| {
            for c in [Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_SMILE_RIGHT] {
                v[c] = 1.0;
            }
            v[Channel::JAW_OPEN] = 0.6;
        });
        (bus, Cues { loudness: 0.1, onset: false, laugh: 0.9 })
    };
    let squint = series(LifeConfig::new(2), 30.0, 3.0, scene, |b| b.values[Channel::EYE_SQUINT_LEFT]);
    assert!(squint[squint.len() - 1] > 0.15, "a quiet but detected laugh still squints");
}

/// Someone talking at `loudness` with a stressed syllable at each time in `onsets` (30 fps frames).
fn speaking(loudness: f32, onsets: &[f64]) -> impl FnMut(&Tick) -> (Bus, Cues) + '_ {
    move |tick| {
        let onset = onsets.iter().any(|t| (tick.time.0 - t).abs() < 0.5 / 30.0);
        (bus_of(|v| v[Channel::JAW_OPEN] = 0.3), Cues { loudness, onset, laugh: 0.0 })
    }
}

#[test]
fn a_stressed_syllable_kicks_the_brows_and_nods_the_head() {
    let brows = |onsets: &[f64]| {
        series(LifeConfig::new(3), 30.0, 3.0, speaking(0.5, onsets), |b| b.values[Channel::BROW_INNER_UP])
    };
    let kicked = brows(&[1.0]);
    let rise = peak(&kicked) - kicked[10];
    println!("brow rise on one onset at loudness 0.5: +{rise:.3}");
    assert!((0.18..=0.42).contains(&rise), "the prototype's 0.2 to 0.4: {rise}");
    assert_eq!(peak(&brows(&[])), 0.0, "no onset, no kick");

    let pitch = |onsets: &[f64]| {
        series(LifeConfig::new(3), 30.0, 3.0, speaking(0.5, onsets), |b| b.values[Channel::HEAD_PITCH])
    };
    let nod = minus(&pitch(&[1.0]), &pitch(&[]));
    assert!(
        peak(&nod) > 0.01 && trough(&nod) > -1e-4,
        "a nod is down (positive pitch): {} {}",
        peak(&nod),
        trough(&nod)
    );
}

#[test]
fn nods_tilt_to_alternate_sides() {
    let roll =
        |onsets: &[f64]| series(LifeConfig::new(3), 30.0, 5.0, speaking(0.6, onsets), |b| b.values[Channel::HEAD_ROLL]);
    let delta = minus(&roll(&[1.0, 3.0]), &roll(&[]));
    let (first, second) = (&delta[30..75], &delta[90..135]);
    assert!(peak(first).max(-trough(first)) > 0.003, "the nod tilts the head");
    let sign = |w: &[f32]| if peak(w) > -trough(w) { 1.0 } else { -1.0 };
    assert_ne!(sign(first), sign(second), "the second nod tilts the other way");
}

#[test]
fn the_eyes_lead_a_fast_head_turn() {
    const STEP: f32 = 0.349; // 20 degrees
    let scene = |turn: bool| {
        move |tick: &Tick| {
            let yaw = if turn && tick.time.0 >= 9.0 { STEP } else { 0.0 };
            (bus_of(|v| v[Channel::HEAD_YAW] = yaw), Cues::default())
        }
    };
    let (mut leads, mut overshoots) = (Vec::new(), Vec::new());
    for seed in 1..=8 {
        let run = |turn: bool| {
            let mut samples = Vec::new();
            simulate(&mut Life::new(LifeConfig::new(seed)), 30.0, 450, &mut scene(turn), |_, _, bus| {
                samples.push((bus.values[Channel::HEAD_YAW], bus.values.gaze()));
            });
            samples
        };
        let (with, without) = (run(true), run(false));
        let (mut gaze_at, mut head_at, mut top) = (None, None, f32::MIN);
        for i in 270..360 {
            let (dx, dy) = (with[i].1.0 - without[i].1.0, with[i].1.1 - without[i].1.1);
            let yaw = with[i].0 - without[i].0;
            gaze_at = gaze_at.or((dx.hypot(dy) >= 0.1).then_some(i));
            head_at = head_at.or((yaw >= STEP / 2.0).then_some(i));
            top = top.max(yaw);
        }
        let (gaze_at, head_at) = (gaze_at.expect("the gaze moves"), head_at.expect("the head arrives"));
        leads.push(head_at - gaze_at);
        overshoots.push((top - STEP) / STEP * 100.0);
        let settled = with[360].0 - without[360].0;
        assert!((settled - STEP).abs() < 0.005, "seed {seed}: the head settles on the target ({settled})");
    }
    println!("eyes lead the head by {leads:?} frames; head overshoot {overshoots:.1?} %");
    assert!(leads.iter().all(|&l| l >= 3), "the eyes arrive at least 3 frames (100 ms) first: {leads:?}");
    assert!(
        overshoots.iter().all(|o| (6.0..=20.0).contains(o)),
        "slight overshoot (zeta 0.55 is about 12%): {overshoots:?}"
    );
}

#[test]
fn the_eyes_lead_again_after_a_frame_with_a_subnormal_dt() {
    // The head snaps 2.5 rad in one frame whose dt is subnormal. Dividing that snap by such a dt overflows the raw
    // head speed to infinity, and inf * 0 left the estimate NaN for good, so the eyes never led again.
    const STEP: f32 = 0.349; // 20 degrees
    const SNAP_AT: u64 = 100;
    let run = |turn: bool| {
        let mut life = Life::new(LifeConfig::new(1));
        let mut samples = Vec::new();
        for i in 0..450 {
            let mut tick = Tick::at_rate(i, 30.0);
            if i == SNAP_AT {
                tick.dt = f32::MIN_POSITIVE / 2.0;
            }
            let snap = if i < SNAP_AT { -1.25 } else { 1.25 };
            let turned = if turn && tick.time.0 >= 9.0 { STEP } else { 0.0 };
            let mut bus = bus_of(|v| v[Channel::HEAD_YAW] = snap + turned);
            life.step(tick, &Cues::default(), &mut bus);
            samples.push((bus.values[Channel::HEAD_YAW], bus.values.gaze()));
        }
        samples
    };
    let (with, without) = (run(true), run(false));
    let (mut gaze_at, mut head_at) = (None, None);
    for i in 270..360 {
        let (dx, dy) = (with[i].1.0 - without[i].1.0, with[i].1.1 - without[i].1.1);
        gaze_at = gaze_at.or((dx.hypot(dy) >= 0.1).then_some(i));
        head_at = head_at.or((with[i].0 - without[i].0 >= STEP / 2.0).then_some(i));
    }
    let (gaze_at, head_at) = (gaze_at.expect("the eyes lead again"), head_at.expect("the head arrives"));
    assert!(head_at >= gaze_at + 3, "the eyes arrive at least 3 frames first: gaze {gaze_at}, head {head_at}");
}

#[test]
fn the_eyes_lead_in_the_direction_the_head_is_turning() {
    // Positive yaw turns toward the character's left and positive pitch nods down; gaze x is positive toward the left
    // and y positive up (the contract's conventions), so a nod down sends the eyes down.
    let toward = |channel: Channel, step: f32| {
        let run = |turn: bool| {
            let mut scene = move |tick: &Tick| {
                (bus_of(|v| v[channel] = if turn && tick.time.0 >= 3.0 { step } else { 0.0 }), Cues::default())
            };
            let mut gaze = Vec::new();
            simulate(&mut Life::new(LifeConfig::new(1)), 30.0, 120, &mut scene, |_, _, bus| {
                gaze.push(bus.values.gaze());
            });
            gaze
        };
        let (with, without) = (run(true), run(false));
        let at = 92; // three frames after the turn starts
        (with[at].0 - without[at].0, with[at].1 - without[at].1)
    };
    let (x, _) = toward(Channel::HEAD_YAW, 0.35);
    assert!(x > 0.1, "yaw toward the left looks left: {x}");
    let (x, _) = toward(Channel::HEAD_YAW, -0.35);
    assert!(x < -0.1, "yaw toward the right looks right: {x}");
    let (_, y) = toward(Channel::HEAD_PITCH, 0.35);
    assert!(y < -0.1, "a nod down looks down: {y}");
    let (_, y) = toward(Channel::HEAD_PITCH, -0.35);
    assert!(y > 0.1, "a nod up looks up: {y}");
}

#[test]
fn breathing_is_a_quarter_hertz_wave_in_zero_to_one_that_keeps_a_held_breath() {
    let breath = series(LifeConfig::new(1), 30.0, 20.0, still, |b| b.values[Channel::BREATH]);
    let peaks = breath.windows(3).filter(|w| w[1] > w[0] && w[1] >= w[2]).count();
    assert!((4..=6).contains(&peaks), "{peaks} breaths in 20 s");
    assert!(trough(&breath) >= 0.0 && peak(&breath) <= 1.0 && peak(&breath) - trough(&breath) > 0.8);

    let held = |_: &Tick| (bus_of(|v| v[Channel::BREATH] = 0.9), Cues::default());
    assert!(trough(&series(LifeConfig::new(1), 30.0, 20.0, held, |b| b.values[Channel::BREATH])) >= 0.9);

    let mut off = LifeConfig::new(1);
    off.systems.breathing = false;
    assert_eq!(peak(&series(off, 30.0, 20.0, still, |b| b.values[Channel::BREATH])), 0.0);
}

#[test]
fn head_brow_and_mouth_noise_are_small_alive_and_different_on_each_side() {
    let run = |read: fn(&Bus) -> f32| series(LifeConfig::new(1), 30.0, 60.0, still, read);
    let yaw = run(|b| b.values[Channel::HEAD_YAW]);
    assert!(peak(&yaw).max(-trough(&yaw)) <= 0.0279 + 1e-4, "head noise stays within +-1.6 degrees");
    assert!(peak(&yaw).max(-trough(&yaw)) > 0.0017, "and is alive (more than 0.1 degrees)");
    let net = |up: Channel, down: Channel| move |b: &Bus| b.values[up] - b.values[down];
    let (left, right) = (
        series(LifeConfig::new(1), 30.0, 60.0, still, net(Channel::BROW_OUTER_UP_LEFT, Channel::BROW_DOWN_LEFT)),
        series(LifeConfig::new(1), 30.0, 60.0, still, net(Channel::BROW_OUTER_UP_RIGHT, Channel::BROW_DOWN_RIGHT)),
    );
    assert!(peak(&left).max(-trough(&left)) <= 0.06 * 1.1 + 1e-3, "brow wobble stays within +-0.06");
    assert!(peak(&minus(&left, &right)).max(-trough(&minus(&left, &right))) > 0.01, "the sides differ");
    let corner =
        series(LifeConfig::new(1), 30.0, 60.0, still, net(Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_FROWN_LEFT));
    assert!(peak(&corner).max(-trough(&corner)) <= 0.05 * 1.1 + 1e-3 && peak(&corner).max(-trough(&corner)) > 0.01);
}

#[test]
fn the_leading_side_arrives_first() {
    let seed = (1..100).find(|&s| Life::new(LifeConfig::new(s)).traits().left_leads).unwrap_or(1);
    let scene = |tick: &Tick| {
        let open = if tick.time.0 >= 1.0 { 0.6 } else { 0.0 };
        (
            bus_of(|v| {
                v[Channel::MOUTH_STRETCH_LEFT] = open;
                v[Channel::MOUTH_STRETCH_RIGHT] = open;
            }),
            Cues::default(),
        )
    };
    let side = |c: Channel| series(LifeConfig::new(seed), 120.0, 2.0, scene, move |b| b.values[c]);
    let (left, right) = (side(Channel::MOUTH_STRETCH_LEFT), side(Channel::MOUTH_STRETCH_RIGHT));
    let reach = |v: &[f32]| v.iter().position(|&x| x >= 0.3).expect("it gets there");
    assert!(reach(&left) < reach(&right), "left leads: {} vs {}", reach(&left), reach(&right));
}
