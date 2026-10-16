//! Every generator gives the same samples for the same seed, and different samples for another.

use omuzaki_audio_mock::signals::{
    Sweep, VoiceSpec, changer, clap, click_train, impulse, pink_noise, plosive, sine, sine_sweep, voice, white_noise,
};

fn bits(signal: &[f32]) -> Vec<u32> {
    signal.iter().map(|s| s.to_bits()).collect()
}

#[test]
fn every_seeded_signal_is_bit_identical_for_a_seed_and_differs_for_another() {
    type Make = Box<dyn Fn(u64) -> Vec<f32>>;
    let makers: Vec<(&str, Make)> = vec![
        ("voice", Box::new(|seed| voice(&VoiceSpec::steady(seed, 0.3)))),
        ("clap", Box::new(|seed| clap(4800, seed, 80.0, 0.9))),
        ("plosive", Box::new(|seed| plosive(4800, seed, 0.9))),
        ("white", Box::new(|seed| white_noise(4800, seed, 0.9))),
        ("pink", Box::new(|seed| pink_noise(4800, seed, 0.9))),
        ("changer", Box::new(|seed| changer(seed).apply(&clap(4800, 1, 80.0, 0.9)))),
    ];
    for (name, make) in &makers {
        assert_eq!(bits(&make(41)), bits(&make(41)), "{name} repeats for a seed");
        assert_ne!(bits(&make(41)), bits(&make(42)), "{name} differs for another seed");
        assert!(make(41).iter().all(|s| s.is_finite()), "{name} is finite");
    }
}

#[test]
fn the_unseeded_signals_are_fixed() {
    assert_eq!(bits(&sine(1000, 440.0, 0.5)), bits(&sine(1000, 440.0, 0.5)));
    assert_eq!(
        bits(&sine_sweep(1000, 20.0, 20_000.0, Sweep::Logarithmic, 0.5)),
        bits(&sine_sweep(1000, 20.0, 20_000.0, Sweep::Logarithmic, 0.5))
    );
    assert_eq!(impulse(8, 3).iter().sum::<f32>(), 1.0);
    assert_eq!(click_train(100, 10, 30, 1.0).iter().filter(|s| **s != 0.0).count(), 3);
}
