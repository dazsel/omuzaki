//! Snapshot cost (D6): a read is an `Arc` clone and a lookup; a change rebuilds the whole map.
//!
//! The rebuild is measured through `set_raw`, the only caller, on a store of 300 keys (the order of magnitude the
//! settings page is designed for). `set_raw` also validates and logs, so the dry run beside it is the part of the cost
//! that is not the rebuild.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_ops_api::{Actor, Mode};
use omuzaki_settings::{Paths, Store};
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::registry::SettingRegistration;
use omuzaki_settings_api::service::{Layer, Settings};
use omuzaki_settings_api::setting::Setting;
use serde_json::json;

const KEYS: usize = 300;

struct Level;

impl Setting for Level {
    type Value = u16;
    const META: SettingMeta = SettingMeta::new(
        "bench.k000",
        "level",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Excluded,
        Apply::Live,
        1,
    );
    fn default() -> u16 {
        0
    }
}

fn store(dir: &std::path::Path) -> Store {
    let registrations: Vec<&'static SettingRegistration> = (0..KEYS)
        .map(|n| {
            let mut registration = SettingRegistration::of::<Level>("bench");
            registration.meta.key = Box::leak(format!("bench.k{n:03}").into_boxed_str());
            &*Box::leak(Box::new(registration))
        })
        .collect();
    Store::open_with(&Paths::new(dir.join("config"), dir.join("data")), registrations)
}

#[allow(clippy::expect_used)] // a benchmark has nowhere to return an error to
fn snapshot(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let store = store(dir.path());
    let by = Actor::Person { name: "bench".into() };
    c.bench_function("snapshot read, 300 keys", |b| {
        b.iter(|| black_box(store.snapshot().value("bench.k150").is_some()));
    });
    c.bench_function("set dry run, 300 keys (validation only)", |b| {
        b.iter(|| store.set_raw("bench.k150", json!(7), Layer::Auto, Mode::DryRun, &by).is_ok());
    });
    let mut n = 0_u16;
    c.bench_function("set apply, 300 keys (snapshot rebuild included)", |b| {
        b.iter(|| {
            n = n.wrapping_add(1);
            store.set_raw("bench.k150", json!(n), Layer::Auto, Mode::Apply, &by).is_ok()
        });
    });
}

criterion_group!(benches, snapshot);
criterion_main!(benches);
