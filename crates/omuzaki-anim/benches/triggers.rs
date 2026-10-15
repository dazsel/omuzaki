//! Benchmark for one trigger step, the per-frame cost of expression hotkeys (spec §10). Run with
//! `cargo bench -p omuzaki-anim --bench triggers`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim::triggers::{TriggerFades, TriggerMode, Triggers, expression_slot};
use omuzaki_anim_api::{BlendMode, ChannelBuf, Layer};
use omuzaki_face_api::{ExpressionId, ExpressionSet};

fn triggers(c: &mut Criterion) {
    let set = ExpressionSet::starter();
    let mut t = Triggers::new(TriggerFades::default());
    for (i, name) in ["happy", "angry"].into_iter().enumerate() {
        let id = ExpressionId::new(name).unwrap_or_else(|e| panic!("expression id {name}: {e}"));
        let slot = expression_slot(&set, &id, &ChannelBuf::NEUTRAL, TriggerMode::Toggle)
            .unwrap_or_else(|e| panic!("starter expression {name}: {e}"));
        t.set_slot(i, Some(slot)).unwrap_or_else(|e| panic!("slot {i}: {e}"));
    }
    let mut out = Layer::absent(BlendMode::Override);
    let mut n = 0_u32;
    c.bench_function("triggers: one step, alternating slots", |b| {
        b.iter(|| {
            // A press every 30 steps keeps the fade moving rather than parked at 0 or 1.
            n = n.wrapping_add(1);
            if n.is_multiple_of(30) {
                t.press((n / 30 % 2) as usize);
            }
            t.step(black_box(1.0 / 60.0), &mut out);
            black_box(&out);
        });
    });
}

criterion_group!(benches, triggers);
criterion_main!(benches);
