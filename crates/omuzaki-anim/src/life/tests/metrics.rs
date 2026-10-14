//! The prototype's liveliness metrics (spec §15, R4.3), measured over simulated minutes.
//!
//! Blink rate, double-blink share, gaze moves per second and the longest frozen window are taken from
//! the earlier web prototype's life test, and the ranges are that test's own gates. Where the prototype ran
//! 10 seeds for 10 simulated minutes, so does this.

use std::ops::RangeInclusive;

use omuzaki_anim_api::{Bus, Channel, Cues, LifeConfig, Personality, Tick};

use crate::life::Life;

use super::harness::{simulate, still};

/// Everything the metrics read, per frame.
#[derive(Copy, Clone, Debug)]
struct Sample {
    blink: [f32; 2],
    /// Left lid openness: wide minus squint.
    lid: f32,
    gaze: (f32, f32),
}

fn record(config: LifeConfig, fps: f32, seconds: f32, mut scene: impl FnMut(&Tick) -> (Bus, Cues)) -> Vec<Sample> {
    let mut life = Life::new(config);
    let mut samples = Vec::new();
    simulate(&mut life, fps, (seconds * fps) as u64, &mut scene, |_, _, bus| {
        let v = &bus.values;
        samples.push(Sample {
            blink: [v[Channel::EYE_BLINK_LEFT], v[Channel::EYE_BLINK_RIGHT]],
            lid: v[Channel::EYE_WIDE_LEFT] - v[Channel::EYE_SQUINT_LEFT],
            gaze: v.gaze(),
        });
    });
    samples
}

/// Times (seconds) at which a series rises through `threshold`.
fn rising_edges(series: impl Iterator<Item = f32>, fps: f32, threshold: f32) -> Vec<f32> {
    let mut up = false;
    let mut edges = Vec::new();
    for (i, v) in series.enumerate() {
        let high = v >= threshold;
        if high && !up {
            edges.push(i as f32 / fps);
        }
        up = high;
    }
    edges
}

/// Blink onsets: either lid past 0.3.
fn blink_times(samples: &[Sample], fps: f32) -> Vec<f32> {
    rising_edges(samples.iter().map(|s| s.blink[0].max(s.blink[1])), fps, 0.3)
}

/// Blinks per minute, and the fraction of blink events that are the second of a double (within 0.6 s of the last).
fn blink_stats(samples: &[Sample], fps: f32) -> (f32, f32) {
    let times = blink_times(samples, fps);
    let doubles = times.windows(2).filter(|w| w[1] - w[0] < 0.6).count();
    let minutes = samples.len() as f32 / fps / 60.0;
    (times.len() as f32 / minutes, doubles as f32 / (times.len() - doubles).max(1) as f32)
}

/// Gaze moves per second: onsets of frames in which the gaze moved at least 0.012 look units at 30 fps (scaled for
/// other rates, so the same speed counts the same).
fn saccades_per_second(samples: &[Sample], fps: f32) -> f32 {
    let threshold = 0.012 * 30.0 / fps;
    let steps = samples.windows(2).map(|w| (w[1].gaze.0 - w[0].gaze.0).hypot(w[1].gaze.1 - w[0].gaze.1));
    rising_edges(steps, fps, threshold).len() as f32 / (samples.len() as f32 / fps)
}

/// The longest stretch, seconds, in which lid, both blink channels and both gaze axes stayed within `eps` of each
/// other: the prototype's "no frozen 0.5 s window on the eyes".
fn longest_frozen_window(samples: &[Sample], fps: f32, eps: f32) -> f32 {
    let channels = |s: &Sample| [s.lid, s.blink[0], s.blink[1], s.gaze.0, s.gaze.1];
    let mut longest = 0;
    for start in 0..samples.len() {
        let first = channels(&samples[start]);
        let (mut lo, mut hi) = (first, first);
        let mut end = start + 1;
        while end < samples.len() {
            let now = channels(&samples[end]);
            for k in 0..5 {
                lo[k] = lo[k].min(now[k]);
                hi[k] = hi[k].max(now[k]);
            }
            if (0..5).any(|k| hi[k] - lo[k] > eps) {
                break;
            }
            end += 1;
        }
        longest = longest.max(end - start);
    }
    longest as f32 / fps
}

