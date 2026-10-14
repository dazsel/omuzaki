//! Perfect sync: the 52 `ARKit` channels as custom expressions. Exports ask for them by default and say exactly what is
//! missing; imports report how complete a model is; both VRM generations carry them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

mod common;

use omuzaki_io_api::{ARKIT_52, ExportOptions, Expression, ExpressionName, IoError, PerfectSync, Severity};
use omuzaki_io_vrm::{Vrm0Exporter, Vrm0Importer, Vrm1Exporter, Vrm1Importer};

use common::{avatar, export_with, import, vrm0_friendly, with_arkit52};

fn name(n: &str) -> ExpressionName {
    ExpressionName::parse(n).unwrap()
}

#[test]
fn exports_ask_for_all_52_channels_by_default_and_list_what_is_missing() {
    let (scene, _) = avatar();
    for vrm0 in [false, true] {
        let (scene, _) = if vrm0 { vrm0_friendly(avatar()) } else { (scene.clone(), common::avatar().1) };
        let result = if vrm0 {
            export_with(&Vrm0Exporter, &scene, &ExportOptions::default())
        } else {
            export_with(&Vrm1Exporter, &scene, &ExportOptions::default())
        };
        let Err(IoError::Rejected(report)) = result else {
            panic!("an avatar without channels must not export by default")
        };
        let missing: Vec<_> = report.findings.iter().filter(|f| f.rule == "perfect_sync.missing_channel").collect();
        // The fixture has one ARKit-named channel of its own (`jawOpen`), so 51 are missing.
        assert_eq!(missing.len(), 51, "vrm0={vrm0}");
        assert!(missing.iter().all(|f| f.severity == Severity::Error && f.hint.is_some()));
        assert!(!missing.iter().any(|f| f.message.contains("jawOpen")));
        // Turning perfect sync off is the explicit way out.
        let off = ExportOptions::default().with_perfect_sync(PerfectSync::Off);
        assert!(if vrm0 {
            export_with(&Vrm0Exporter, &scene, &off).is_ok()
        } else {
            export_with(&Vrm1Exporter, &scene, &off).is_ok()
        });
    }
}

#[test]
fn a_complete_set_exports_by_default_and_is_recognised_and_kept_on_import() {
    let (scene, _) = with_arkit52(avatar());
    let (file, report) = export_with(&Vrm1Exporter, &scene, &ExportOptions::default()).unwrap();
    assert!(!report.has_errors());
    let out = import(&Vrm1Importer, &file);
    assert!(out.report.has_rule("vrm.perfect_sync_complete"), "{:#?}", out.report);
    assert!(!out.report.has_rule("vrm.perfect_sync_partial"));
    let (a, b) = (scene.avatar.as_ref().unwrap(), out.scene.avatar.as_ref().unwrap());
    for (i, channel) in ARKIT_52.iter().enumerate() {
        let (x, y) = (&a.expressions[&name(channel)], &b.expressions[&name(channel)]);
        assert_eq!(x, y, "{channel}");
        assert_eq!(y.morph_binds[0].index as usize, i);
    }
    assert_eq!(out.scene.meshes, scene.meshes, "52 sparse morph targets come back exactly");
    // Sparse storage keeps the file small: 52 targets of an 81-vertex face would be 52 x 972 bytes dense.
    let (doc, bin) = common::parts(&file);
    let face = doc["meshes"].as_array().unwrap().iter().find(|m| m["name"] == "Face").unwrap();
    let sparse = face["primitives"][0]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| doc["accessors"][t["POSITION"].as_u64().unwrap() as usize].get("sparse").is_some());
    assert_eq!(sparse.count(), 52);
    assert!(bin.len() < 52 * 972 + 40_000, "{} bytes", bin.len());
}

#[test]
fn perfect_sync_survives_a_trip_through_vrm0() {
    let (scene, _) = vrm0_friendly(with_arkit52(avatar()));
    let (file, _) = export_with(&Vrm0Exporter, &scene, &ExportOptions::default()).unwrap();
    let (doc, _) = common::parts(&file);
    let groups = doc["extensions"]["VRM"]["blendShapeMaster"]["blendShapeGroups"].as_array().unwrap();
    let jaw = groups.iter().find(|g| g["name"] == "jawOpen").unwrap();
    assert_eq!(jaw["presetName"], "unknown", "channels are custom clips named like ARKit's keys");
    let out = import(&Vrm0Importer, &file);
    assert!(out.report.has_rule("vrm.perfect_sync_complete"), "{:#?}", out.report);
    let (a, b) = (scene.avatar.as_ref().unwrap(), out.scene.avatar.as_ref().unwrap());
    for channel in ARKIT_52 {
        assert_eq!(a.expressions[&name(channel)], b.expressions[&name(channel)], "{channel}");
    }
}

#[test]
fn an_inert_channel_is_a_warning_and_an_error_in_strict_mode() {
    let (mut scene, _) = with_arkit52(avatar());
    scene.avatar.as_mut().unwrap().expressions.insert(name("tongueOut"), Expression::new());
    let (_, report) = export_with(&Vrm1Exporter, &scene, &ExportOptions::default()).unwrap();
    let inert: Vec<_> = report.findings.iter().filter(|f| f.rule == "perfect_sync.inert_channel").collect();
    assert_eq!(inert.len(), 1);
    assert!(inert[0].message.contains("tongueOut") && inert[0].severity == Severity::Warning);
    let strict = ExportOptions::default().strict();
    let Err(IoError::Rejected(r)) = export_with(&Vrm1Exporter, &scene, &strict) else {
        panic!("strict mode refuses it")
    };
    assert!(r.has_rule("perfect_sync.inert_channel"));
}

#[test]
fn partial_sets_and_wrong_capitalisation_are_reported_on_import() {
    let (mut scene, _) = with_arkit52(avatar());
    let av = scene.avatar.as_mut().unwrap();
    for gone in ["eyeBlinkLeft", "mouthClose", "tongueOut"] {
        av.expressions.remove(&name(gone));
    }
    let renamed = av.expressions.remove(&name("jawOpen")).unwrap();
    av.expressions.insert(name("JawOpen"), renamed);
    let off = ExportOptions::default().with_perfect_sync(PerfectSync::Off);
    let (file, _) = export_with(&Vrm1Exporter, &scene, &off).unwrap();
    let out = import(&Vrm1Importer, &file);
    let note =
        out.report.findings.iter().find(|f| f.rule == "vrm.perfect_sync_partial").expect("a partial set is noted");
    assert_eq!(note.severity, Severity::Info);
    assert!(note.message.starts_with("48 of 52"), "{}", note.message);
    assert!(note.message.contains("eyeBlinkLeft") && note.message.contains("capitalisation"), "{}", note.message);
    assert!(!out.report.has_rule("vrm.perfect_sync_complete"));
    // A model with no ARKit channels at all says nothing.
    let (plain, _) = avatar();
    let mut plain = plain;
    plain.avatar.as_mut().unwrap().expressions.remove(&name("jawOpen"));
    let out = import(&Vrm1Importer, &common::export(&Vrm1Exporter, &plain));
    assert!(!out.report.has_rule("vrm.perfect_sync_partial") && !out.report.has_rule("vrm.perfect_sync_complete"));
}
