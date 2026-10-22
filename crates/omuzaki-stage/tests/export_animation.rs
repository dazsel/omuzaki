//! `io.export_animation` (#1173): the kit character made by `character.create`, a pose-library clip or an emote on its
//! skeleton, planned and worked as the executor runs a job, through the real VRMA exporter; the file reads back
//! through the real reader and plays the clip back on the character within the converter's tolerance.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "the test reads the file the op wrote; the fence is for ops")]

use std::sync::Arc;

use omuzaki_anim::hands::HandRest;
use omuzaki_anim::poses::ClipLibrary;
use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::ops::character::create::CharacterCreate;
use omuzaki_char_api::{BuildServices, CharId, CharacterBuilder, Quality as BuildQuality, RegistryBuilder};
use omuzaki_core::glam::{DQuat, Quat};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{ExpressionName, HumanBoneName, IoAnimation, Keys, PresetExpression, Track};
use omuzaki_io_ops::ops::list_formats::Formats;
use omuzaki_io_vrm::read_vrma;
use omuzaki_ops_api::{
    Actor, CancelFlag, ErrorCode, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, ProjectDir, Services,
    ServicesBuilder, find,
};
use omuzaki_render_api::MtoonSupport;
use omuzaki_stage::body::{Body, BodyServices, load_body};
use omuzaki_stage::ops::motion::export_animation::{
    AnimationExportResult, ExportAnimation, ND_NOTICE, PROPORTIONS_NOTICE, notices,
};
use omuzaki_store_api::{Commercial, Provenance, Source, Store};
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

struct Signal(CancelFlag);

impl JobSignal for Signal {
    fn progress(&self, _stage: &str, _fraction: f32) {}

    fn is_cancelled(&self) -> bool {
        false
    }

    fn cancel_flag(&self) -> CancelFlag {
        self.0.clone()
    }

    fn spend(&self, _usd_micros: u64) -> Result<(), OpError> {
        Ok(())
    }
}

struct Doc<'a>(&'a MemDoc);

impl JobDoc for Doc<'_> {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(self.0);
    }
}

/// A project with the kit character (`character.create`, template `standard`, seed 7).
fn kit_project() -> MemDoc {
    let (actor, clock) = (Actor::Person { name: "test".into() }, FakeClock::default());
    let mut doc = MemDoc::from_value(json!({}));
    let create: CharacterCreate =
        serde_json::from_value(json!({ "id": "kit", "name": "Kit", "template": "standard", "seed": 7 })).unwrap();
    doc.apply(&create.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap().patch).unwrap();
    doc
}

fn services() -> Services {
    ServicesBuilder::new()
        .insert::<dyn Store>(Arc::new(MemStore::new()))
        .insert::<dyn CharacterBuilder>(Arc::new(RegistryBuilder))
        .insert::<BuildServices>(Arc::new(BuildServices::default()))
        .build()
}

/// The planned result (a dry run) and the worked one with the file's bytes, or the first refusal.
fn export(doc: &MemDoc, args: Value) -> Result<(AnimationExportResult, AnimationExportResult, Vec<u8>), OpError> {
    let (actor, clock) = (Actor::Person { name: "test".into() }, FakeClock::default());
    let services = services();
    let dir = tempfile::tempdir().unwrap();
    let project = ProjectDir::open(dir.path()).unwrap();
    let grants = Grants::person();
    let op: ExportAnimation = serde_json::from_value(args).map_err(|e| OpError::invalid("args", e.to_string()))?;
    let ctx = OpCtx::new(doc, &actor, &clock).with_grants(&grants).with_project_dir(&project).with_services(&services);
    let planned = serde_json::from_value(op.plan(&ctx)?.output.unwrap()).unwrap();
    let (job_doc, signal) = (Doc(doc), Signal(CancelFlag::new()));
    let job = JobCtx::new(JobId(1), &actor, &job_doc, &signal)
        .with_grants(&grants)
        .with_project_dir(&project)
        .with_services(&services);
    let worked: AnimationExportResult = serde_json::from_value(op.work(&job)?).unwrap();
    let bytes = std::fs::read(dir.path().join(&op.path)).unwrap();
    Ok((planned, worked, bytes))
}

