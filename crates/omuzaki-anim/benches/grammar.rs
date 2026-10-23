//! Benchmark for the grammar stage's per-frame cost (spec §10: the whole CPU side is under 3% of an 8-core at 1080p30).
//! Run with `cargo bench -p omuzaki-anim --bench grammar`.

// A stage over the registered rules cannot fail to build; a panic in a benchmark is a clear failure.
#![allow(clippy::expect_used)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim::grammar::{GrammarConfig, GrammarStage};
use omuzaki_anim_api::{Bus, Channel, ChannelMask, Cues, Source, Tick};
use omuzaki_face_api::{GrammarRules, RuleConfig, RuleId};

/// A face with both mouth corners at `smile` and `frown`, the jaw at `jaw` and the lips puckered by `pucker`.
/// Speech loud enough for a laugh, so the laugh's whole path is measured.
const LOUD: Cues = Cues { loudness: 0.8, onset: false, laugh: 0.0, eyes_lead_turn: false, look_shift: false };

fn mouth(smile: f32, frown: f32, jaw: f32, pucker: f32) -> Bus {
    let mut bus = Bus::default();
    for channel in [Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_SMILE_RIGHT] {
        bus.values[channel] = smile;
    }
    for channel in [Channel::MOUTH_FROWN_LEFT, Channel::MOUTH_FROWN_RIGHT] {
        bus.values[channel] = frown;
    }
    bus.values[Channel::JAW_OPEN] = jaw;
    bus.values[Channel::MOUTH_PUCKER] = pucker;
    bus
}

/// A smiling face with both eyes mid-blink, so the rule takes its writing path.
fn blinking_smile() -> Bus {
    let mut bus = Bus::default();
    for channel in [Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_SMILE_RIGHT] {
        bus.values[channel] = 0.6;
    }
    for channel in [Channel::EYE_BLINK_LEFT, Channel::EYE_BLINK_RIGHT] {
        bus.values[channel] = 0.9;
    }
    bus
}

fn stage(config: &GrammarConfig) -> GrammarStage {
    GrammarStage::new(config).expect("the registered rules have valid, distinct ids")
}

fn grammar_frame(c: &mut Criterion) {
    let tick = Tick::at_rate(0, 60.0);

    // What every case below pays just to copy the bus in and out; subtract it to get the stage's own cost.
    c.bench_function("grammar: baseline, copy the bus", |b| {
        b.iter(|| black_box(black_box(blinking_smile())));
    });

    let mut on = stage(&GrammarConfig::default());
    c.bench_function("grammar: one frame, smile-blink-arc writes the arcs", |b| {
        b.iter(|| {
            let mut bus = black_box(blinking_smile());
            on.step(tick, &LOUD, &mut bus);
            black_box(bus);
        });
    });

    // Each look at full effect, on the stage with every registered rule: the cost of the rules that have something to
    // say, on top of the ones that have not.
    let looks = [
        ("laugh-squint squints, lifts the cheeks and closes into ^^", mouth(1.0, 0.0, 0.6, 0.0)),
        ("pout-cheek-puff puffs the cheeks", mouth(0.0, 0.7, 0.0, 0.7)),
        ("grin-fangs shows the fangs", mouth(0.8, 0.0, 0.2, 0.0)),
    ];
    for (name, face) in looks {
        let mut all = stage(&GrammarConfig::default());
        c.bench_function(&format!("grammar: one frame, {name}"), |b| {
            b.iter(|| {
                let mut bus = black_box(face);
                all.step(tick, &LOUD, &mut bus);
                black_box(bus);
            });
        });
    }

    let mut resting = stage(&GrammarConfig::default());
    c.bench_function("grammar: one frame, nothing to do (no smile)", |b| {
        b.iter(|| {
            let mut bus = black_box(Bus::default());
            resting.step(tick, &LOUD, &mut bus);
            black_box(bus);
        });
    });

    let off = RuleConfig { enabled: false, ..RuleConfig::default() };
    let rules = GrammarRules([(RuleId::new("smile-blink-arc").expect("a slug"), off)].into());
    let mut disabled = stage(&GrammarConfig::from_rules(rules));
    c.bench_function("grammar: one frame, the rule is switched off", |b| {
        b.iter(|| {
            let mut bus = black_box(blinking_smile());
            disabled.step(tick, &LOUD, &mut bus);
            black_box(bus);
        });
    });

    let mut person = stage(&GrammarConfig::default());
    c.bench_function("grammar: one frame, a person drives the eyes", |b| {
        b.iter(|| {
            let mut bus = black_box(blinking_smile());
            bus.owners.claim(Source::Tracker, ChannelMask::EYES);
            person.step(tick, &LOUD, &mut bus);
            black_box(bus);
        });
    });
}

criterion_group!(benches, grammar_frame);
criterion_main!(benches);
