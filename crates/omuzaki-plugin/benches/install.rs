//! Benches of design §5.2: `install_2mib` (read, hash, stage, copy and compile a package with a 2 MiB component;
//! budget 1.5 s worst of 5 on `R`) and `load_cached` (the component from the compiled-code cache; budget 20 ms p95).

// A bench's setup may panic: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::io::Write as _;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_plugin::cache::CodeCache;
use omuzaki_plugin::install::InstallRoot;
use omuzaki_plugin::package::{PackageLimits, PluginPackage};
use omuzaki_plugin::{HostConfig, PluginEngine};

/// A component of about 2 MiB of code: many small functions, so compiling has real work to do.
fn big_component() -> Vec<u8> {
    let body =
        "(local.set 1 (i32.add (local.get 0) (i32.const 7))) (local.set 0 (i32.mul (local.get 1) (i32.const 3))) "
            .repeat(12);
    let funcs: String = (0..9000)
        .map(|i| format!("(func $f{i} (param i32) (result i32) (local i32) {body} (local.get 0))\n"))
        .collect::<Vec<_>>()
        .concat();
    let text = format!(
        r#"(component
  (core module $m (memory (export "memory") 1)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) (i32.const 1024))
    {funcs}
    (func (export "init") (param i32 i32) (result i32) (i32.store8 (i32.const 16) (i32.const 0)) (i32.const 16)))
  (core instance $i (instantiate $m))
  (alias core export $i "memory" (core memory $memory))
  (alias core export $i "realloc" (core func $realloc))
  (alias core export $i "init" (core func $init-core))
  (func $init (param "config" string) (result (result (error string)))
    (canon lift (core func $init-core) (memory $memory) (realloc $realloc)))
  (instance $lifecycle (export "init" (func $init)))
  (export "omuzaki:plugin/lifecycle@0.1.0" (instance $lifecycle)))"#
    );
    wat::parse_str(&text).unwrap()
}

fn zip_of(component: &[u8]) -> Vec<u8> {
    let manifest = "id = \"big\"\nversion = \"0.1.0\"\napi = \"^0.1\"\nlicence = \"MIT\"\nname = \"Big\"\ncomponent = \"plugin.wasm\"\n";
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    writer.start_file("plugin.toml", options).unwrap();
    writer.write_all(manifest.as_bytes()).unwrap();
    writer.start_file("plugin.wasm", options).unwrap();
    writer.write_all(component).unwrap();
    writer.finish().unwrap().into_inner()
}

fn benches(c: &mut Criterion) {
    let engine = PluginEngine::new(HostConfig::default(), Arc::new(FakeClock::new(Seconds(0.0)))).unwrap();
    let component = big_component();
    assert!(component.len() > 1024 * 1024, "{} bytes", component.len());
    let archive = zip_of(&component);
    let mut group = c.benchmark_group("plugin");
    group.sample_size(10);
    group.bench_function("install_2mib", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let package = PluginPackage::open_zip(&archive, &PackageLimits::default()).unwrap();
            let installed = InstallRoot::new(dir.path().join("data")).install(&package).unwrap();
            CodeCache::new(dir.path().join("cache")).load(&engine, &installed.hash, package.component()).unwrap()
        });
    });
    let dir = tempfile::tempdir().unwrap();
    let cache = CodeCache::new(dir.path());
    let hash = omuzaki_store_api::ContentHash([1; 32]);
    cache.load(&engine, &hash, &component).unwrap();
    group.bench_function("load_cached", |b| b.iter(|| cache.load(&engine, &hash, &component).unwrap()));
    group.finish();
}

criterion_group!(plugin, benches);
criterion_main!(plugin);