// The prototype's gates (life-test.mjs): blinks a minute (over all seeds, and for each), the share of blinks that are
// doubles, gaze moves a second (the prototype's 12 to 80 in 20 s) and the longest window the eyes may stand still.
const BLINKS_PER_MIN: RangeInclusive<f32> = 15.0..=20.0;
const BLINKS_PER_MIN_EACH_SEED: RangeInclusive<f32> = 13.0..=22.0;
const DOUBLE_SHARE: RangeInclusive<f32> = 0.08..=0.18;
const SACCADES_PER_S: RangeInclusive<f32> = 0.6..=4.0;
const FROZEN_LIMIT_S: f32 = 0.5;
const TEN_MINUTES: f32 = 600.0;

#[test]
fn blink_rate_matches_the_prototype_over_ten_minutes() {
    let (mut rates, mut doubles) = (Vec::new(), Vec::new());
    for seed in 1..=10 {
        let samples = record(LifeConfig::new(seed * 11), 30.0, TEN_MINUTES, still);
        let (rate, share) = blink_stats(&samples, 30.0);
        rates.push(rate);
        doubles.push(share);
    }
    let mean = rates.iter().sum::<f32>() / 10.0;
    let share = doubles.iter().sum::<f32>() / 10.0;
    println!("blinks per minute per seed {rates:.1?}, mean {mean:.1}; double share {share:.3}");
    assert!(BLINKS_PER_MIN.contains(&mean), "mean {mean}");
    assert!(rates.iter().all(|r| BLINKS_PER_MIN_EACH_SEED.contains(r)), "{rates:?}");
    assert!(DOUBLE_SHARE.contains(&share), "double share {share}");
}

#[test]
fn saccade_rate_matches_the_prototype_over_ten_minutes() {
    let rates: Vec<f32> = (1..=5)
        .map(|seed| saccades_per_second(&record(LifeConfig::new(seed), 30.0, TEN_MINUTES, still), 30.0))
        .collect();
    println!("gaze moves per second per seed {rates:.2?}");
    assert!(rates.iter().all(|r| SACCADES_PER_S.contains(r)), "{rates:?}");
}

#[test]
fn the_eyes_are_never_frozen_for_half_a_second() {
    for (fps, seed) in [(30.0, 1), (30.0, 2), (30.0, 3), (60.0, 4), (60.0, 5), (24.0, 6)] {
        let samples = record(LifeConfig::new(seed), fps, TEN_MINUTES, still);
        let longest = longest_frozen_window(&samples, fps, 0.003);
        println!("fps {fps} seed {seed}: longest frozen window {longest:.2} s");
        assert!(longest < FROZEN_LIMIT_S, "fps {fps} seed {seed}: {longest} s");
    }
}

#[test]
fn every_personality_runs_stably_and_the_energetic_one_blinks_more_than_the_calm_one() {
    let rate = |p: Personality| {
        let samples = record(LifeConfig::new(3).with_personality(p), 30.0, TEN_MINUTES, still);
        assert!(samples.iter().all(|s| s.lid.is_finite() && s.gaze.0.is_finite()));
        blink_stats(&samples, 30.0).0
    };
    let rates: Vec<f32> =
        [Personality::Natural, Personality::Energetic, Personality::Calm, Personality::Shy, Personality::Sleepy]
            .into_iter()
            .map(rate)
            .collect();
    println!(
        "blinks per minute: natural {:.1}, energetic {:.1}, calm {:.1}, shy {:.1}, sleepy {:.1}",
        rates[0], rates[1], rates[2], rates[3], rates[4]
    );
    assert!(rates[1] > rates[2], "energetic {} vs calm {}", rates[1], rates[2]);
}