fn kit_skeleton(doc: &MemDoc) -> omuzaki_anim_api::Skeleton {
    let (clock, kit_build, store) = (FakeClock::default(), BuildServices::default(), MemStore::new());
    let body_services = BodyServices::new(&RegistryBuilder, &kit_build, &store, MtoonSupport::NONE, &clock);
    match load_body(doc, &body_services, &CharId::new("kit").unwrap(), None, BuildQuality::Live).unwrap() {
        Body::Kit(built) => built.rig.skeleton,
        Body::Avatar(avatar) => avatar.skeleton,
    }
}

/// The JSON chunk of a GLB (the first chunk, after the 12-byte header).
fn glb_json(bytes: &[u8]) -> Value {
    let len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    assert_eq!(&bytes[16..20], b"JSON");
    serde_json::from_slice(&bytes[20..20 + len]).unwrap()
}

fn rotation(a: &IoAnimation, bone: &str) -> Option<Keys<Quat>> {
    let bone = HumanBoneName::new(bone).unwrap();
    a.tracks.iter().find_map(|t| match t {
        Track::Rotation { bone: b, keys, .. } if *b == bone => Some(keys.clone()),
        _ => None,
    })
}

fn dquat(q: Quat) -> DQuat {
    DQuat::from_array(q.to_array().map(f64::from))
}

/// Linear playback of `keys` at `t` in `f64`, slerped like a player.
fn play(keys: &Keys<Quat>, t: f32) -> DQuat {
    let i = keys.times.partition_point(|k| *k <= t);
    if i == 0 {
        return dquat(keys.values[0]);
    }
    if i >= keys.times.len() {
        return dquat(*keys.values.last().unwrap());
    }
    let u = f64::from(t - keys.times[i - 1]) / f64::from(keys.times[i] - keys.times[i - 1]);
    let (from, mut to) = (dquat(keys.values[i - 1]), dquat(keys.values[i]));
    if from.dot(to) < 0.0 {
        to = -to;
    }
    let theta = from.dot(to).clamp(-1.0, 1.0).acos();
    if theta < 1e-12 {
        return from;
    }
    (from * (((1.0 - u) * theta).sin() / theta.sin()) + to * ((u * theta).sin() / theta.sin())).normalize()
}

/// The angle between two rotations, precise for small angles.
fn angle(a: DQuat, b: DQuat) -> f64 {
    let d = a.conjugate() * b;
    2.0 * (d.x * d.x + d.y * d.y + d.z * d.z).sqrt().atan2(d.w.abs())
}

#[test]
fn a_library_clip_exports_on_the_kit_skeleton_and_plays_back_on_it() {
    let doc = kit_project();
    let (planned, worked, bytes) =
        export(&doc, json!({ "character": "kit", "clip": "wave", "path": "exports/wave.vrma" })).unwrap();
    // The dry run says what the job writes, without the file.
    assert!(planned.file.is_none() && worked.file.is_some());
    assert_eq!(planned.summary, worked.summary);
    assert_eq!(planned.notices, worked.notices);
    let file = worked.file.as_ref().unwrap();
    assert_eq!((file.media_type.as_str(), file.bytes), ("model/gltf-binary", bytes.len() as u64));
    assert_eq!(worked.notices[0], PROPORTIONS_NOTICE, "the proportions line comes first");
    assert_eq!(worked.notices[1], "Licence: LicenseRef-Omuzaki-User");
    assert!(!worked.report.has_errors(), "{:?}", worked.report);
    println!("wave.vrma: {} bytes, {:?}", bytes.len(), worked.summary);
    assert!(planned.summary.is_some(), "the dry run converts");

    // The file is a VRM Animation the reader takes, with the character's rest and the clip's motion.
    let json = glb_json(&bytes);
    assert_eq!(json["extensionsUsed"], json!(["VRMC_vrm_animation"]));
    assert!(json.get("extensionsRequired").is_none());
    let a = read_vrma(&bytes).unwrap();
    assert!(a.looped, "the wave loops");
    assert_eq!(a.duration(), 0.6);
    let skeleton = kit_skeleton(&doc);
    let rest = omuzaki_io_ops::convert::animation::rest_humanoid(&skeleton).unwrap();
    assert!((a.rest.hips_height().unwrap() - rest.hips_height().unwrap()).abs() < 1e-6, "the kit's own stature");
    let clip = ClipLibrary::builtin().unwrap().get("wave").unwrap();
    let hands = HandRest::of(&skeleton).unwrap_or_else(|_| HandRest::nominal());
    let forearm = rotation(&a, "rightLowerArm").expect("the wave swings the right forearm");
    assert!(forearm.len() > 2, "a swing is more than a hold");
    for k in 0..=18 {
        #[allow(clippy::cast_precision_loss)]
        let t = k as f32 / 30.0;
        let exact = clip.sample(t, &hands).get(HumanoidBone::RightLowerArm);
        let error = angle(play(&forearm, t), dquat(exact));
        assert!(error < 1e-5, "{t}: {error} rad");
    }
    // Only what moves gets a track; the legs stay with the player's idle.
    assert!(rotation(&a, "leftUpperLeg").is_none());
    assert_eq!(Some(a.summary()), worked.summary);
}

