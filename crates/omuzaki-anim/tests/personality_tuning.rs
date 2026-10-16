//! The personality presets measured against the liveliness metrics (VF-05) on a still face, eight seeds each.

#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use omuzaki_anim::life::Life;
use omuzaki_anim::measure::{Baseline, liveliness};
use omuzaki_anim_api::{Bus, ChannelBuf, Cues, LifeConfig, LifeLayer, Personality, Tick};

const FPS: f32 = 60.0;
const SEEDS: std::ops::RangeInclusive<u64> = 1..=8;
const SECONDS: f32 = 120.0;

fn run(seed: u64, personality: Personality) -> Vec<ChannelBuf> {
    let mut life = Life::new(LifeConfig::new(seed).with_personality(personality));
    (0..(SECONDS * FPS) as u64)
        .map(|i| {
            let mut bus = Bus::default();
            life.step(Tick::at_rate(i, FPS), &Cues::default(), &mut bus);
            bus.values
        })
        .collect()
}

/// What one preset measured, averaged over the seeds.
struct Row {
    blinks_per_minute: f32,
    saccades_per_second: f32,
    change_per_second: f32,
}

fn measure(p: Personality) -> Row {
    let rest = Baseline::builtin("rest").unwrap();
    let ms: Vec<_> = SEEDS.map(|s| liveliness(&run(s, p), 1.0 / FPS)).collect();
    for (seed, m) in SEEDS.zip(&ms) {
        // The prototype's R4.3: no frozen 0.5 s window; and every run lands in the at-rest band.
        assert!(rest.compare(m).all_ok(), "{p:?} seed {seed}: {m:?} -> {:?}", rest.compare(m));
    }
    let n = ms.len() as f32;
    let mean = |f: fn(&omuzaki_anim::measure::Liveliness) -> f32| ms.iter().map(f).sum::<f32>() / n;
    let row = Row {
        blinks_per_minute: mean(|m| m.blinks_per_minute),
        saccades_per_second: mean(|m| m.saccades_per_second),
        change_per_second: mean(|m| m.change_per_second),
    };
    println!(
        "{p:?}: blinks/min {:.1}, saccades/s {:.2}, change/s {:.4}, longest frozen {:.2} s",
        row.blinks_per_minute,
        row.saccades_per_second,
        row.change_per_second,
        ms.iter().map(|m| m.longest_frozen_s).fold(0.0, f32::max)
    );
    row
}

#[test]
fn every_preset_lives_inside_the_liveliness_bands_and_they_differ_in_the_expected_order() {
    let [natural, energetic, calm, shy, sleepy] =
        [Personality::Natural, Personality::Energetic, Personality::Calm, Personality::Shy, Personality::Sleepy]
            .map(measure);
    for (name, row) in
        [("natural", &natural), ("energetic", &energetic), ("calm", &calm), ("shy", &shy), ("sleepy", &sleepy)]
    {
        assert!((15.0..=20.0).contains(&row.blinks_per_minute), "{name}: {} blinks a minute", row.blinks_per_minute);
    }
    assert!(
        energetic.blinks_per_minute > natural.blinks_per_minute && natural.blinks_per_minute > calm.blinks_per_minute
    );
    assert!(calm.blinks_per_minute > sleepy.blinks_per_minute);
    assert!(
        energetic.saccades_per_second > shy.saccades_per_second
            && shy.saccades_per_second > natural.saccades_per_second
    );
    assert!(natural.saccades_per_second > sleepy.saccades_per_second);
    assert!(energetic.change_per_second > shy.change_per_second && shy.change_per_second > natural.change_per_second);
    assert!(natural.change_per_second > calm.change_per_second && calm.change_per_second > sleepy.change_per_second);
}
