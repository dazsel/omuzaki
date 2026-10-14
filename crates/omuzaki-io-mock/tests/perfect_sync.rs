//! Perfect-sync export end to end: bake the 52 `ARKit` channels with the mock baker, check completeness, export a
//! VRM, read it back.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_io_api::{
    ARKIT_52, BakeBudget, BakeRequest, ExportCtx, ExportOptions, ExpressionBaker, ExpressionName, ImportCtx,
    ImportInput, ImportOptions, IoError, IoScene, NoProgress, PerfectSync, Realisation, Severity, extra_expression,
    preflight, run_export, run_import,
};
use omuzaki_io_mock::fixtures::{BODY_NODE, options_without_face_channels, tiny_avatar};
use omuzaki_io_mock::{MockBaker, MockVrm1Exporter, MockVrm1Importer};

fn arkit_channels() -> Vec<ExpressionName> {
    ARKIT_52.iter().map(|n| ExpressionName::parse(n).unwrap()).collect()
}

/// The fixture minus its own expressions and morph target, so the baker owns every channel.
fn bare_avatar() -> IoScene {
    let mut s = tiny_avatar();
    s.avatar.as_mut().unwrap().expressions.clear();
    s.meshes[0].primitives[0].morph_targets.clear();
    s.meshes[0].morph_target_names.clear();
    s
}

#[test]
fn perfect_sync_is_on_by_default_and_extras_are_not() {
    assert_eq!(ExportOptions::default().perfect_sync, PerfectSync::Arkit52);
    // Without baking, a default export of an avatar is refused with one error per missing channel.
    let report = preflight(&bare_avatar(), &ExportOptions::default());
    assert_eq!(report.errors().filter(|f| f.rule == "perfect_sync.missing_channel").count(), 52);
    // Off means nothing is required.
    assert_eq!(preflight(&bare_avatar(), &options_without_face_channels()).findings, []);
}

#[test]
fn a_scene_without_an_avatar_is_not_asked_for_face_channels() {
    let mut prop = bare_avatar();
    prop.avatar = None;
    let report = preflight(&prop, &ExportOptions::default());
    assert!(!report.errors().any(|f| f.rule.starts_with("perfect_sync.")), "{report:#?}");
}

#[test]
fn baking_then_exporting_carries_all_52_channels() {
    let mut scene = bare_avatar();
    let request = BakeRequest::new(arkit_channels(), BODY_NODE);
    let report = MockBaker.bake(&mut scene, &request, &NoProgress).expect("bake");
    assert_eq!(report.channels.len(), 52);
    assert!(report.channels.iter().all(|c| c.realisation == Realisation::Morph { targets: 1 }));
    assert_eq!(scene.meshes[0].morph_target_count(), 52);
    assert_eq!(scene.meshes[0].morph_target_names[17], "jawOpen", "target names follow the channel order");

    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Arkit52);
    let out = run_export(&MockVrm1Exporter, &scene, &ExportCtx::new(&options)).expect("export");
    assert_eq!(out.report.count(Severity::Error), 0);

    let import_options = ImportOptions::default();
    let back = run_import(&MockVrm1Importer, &ImportInput::new(&out.files[0].bytes), &ImportCtx::new(&import_options))
        .expect("import")
        .scene;
    let avatar = back.avatar.expect("avatar");
    for (i, name) in ARKIT_52.iter().enumerate() {
        let e = &avatar.expressions[&ExpressionName::parse(name).unwrap()];
        assert_eq!((e.morph_binds.len(), e.morph_binds[0].index as usize), (1, i), "{name}");
    }
    assert_eq!(avatar.expressions.len(), 52);
}

#[test]
fn a_morph_budget_makes_the_remaining_channels_inert_and_strict_mode_refuses_that() {
    let mut scene = bare_avatar();
    let mut request = BakeRequest::new(arkit_channels(), BODY_NODE);
    request.budget = BakeBudget { max_morph_targets: Some(10), max_atlas_px: None };
    let report = MockBaker.bake(&mut scene, &request, &NoProgress).expect("bake");
    let inert = report.channels.iter().filter(|c| matches!(c.realisation, Realisation::Inert { .. })).count();
    assert_eq!((inert, scene.meshes[0].morph_target_count()), (42, 10));

    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Arkit52);
    let out = run_export(&MockVrm1Exporter, &scene, &ExportCtx::new(&options)).expect("inert channels are warnings");
    assert_eq!(out.report.findings.iter().filter(|f| f.rule == "perfect_sync.inert_channel").count(), 42);
    let strict = options.strict();
    assert!(matches!(run_export(&MockVrm1Exporter, &scene, &ExportCtx::new(&strict)), Err(IoError::Rejected(_))));
}

#[test]
fn extras_are_exported_as_prefixed_custom_expressions() {
    let mut scene = bare_avatar();
    let mut channels = arkit_channels();
    channels.push(extra_expression("heart-eyes").unwrap());
    MockBaker.bake(&mut scene, &BakeRequest::new(channels, BODY_NODE), &NoProgress).expect("bake");
    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Arkit52WithExtras);
    let out = run_export(&MockVrm1Exporter, &scene, &ExportCtx::new(&options)).expect("export");
    let (doc, _) = omuzaki_io_mock::fixtures::glb_parts(&out.files[0].bytes).unwrap();
    let custom = doc["extensions"]["VRMC_vrm"]["expressions"]["custom"].as_object().unwrap();
    assert_eq!(custom.len(), 53);
    assert!(custom.contains_key("omuzaki_heart-eyes") && custom.contains_key("jawOpen"));
    assert!(doc["extensions"]["VRMC_vrm"]["expressions"]["preset"].get("jawOpen").is_none(), "never a preset");
}

#[test]
fn baking_needs_a_mesh_to_attach_to() {
    let mut scene = bare_avatar();
    let err = MockBaker
        .bake(&mut scene, &BakeRequest::new(arkit_channels(), omuzaki_io_api::NodeIx(1)), &NoProgress)
        .unwrap_err();
    assert!(matches!(err, IoError::Unsupported { .. }));
}
