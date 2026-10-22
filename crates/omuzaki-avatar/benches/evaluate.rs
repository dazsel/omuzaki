//! `ExpressionEvaluator::evaluate` with 60 expressions on 4 meshes (#865: at most 20 µs on the build box, class
//! `reported`, spec §10). Every expression has two morph binds and every fourth a colour bind; all 52 `ARKit` channels
//! and 8 custom slots drive one each, and every channel is non-zero, so the whole table runs each frame.
#![allow(clippy::cast_precision_loss, clippy::expect_used)] // bench setup; loop counters as data

#[path = "../tests/common/mod.rs"]
mod common;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::{ARKIT_COUNT, Channel, ChannelBuf};
use omuzaki_avatar::evaluator::ExpressionEvaluator;
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::expression::{AvatarExpression, ColourBind, ExprIx, MorphBind, Overrides};
use omuzaki_core::glam::Vec4;
use omuzaki_io_api::{ColorBindKind, Override};
use omuzaki_store_api::ContentHash;

const EXPRESSIONS: u16 = 60;
const MESHES: u16 = 4;
const TARGETS: u16 = 30;

fn avatar() -> ImportedAvatar {
    let mut a = ImportedAvatar::new(ContentHash([3; 32]), common::skeleton(true));
    a.meshes = (0..MESHES).map(|_| common::mesh(usize::from(TARGETS))).collect();
    a.materials.insert(common::material(), omuzaki_char_api::MaterialSpec::toon(omuzaki_core::colour::Colour::WHITE));
    for i in 0..EXPRESSIONS {
        let channel = if usize::from(i) < ARKIT_COUNT {
            Channel::from_index(usize::from(i))
        } else {
            Channel::custom(usize::from(i) - ARKIT_COUNT)
        }
        .expect("an assigned channel");
        let mut e = AvatarExpression::new(common::name(channel.name()))
            .with_morph(MorphBind::new(i % MESHES, i % TARGETS, 1.0))
            .with_morph(MorphBind::new((i + 1) % MESHES, (i * 7) % TARGETS, 0.5));
        if i % 4 == 0 {
            e = e.with_colour(ColourBind::new(
                common::material(),
                ColorBindKind::Color,
                Vec4::ONE,
                Vec4::new(1.0, 0.5, 0.5, 1.0),
            ));
        }
        if i >= 52 {
            e = e.with_overrides(Overrides::new(Override::Blend, Override::None, Override::Block));
        }
        a.expressions.push(e);
        a.drivers.set(channel, Some(ExprIx(i)));
    }
    a.gaze = omuzaki_avatar_api::gaze::AvatarGaze::NativeArkit;
    a
}

fn bench(c: &mut Criterion) {
    let a = avatar();
    assert_eq!(a.validate(), Ok(()));
    let evaluator = ExpressionEvaluator::new(&a);
    let (mut weights, mut binds) = (evaluator.morph_weights(), evaluator.bind_deltas());
    let mut channels = ChannelBuf::NEUTRAL;
    for i in 0..usize::from(EXPRESSIONS) {
        let channel = if i < ARKIT_COUNT { Channel::from_index(i) } else { Channel::custom(i - ARKIT_COUNT) };
        channels.set(channel.expect("assigned"), (i as f32 * 0.37).fract());
    }
    c.bench_function("evaluate_60_expressions_4_meshes", |b| {
        b.iter(|| evaluator.evaluate(std::hint::black_box(&channels), &mut weights, &mut binds));
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
