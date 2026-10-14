//! Local smoke test on real files. The sample character export is optional local test material: it is kept
//! outside the repository, is never committed, and this test only reads it. Run it by hand:
//!
//! ```text
//! cargo test --release -p omuzaki-gltf --test smoke -- --ignored --nocapture
//! OMUZAKI_SMOKE_GLB=/path/to/other.glb cargo test ... (any GLB)
//! OMUZAKI_SMOKE_OUT=/path/to/scratch.glb  (also write the re-exported file, for the Khronos validator)
//! ```

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::time::Instant;

use common::{assert_same_scene, export_glb, import_with};
use omuzaki_io_api::{ImportOptions, NoResolver};
use omuzaki_io_mock::fixtures::authored_provenance;

/// The sample model's place under the assets folder (`$OMUZAKI_ASSETS`, default `assets-local`).
const MIRA: &str = "smoke.glb";

fn path() -> String {
    std::env::var("OMUZAKI_SMOKE_GLB")
        .unwrap_or_else(|_| omuzaki_core::assets::assets_root().join(MIRA).to_string_lossy().into_owned())
}

#[test]
#[ignore = "reads a local file from the assets folder; run by hand"]
fn imports_exports_and_reimports_a_real_model() {
    let path = path();
    let Ok(bytes) = std::fs::read(&path) else {
        println!("skipped: {path} isn't there");
        return;
    };
    let start = Instant::now();
    let out = import_with(&bytes, &ImportOptions::default(), &NoResolver).expect("import");
    let imported = start.elapsed();
    println!("import {} bytes in {imported:?}: {:?}", bytes.len(), out.scene.summary());
    for f in &out.report.findings {
        println!("  {:?} {} {}", f.severity, f.rule, f.message);
    }
    assert!(!out.report.has_errors());

    let mut scene = out.scene;
    scene.provenance = Some(authored_provenance()); // the real record is the author's to set; this only lets the export run
    let start = Instant::now();
    let glb = export_glb(&scene);
    println!("export {} bytes in {:?}", glb.len(), start.elapsed());
    assert_eq!(glb, export_glb(&scene), "deterministic");
    if let Ok(out) = std::env::var("OMUZAKI_SMOKE_OUT") {
        std::fs::write(out, &glb).expect("write the re-exported file");
    }

    let start = Instant::now();
    let again = import_with(&glb, &ImportOptions::default(), &NoResolver).expect("re-import").scene;
    println!("re-import in {:?}", start.elapsed());
    assert_same_scene(&again, &scene);
    assert_eq!(glb, export_glb(&again), "export(import(export(x))) == export(x)");
}
