//! Put and get throughput of the on-disk store for 1 MiB and 100 MiB assets.
//!
//! `put` times hashing, the staged writes, fsyncs and the rename into place, into a fresh store each iteration so no
//! put hits the dedup fast path. `get` times the read plus re-hash with the file in the page cache (a cold read is
//! bounded by the disk, not by this code). Run with `TMPDIR` on the disk you care about; by default that is `/tmp`.
//!
//!     cargo bench -p omuzaki-store
// A panic is a clear failure in a benchmark; error plumbing would only obscure it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use omuzaki_store::DiskStore;
use omuzaki_store_api::{Commercial, Store};
use omuzaki_store_mock::conformance::{payload, sample_provenance};

const SIZES: [(&str, usize); 2] = [("1MiB", 1 << 20), ("100MiB", 100 << 20)];

fn put(c: &mut Criterion) {
    let mut group = c.benchmark_group("put");
    for (name, len) in SIZES {
        let bytes = payload(name, len);
        group.throughput(Throughput::Bytes(len as u64)).sample_size(if len > 1 << 20 { 10 } else { 50 });
        group.bench_function(name, |b| {
            b.iter_batched(
                || {
                    let dir = tempfile::tempdir().unwrap();
                    let store = DiskStore::open(dir.path()).unwrap();
                    (dir, store)
                },
                // Returning the directory drops (and deletes) it outside the timed section.
                |(dir, store)| {
                    store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
                    dir
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn get(c: &mut Criterion) {
    let mut group = c.benchmark_group("get");
    for (name, len) in SIZES {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskStore::open(dir.path()).unwrap();
        let hash = store.put(&payload(name, len), sample_provenance(Commercial::Ok)).unwrap();
        group.throughput(Throughput::Bytes(len as u64)).sample_size(if len > 1 << 20 { 10 } else { 50 });
        group.bench_function(name, |b| b.iter(|| store.get(&hash).unwrap()));
    }
    group.finish();
}

criterion_group!(benches, put, get);
criterion_main!(benches);
