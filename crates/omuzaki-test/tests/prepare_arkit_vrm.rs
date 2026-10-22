//! The ARKit-52 synthesis of `omuzaki-io-ops` through the real VRM exporter (#1663). `omuzaki-io-ops` may not depend on
//! `omuzaki-io-vrm` (`cargo xtask lint-deps`), so its own test checks the export contract's preflight; this app-crate
//! test checks the same fixture (`omuzaki-io-mock`'s `vroid_like_avatar`) through `omuzaki-io-vrm`, so the two can't
//! drift.

// Test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_io_api::{
    ExportCtx, ExportOptions, ImportCtx, ImportInput, ImportOptions, IoError, PerfectSync, Severity, run_export,
    run_import,
};
use omuzaki_io_mock::fixtures::vroid_like_avatar;
use omuzaki_io_ops::prepare::arkit::{Synthesis, synthesise};
use omuzaki_io_vrm::{Vrm1Exporter, Vrm1Importer};

#[test]
fn the_vrm_export_refuses_exactly_the_channels_synthesis_left_missing() {
    let mut scene = vroid_like_avatar();
    let report = synthesise(&mut scene);
    let missing: Vec<&str> =
        report.channels.iter().filter(|c| c.how == Synthesis::Missing).map(|c| c.channel).collect();
    assert!(!missing.is_empty() && missing.len() < 52, "{} missing", missing.len());
    assert!(scene.check().findings.is_empty(), "{:#?}", scene.check());

    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Arkit52);
    let Err(IoError::Rejected(refusal)) = run_export(&Vrm1Exporter, &scene, &ExportCtx::new(&options)) else {
        panic!("an export with missing channels is refused");
    };
    let errors: Vec<_> = refusal.findings.iter().filter(|f| f.severity == Severity::Error).collect();
    assert!(errors.iter().all(|f| f.rule == "perfect_sync.missing_channel"), "{errors:#?}");
    let mut named: Vec<&str> =
        errors.iter().map(|f| f.location.as_deref().unwrap().trim_start_matches("avatar/expressions/")).collect();
    named.sort_unstable();
    let mut expected = missing.clone();
    expected.sort_unstable();
    assert_eq!(named, expected, "one error per missing channel, and no other");
}

#[test]
fn the_synthesised_channels_survive_the_vrm_round_trip() {
    let mut scene = vroid_like_avatar();
    let report = synthesise(&mut scene);
    let built: Vec<&str> = report.channels.iter().filter(|c| c.how != Synthesis::Missing).map(|c| c.channel).collect();
    assert_ne!(built.len(), 0, "synthesis builds some channels");
    // With the 52 channels not required, the scene exports as it is.
    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Off);
    let out = run_export(&Vrm1Exporter, &scene, &ExportCtx::new(&options)).expect("exports");
    let bytes = &out.files[0].bytes;
    let back = run_import(&Vrm1Importer, &ImportInput::new(bytes), &ImportCtx::new(&ImportOptions::default()))
        .expect("imports")
        .scene;
    assert!(back.check().findings.is_empty(), "{:#?}", back.check());
    let expressions = &back.avatar.as_ref().unwrap().expressions;
    for channel in built {
        let found = expressions.iter().find(|(n, _)| n.as_str() == channel);
        assert!(found.is_some_and(|(_, e)| !e.is_inert()), "{channel} came back with its binds");
    }
}
