//! An opt-in smoke test on a model that must never be committed (private test material, `docs/licensing/policy.md`).
//!
//! Point `OMUZAKI_SMOKE_VRM` at a `.vrm` and run
//! `cargo test -p omuzaki-io-vrm --test private_smoke -- --nocapture`. Without the variable the test does nothing, so CI
//! and fresh checkouts pass. It imports the file with whichever importer recognises it, prints what it found,
//! exports it again (privately, without perfect sync), imports that, and checks the second export is byte-identical.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Instant;

use omuzaki_io_api::{
    ExportCtx, ImportCtx, ImportInput, ImportOptions, Severity, find_exporter, find_importer, run_export, run_import,
    sniff,
};

#[test]
fn a_private_model_round_trips() {
    // Linking is what registers the formats, and a linker drops a crate nothing refers to, so touch it.
    let _ = omuzaki_io_api::Importer::info(&omuzaki_io_vrm::Vrm1Importer);
    let Ok(path) = std::env::var("OMUZAKI_SMOKE_VRM") else {
        eprintln!("OMUZAKI_SMOKE_VRM is not set; skipping the private smoke test");
        return;
    };
    let bytes = std::fs::read(&path).expect("read the model");
    let name = std::path::Path::new(&path).file_name().and_then(|n| n.to_str()).unwrap_or("model.vrm").to_owned();
    let input = ImportInput::new(&bytes).named(&name);
    let detected = sniff(&input);
    let format = detected.first().expect("a VRM importer recognises the file").format;
    println!("{name}: {} bytes, detected {detected:?}", bytes.len());

    let options = ImportOptions::default();
    let started = Instant::now();
    let imported =
        run_import(find_importer(format).unwrap().as_ref(), &input, &ImportCtx::new(&options)).expect("import");
    println!("import took {:?}", started.elapsed());
    println!("{:#?}", imported.scene.summary());
    for severity in [Severity::Error, Severity::Warning, Severity::Info] {
        for f in imported.report.findings.iter().filter(|f| f.severity == severity) {
            println!("  {severity:?} {} {} {}", f.rule, f.message, f.location.as_deref().unwrap_or(""));
        }
    }
    let p = imported.scene.provenance.as_ref().unwrap();
    println!("provenance: licence {} commercial {:?} author {:?}", p.licence, p.commercial, p.author);

    let export_options = common::options();
    for out_format in [format, if format == "vrm1" { "vrm0" } else { "vrm1" }] {
        let exporter = find_exporter(out_format).unwrap();
        let started = Instant::now();
        let first = run_export(exporter.as_ref(), &imported.scene, &ExportCtx::new(&export_options)).expect("export");
        println!("export to {out_format}: {} bytes in {:?}", first.files[0].bytes.len(), started.elapsed());
        let again = run_import(
            find_importer(out_format).unwrap().as_ref(),
            &ImportInput::new(&first.files[0].bytes).named("again.vrm"),
            &ImportCtx::new(&options),
        )
        .expect("re-import");
        let second = run_export(exporter.as_ref(), &again.scene, &ExportCtx::new(&export_options)).expect("re-export");
        if out_format == format {
            assert_eq!(first.files[0].bytes, second.files[0].bytes, "export(import(export(x))) == export(x)");
        }
        let (before, after) = (imported.scene.summary(), again.scene.summary());
        println!("{out_format}: re-imported {after:?}");
        // Same format: nothing changes. The other generation adds tail nodes to spring chains (0.x) and keeps only the
        // colliders some chain uses, but never loses geometry, textures or expressions.
        let geometry = |s: &omuzaki_io_api::SceneSummary| {
            (s.vertices, s.triangles, s.images, s.image_bytes, s.expressions, s.humanoid_bones)
        };
        assert_eq!(geometry(&after), geometry(&before), "{out_format}: the model keeps its content");
        if out_format == format {
            assert_eq!(after, before);
        }
    }
}
