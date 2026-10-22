//! `Injector::push` (#727): every custom parameter and every VTS default from one frame's channels, on the frame thread.

// A benchmark's setup may panic: a panic is a clear failure.
#![allow(clippy::unwrap_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::channel::{CHANNELS, ChannelBuf};
use omuzaki_vts::inject::{ChannelMap, Injector};

fn bench(c: &mut Criterion) {
    let mut map = ChannelMap::builtin().unwrap();
    let defaults: Vec<String> = map.defaults().map(|(name, _)| name.to_owned()).collect();
    for name in &defaults {
        map.enable_default(name, true).unwrap();
    }
    let injector = Injector::new(&map);
    let mut buf = ChannelBuf::default();
    for (n, info) in CHANNELS.iter().enumerate().take(73) {
        buf.set(info.channel, f32::from(u8::try_from(n).unwrap()) / 100.0);
    }
    c.bench_function("vts push, 90 parameters", |b| b.iter(|| injector.push(std::hint::black_box(&buf), true)));
}

criterion_group!(benches, bench);
criterion_main!(benches);
