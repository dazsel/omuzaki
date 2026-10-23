//! The animation half of the io contract (`docs/design/io-api.md` D11): every `anim.*` rule, the rights preflight,
//! the registry, and a round trip through the mock's VRMA writer and reader.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_io_api::{
    ExportCtx, ExportOptions, ExpressionName, HUMAN_BONES, HumanBoneName, IoAnimation, IoError, Keys, PresetExpression,
    Purpose, Report, RestBone, RestHumanoid, Track, animation_formats, find_animation_exporter,
    licence_forbids_derivatives, run_export_animation,
};
use omuzaki_io_mock::{MockVrmaExporter, read_vrma};
use omuzaki_store_api::{Commercial, Provenance, Source};

/// A named way to break an animation (or its rest tree) so one rule fires.
type Break<T> = (&'static str, Box<dyn Fn(&mut T)>);

fn bone(name: &str) -> HumanBoneName {
    HumanBoneName::new(name).unwrap()
}

/// A rest tree of every bone but the eyes, about 1.6 m tall, each bone 10 cm from its parent (hips 0.9 m up).
fn rest() -> RestHumanoid {
    let bones = HUMAN_BONES
        .iter()
        .filter(|s| !matches!(s.name, "leftEye" | "rightEye"))
        .map(|s| {
            let at = if s.name == "hips" { Vec3::new(0.0, 0.9, 0.0) } else { Vec3::new(0.0, 0.1, 0.0) };
            RestBone::new(bone(s.name), at)
        })
        .collect();
    RestHumanoid::new(bones)
}

fn cc0() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: Some("Omuzaki contributors".into()),
        licence: "CC0-1.0".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

/// A wave: the right upper arm turns over a second, the hips bob, `happy` fades in, the gaze turns left.
fn wave() -> IoAnimation {
    let mut a = IoAnimation::new("wave", rest());
    let times = vec![0.0, 0.5, 1.0];
    a.tracks = vec![
        Track::expression(
            ExpressionName::Preset(PresetExpression::Happy),
            Keys::linear(times.clone(), vec![0.0, 1.0, 0.0]),
        ),
        Track::rotation(
            bone("rightUpperArm"),
            Keys::linear(times.clone(), vec![Quat::IDENTITY, Quat::from_rotation_z(-1.0), Quat::from_rotation_z(-0.5)]),
        ),
        Track::hips(Keys::linear(
            times.clone(),
            vec![Vec3::new(0.0, 0.9, 0.0), Vec3::new(0.0, 0.88, 0.0), Vec3::new(0.0, 0.9, 0.0)],
        )),
        Track::look_at(Keys::linear(times.clone(), vec![Quat::IDENTITY, Quat::from_rotation_y(0.3), Quat::IDENTITY])),
        Track::expression(ExpressionName::parse("omuzaki_smug").unwrap(), Keys::step(times, vec![0.0, 1.0, 0.0])),
    ];
    a.provenance = Some(cc0());
    a
}

fn rules(report: &Report) -> Vec<&str> {
    report.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn export(a: &IoAnimation, options: &ExportOptions) -> Result<Vec<u8>, IoError> {
    let out = run_export_animation(&MockVrmaExporter, a, &ExportCtx::new(options))?;
    Ok(out.files[0].bytes.clone())
}

#[test]
fn a_well_formed_animation_passes_and_summarises() {
    let a = wave();
    assert!(a.check().findings.is_empty(), "{:?}", a.check());
    let s = a.summary();
    assert_eq!((s.bone_tracks, s.hips_tracks, s.expression_tracks, s.look_at_tracks), (1, 1, 2, 1));
    assert_eq!((s.keys, s.rest_bones, s.duration), (15, 53, 1.0));
    assert_eq!(a.rest.hips_height(), Some(0.9));
    assert_eq!(a.rest.parent_of(bone("upperChest")), Some(bone("chest")));
    let head = a.rest.world_position(bone("head")).unwrap();
    assert!((head.y - 1.4).abs() < 1e-5, "hips 0.9 + spine, chest, upperChest, neck, head at 0.1 each: {head}");
}

#[test]
fn every_track_rule_fires_where_it_should() {
    let cases: Vec<Break<IoAnimation>> = vec![
        ("anim.no_tracks", Box::new(|a| a.tracks.clear())),
        ("anim.no_keys", Box::new(|a| a.tracks[2] = Track::hips(Keys::linear(vec![], vec![])))),
        (
            "anim.keys_length_mismatch",
            Box::new(|a| a.tracks[2] = Track::hips(Keys::linear(vec![0.0, 1.0], vec![Vec3::Y]))),
        ),
        ("anim.time_invalid", Box::new(|a| a.tracks[2] = Track::hips(Keys::linear(vec![-1.0], vec![Vec3::Y])))),
        ("anim.time_invalid", Box::new(|a| a.tracks[2] = Track::hips(Keys::linear(vec![f32::NAN], vec![Vec3::Y])))),
        (
            "anim.time_order",
            Box::new(|a| a.tracks[2] = Track::hips(Keys::linear(vec![0.0, 0.0], vec![Vec3::Y, Vec3::Y]))),
        ),
        ("anim.value_non_finite", Box::new(|a| a.tracks[2] = Track::hips(Keys::linear(vec![0.0], vec![Vec3::NAN])))),
        ("anim.value_non_finite", Box::new(|a| a.look_at_offset = Some(Vec3::INFINITY))),
        (
            "anim.rotation_not_unit",
            Box::new(|a| {
                a.tracks[1] =
                    Track::rotation(bone("spine"), Keys::linear(vec![0.0], vec![Quat::from_xyzw(0.0, 0.0, 0.0, 2.0)]));
            }),
        ),
        ("anim.duplicate_track", Box::new(|a| a.tracks.push(a.tracks[1].clone()))),
        (
            "anim.eye_bone",
            Box::new(|a| {
                a.tracks.push(Track::rotation(bone("leftEye"), Keys::linear(vec![0.0], vec![Quat::IDENTITY])));
            }),
        ),
        (
            "anim.expression_reserved",
            Box::new(|a| {
                let name = ExpressionName::Preset(PresetExpression::LookUp);
                a.tracks.push(Track::expression(name, Keys::linear(vec![0.0], vec![1.0])));
            }),
        ),
        ("anim.track_without_rest", Box::new(|a| a.rest.bones.retain(|b| b.bone != bone("rightUpperArm")))),
    ];
    for (rule, break_it) in cases {
        let mut a = wave();
        break_it(&mut a);
        let report = a.check();
        assert!(report.has_rule(rule), "{rule}: {:?}", rules(&report));
        assert!(report.has_errors(), "{rule} is an error");
    }
}

#[test]
fn rest_rules_need_the_required_bones_their_parents_and_a_standing_hips() {
    let breaks: Vec<Break<RestHumanoid>> = vec![
        ("anim.rest_invalid", Box::new(|r| r.bones.retain(|b| b.bone != bone("leftHand")))),
        ("anim.rest_invalid", Box::new(|r| r.bones.retain(|b| b.bone != bone("chest")))),
        ("anim.rest_invalid", Box::new(|r| r.bones.push(RestBone::new(bone("leftEye"), Vec3::Y)))),
        ("anim.rest_invalid", Box::new(|r| r.bones.push(r.bones[3]))),
        ("anim.rest_hips_height", Box::new(|r| r.bones[0].translation.y = 0.0005)),
    ];
    for (rule, break_it) in breaks {
        let mut a = wave();
        break_it(&mut a.rest);
        assert!(a.check().has_rule(rule), "{rule}: {:?}", rules(&a.check()));
    }
    // A missing optional bone is fine; its children attach to the nearest present ancestor.
    let mut a = wave();
    a.rest.bones.retain(|b| b.bone != bone("upperChest"));
    assert!(a.check().findings.is_empty(), "{:?}", a.check());
    assert_eq!(a.rest.parent_of(bone("neck")), Some(bone("chest")));
}

#[test]
fn warnings_flag_flips_weights_outside_the_range_and_too_many_custom_tracks() {
    let mut a = wave();
    a.tracks[1] = Track::rotation(
        bone("rightUpperArm"),
        Keys::linear(vec![0.0, 1.0], vec![Quat::IDENTITY, -Quat::from_rotation_z(0.1)]),
    );
    a.tracks[0] =
        Track::expression(ExpressionName::Preset(PresetExpression::Happy), Keys::linear(vec![0.0], vec![1.5]));
    for i in 0..101 {
        let name = ExpressionName::parse(&format!("extra_{i:03}")).unwrap();
        a.tracks.push(Track::expression(name, Keys::linear(vec![0.0], vec![0.5])));
    }
    let report = a.check();
    for rule in ["anim.quaternion_flip", "anim.weight_range", "anim.custom_expression_limit"] {
        assert!(report.has_rule(rule), "{rule}: {:?}", rules(&report));
    }
    assert!(!report.has_errors(), "{:?}", rules(&report));
}

#[test]
fn the_mock_registers_vrma_and_round_trips_every_track() {
    assert!(animation_formats().iter().any(|f| f.id == "vrma" && f.extensions == ["vrma"]));
    assert!(find_animation_exporter("vrma").is_some());
    let a = wave();
    let bytes = export(&a, &ExportOptions::default()).unwrap();
    let mut back = read_vrma(&bytes).unwrap();
    back.provenance = a.provenance.clone();
    assert_eq!(back.name, "wave");
    assert_eq!(back.rest.sorted(), a.rest.sorted());
    let mine: Vec<&Track> = a.canonical_tracks();
    let theirs: Vec<&Track> = back.canonical_tracks();
    assert_eq!(mine, theirs);
}

#[test]
fn equal_animations_give_equal_bytes_whatever_the_track_order() {
    let a = wave();
    let mut shuffled = wave();
    shuffled.tracks.reverse();
    shuffled.rest.bones.reverse();
    let options = ExportOptions::default().with_generator("Omuzaki test");
    assert_eq!(export(&a, &options).unwrap(), export(&shuffled, &options).unwrap());
}

#[test]
fn rights_need_a_record_pass_the_gate_to_publish_and_refuse_published_derivatives_of_nd_motion() {
    let publish = ExportOptions::default().with_purpose(Purpose::Publish);
    let mut a = wave();
    a.provenance = None;
    let Err(IoError::Rejected(r)) = export(&a, &ExportOptions::default()) else { panic!("no record") };
    assert!(r.has_rule("rights.provenance_missing"));

    let mut nc = wave();
    nc.provenance = Some(Provenance { licence: "CC-BY-NC-4.0".into(), commercial: Commercial::No, ..cc0() });
    let Err(IoError::Rejected(r)) = export(&nc, &publish) else { panic!("the gate refuses non-commercial") };
    assert!(r.has_rule("rights.gate_non_commercial"), "{:?}", rules(&r));
    assert!(export(&nc, &ExportOptions::default()).is_ok(), "a private export skips the gate");

    let mut nd = wave();
    nd.provenance = Some(Provenance { licence: "CC-BY-ND-4.0".into(), ..cc0() });
    let Err(IoError::Rejected(r)) = export(&nd, &publish) else { panic!("ND cannot be published") };
    assert_eq!(rules(&r), ["rights.licence_forbids_derivatives"]);
    let private = run_export_animation(&MockVrmaExporter, &nd, &ExportCtx::new(&ExportOptions::default())).unwrap();
    assert_eq!(rules(&private.report), ["rights.nd_private_export"]);

    let mut test = wave();
    test.provenance = Some(Provenance { private_test: true, ..cc0() });
    let Err(IoError::Rejected(r)) = export(&test, &publish) else { panic!("private test material never ships") };
    assert!(r.has_rule("rights.gate_private_test"));
    let private = run_export_animation(&MockVrmaExporter, &test, &ExportCtx::new(&ExportOptions::default())).unwrap();
    assert_eq!(rules(&private.report), ["rights.private_test_export"]);
}

#[test]
fn no_derivatives_is_any_identifier_with_a_whole_nd_segment() {
    let with = |licence: &str| licence_forbids_derivatives(&Provenance { licence: licence.into(), ..cc0() });
    for nd in [
        "CC-BY-ND-4.0",
        "CC-BY-NC-ND-4.0",
        "CC0-1.0 AND CC-BY-ND-4.0",
        "(MIT OR CC-BY-ND-4.0)",
        "LicenseRef-Studio-ND-1",
    ] {
        let f = with(nd).unwrap_or_else(|| panic!("{nd} forbids derivatives"));
        assert_eq!(
            (f.rule.as_str(), f.location.as_deref()),
            ("rights.licence_forbids_derivatives", Some("provenance/licence"))
        );
    }
    for ok in ["CC0-1.0", "CC-BY-4.0", "MIT", "LicenseRef-STANDARD-1", "LicenseRef-Brand-1"] {
        assert!(with(ok).is_none(), "{ok}");
    }
}

#[test]
fn strict_mode_turns_a_warning_into_a_refusal() {
    let mut a = wave();
    a.tracks[0] =
        Track::expression(ExpressionName::Preset(PresetExpression::Happy), Keys::linear(vec![0.0], vec![1.5]));
    assert!(export(&a, &ExportOptions::default()).is_ok());
    assert!(matches!(export(&a, &ExportOptions::default().strict()), Err(IoError::Rejected(_))));
}
