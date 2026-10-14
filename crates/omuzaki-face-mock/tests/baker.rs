//! The registry finds the mock baker, and what it bakes exports through `omuzaki-io-mock`'s exporter.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::glam::Vec3;
use omuzaki_face_api::{BakerInput, face_baker};
use omuzaki_face_mock::{mock_profile, mock_projection};
use omuzaki_io_api::{
    ARKIT_52, BakeRequest, ExportCtx, ExportOptions, ExpressionName, NoProgress, Realisation, Severity, run_export,
};
use omuzaki_io_mock::MockVrm1Exporter;
use omuzaki_io_mock::fixtures::{BODY_NODE, tiny_avatar};

#[test]
fn the_registry_finds_the_mock_baker_and_its_bake_exports() {
    let factory = face_baker().expect("no priority conflict").expect("the mock face baker is registered");
    let profile = mock_profile();
    let baker = factory
        .create(BakerInput { profile: &profile, projection: mock_projection(), head_centre: Vec3::new(0.0, 0.5, 0.0) })
        .expect("the mock factory always creates a baker");

    // The fixture minus its own expressions and morph targets, so the baker owns every channel.
    let mut scene = tiny_avatar();
    scene.avatar.as_mut().unwrap().expressions.clear();
    scene.meshes[0].primitives[0].morph_targets.clear();
    scene.meshes[0].morph_target_names.clear();
    let channels: Vec<ExpressionName> = ARKIT_52.iter().map(|n| ExpressionName::parse(n).unwrap()).collect();

    let report = baker.bake(&mut scene, &BakeRequest::new(channels, BODY_NODE), &NoProgress).expect("bake");
    assert_eq!(report.channels.len(), 52);
    assert!(report.channels.iter().all(|c| matches!(c.realisation, Realisation::Inert { .. })));
    assert_eq!(scene.avatar.as_ref().unwrap().expressions.len(), 52, "one expression per channel");

    let options = ExportOptions::default();
    let out = run_export(&MockVrm1Exporter, &scene, &ExportCtx::new(&options)).expect("export");
    assert_eq!(out.report.count(Severity::Error), 0);
    assert_eq!(out.report.count(Severity::Warning), 52, "each inert channel is one warning");
}