#[test]
fn every_bone_mirroring_and_the_name_are_the_callers() {
    let doc = kit_project();
    let args = json!({
        "character": "kit", "clip": "wave", "mirror": true, "all_bones": true, "name": "wave left", "fps": 60,
        "path": "exports/wave-left.vrma"
    });
    let (_, worked, bytes) = export(&doc, args).unwrap();
    let a = read_vrma(&bytes).unwrap();
    assert_eq!(a.name, "wave left");
    assert_eq!(worked.summary.unwrap().bone_tracks, a.rest.bones.len(), "every rest bone");
    let left = rotation(&a, "leftLowerArm").unwrap();
    let right = rotation(&a, "rightLowerArm").unwrap();
    // Mirrored, the left forearm swings and the right one is still.
    let spread = |k: &Keys<Quat>| k.values.iter().map(|q| angle(dquat(k.values[0]), dquat(*q))).fold(0.0, f64::max);
    assert!(spread(&left) > 0.2 && spread(&right) < 0.05, "{} {}", spread(&left), spread(&right));
}

#[test]
fn an_emote_eases_its_expression_and_pose_in_and_out() {
    let doc = kit_project();
    let args = json!({
        "character": "kit",
        "emote": { "expression": "happy", "pose": { "id": "wave-right" }, "hold_s": 1.0 },
        "path": "exports/hi.vrma"
    });
    let (_, worked, bytes) = export(&doc, args).unwrap();
    let a = read_vrma(&bytes).unwrap();
    assert!(!a.looped);
    assert!((a.duration() - 1.4).abs() < 1e-6, "0.2 + 1.0 + 0.2: {}", a.duration());
    let weights = a
        .tracks
        .iter()
        .find_map(|t| match t {
            Track::Expression { name, keys, .. } => Some((name.clone(), keys.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(weights.0, ExpressionName::Preset(PresetExpression::Happy));
    assert_eq!((weights.1.values[0], *weights.1.values.last().unwrap()), (0.0, 0.0));
    assert!(weights.1.values.iter().any(|w| (*w - 1.0).abs() < 1e-6), "held at full weight");
    let arm = rotation(&a, "rightUpperArm").expect("the pose turns the arm");
    assert_eq!(arm.values[0], Quat::IDENTITY, "from rest");
    assert_eq!(worked.summary.unwrap().expression_tracks, 1);
    assert_eq!(worked.report.findings.iter().filter(|f| f.rule.starts_with("anim_export.expression")).count(), 0);
}

#[test]
fn a_custom_expression_is_named_for_the_model_and_reported() {
    let doc = kit_project();
    let args = json!({ "character": "kit", "emote": { "expression": "smug" }, "path": "exports/smug.vrma" });
    let (planned, _, bytes) = export(&doc, args).unwrap();
    assert!(planned.report.has_rule("anim_export.expression_custom"), "{:?}", planned.report);
    let a = read_vrma(&bytes).unwrap();
    assert_eq!(a.summary().bone_tracks, 0, "no pose, no bone tracks");
    assert!(a.tracks.iter().any(|t| matches!(t, Track::Expression { name, .. } if name.as_str() == "omuzaki_smug")));
}

#[test]
fn bad_arguments_are_refused_before_anything_is_loaded() {
    let doc = kit_project();
    let refused = |args: Value| export(&doc, args).map(|_| ()).unwrap_err();
    let cases = [
        (json!({ "character": "kit", "path": "a.vrma" }), "clip"),
        (json!({ "character": "kit", "clip": "wave", "emote": { "expression": "happy" }, "path": "a.vrma" }), "emote"),
        (json!({ "character": "kit", "clip": "wave", "path": "a.vrm" }), "path"),
        (json!({ "character": "kit", "clip": "wave", "fps": 0, "path": "a.vrma" }), "fps"),
        (json!({ "character": "kit", "clip": "wave", "format": "bvh", "path": "a.vrma" }), "format"),
        (json!({ "character": "kit", "emote": { "expression": "happy" }, "mirror": true, "path": "a.vrma" }), "mirror"),
        (json!({ "character": "kit", "emote": { "expression": "happy", "fade_s": 0 }, "path": "a.vrma" }), "emote"),
        (json!({ "character": "kit", "emote": { "expression": "happy", "intensity": 2 }, "path": "a.vrma" }), "emote"),
        (
            json!({ "character": "kit", "emote": { "expression": "happy", "pose": { "id": "nope" } }, "path": "a.vrma" }),
            "emote/pose/id",
        ),
    ];
    for (args, field) in cases {
        let error = refused(args.clone());
        assert_eq!(error.field.as_deref(), Some(field), "{args}: {error:?}");
    }
    let error = refused(json!({ "character": "kit", "clip": "wavee", "path": "a.vrma" }));
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.hint.as_deref().unwrap().contains("did you mean 'wave'"), "{error:?}");
}

#[test]
fn the_notices_carry_the_licence_the_attribution_and_the_nd_line() {
    let record = Provenance {
        source: Source::Authored,
        author: Some("Kit".into()),
        licence: "CC-BY-ND-4.0".into(),
        attribution: Some("Motion by Kit".into()),
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    };
    assert_eq!(
        notices(Some(&record)),
        [PROPORTIONS_NOTICE, "Licence: CC-BY-ND-4.0", "Attribution: Motion by Kit", ND_NOTICE]
    );
    assert_eq!(notices(None), [PROPORTIONS_NOTICE]);
}

#[test]
fn the_op_and_the_format_are_registered() {
    let meta = (find("io.export_animation").expect("registered").meta)();
    assert_eq!(meta.name, "io.export_animation");
    let formats = Formats::registered();
    assert!(formats.animations.iter().any(|f| f.id == "vrma" && f.extensions == ["vrma"]), "{:?}", formats.animations);
}

#[test]
fn a_host_without_a_builder_still_dry_runs_the_arguments_and_the_notices() {
    let doc = kit_project();
    let (actor, clock) = (Actor::Person { name: "test".into() }, FakeClock::default());
    let dir = tempfile::tempdir().unwrap();
    let project = ProjectDir::open(dir.path()).unwrap();
    let grants = Grants::person();
    let ctx = OpCtx::new(&doc, &actor, &clock).with_grants(&grants).with_project_dir(&project);
    let op: ExportAnimation =
        serde_json::from_value(json!({ "character": "kit", "clip": "walk", "path": "walk.vrma" })).unwrap();
    let planned: AnimationExportResult = serde_json::from_value(op.plan(&ctx).unwrap().output.unwrap()).unwrap();
    assert_eq!((planned.summary, planned.file), (None, None));
    assert_eq!(planned.notices, [PROPORTIONS_NOTICE, "Licence: LicenseRef-Omuzaki-User"]);
    let missing: ExportAnimation =
        serde_json::from_value(json!({ "character": "nobody", "clip": "walk", "path": "walk.vrma" })).unwrap();
    assert_eq!(missing.plan(&ctx).unwrap_err().code, ErrorCode::NotFound);
}
