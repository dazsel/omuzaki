//! The fixtures of the three-vrm conformance check (#1277, `tests/interop/vrma/`): `cargo run -p omuzaki-stage --example
//! vrma_interop -- DIR`.
//!
//! It writes into `DIR`, from the kit's own character (`character.create`, template `standard`, seed 7; nothing
//! third-party):
//! - `kit.vrm`, the character exported to VRM 1.0, and `kit-short.vrm`, the same exported at 80% of its stature
//!   (its hips stand about a quarter lower);
//! - `wave.vrma`, `walk.vrma` (every bone) and `hi.vrma` (the `happy` emote over the `wave-right` pose), each written by
//!   `io.export_animation` as a person's export is;
//! - `look.vrma`, a gaze of yaw +0.3 rad and pitch +0.2 rad, and `held.vrma`, a `happy` weight and a forearm rotation
//!   held (`Step`) from 0 to 1 at 0.5 s, both written by the VRMA exporter from an animation made here;
//! - `expected.json`: for each file, the times to probe and what Omuzaki's own reading gives at each one (every rest
//!   bone's rotation, the hips position in the source's metres, each expression's weight, the gaze's quaternion, yaw
//!   and pitch), and each model's rest hips height as Omuzaki's importer reads it.
//!
//! `check.mjs` plays the files in three-vrm and compares. The clips' files are checked here first against the clips
//! themselves (within 1e-5 rad), so the chain is clip, file, player.

// A fixture writer's sizes are small counts in float maths; the casts are exact enough.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#![allow(clippy::disallowed_methods, reason = "a fixture writer writes its files; the fence is for ops")]

#[path = "../tests/avatar/kit.rs"]
mod kit;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use omuzaki_anim::hands::HandRest;
use omuzaki_anim::poses::ClipLibrary;
use omuzaki_anim_api::{HumanoidBone, Skeleton};
use omuzaki_char::ops::character::create::CharacterCreate;
use omuzaki_char_api::{
    BuildServices, BuiltCharacter, CharId, CharacterBuilder, Quality as BuildQuality, RegistryBuilder,
};
use omuzaki_core::glam::{DQuat, EulerRot, Quat, Vec3};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{
    ExportCtx, ExportOptions, ExpressionName, ImportCtx, ImportInput, ImportOptions, Interpolation, IoAnimation, Keys,
    PerfectSync, PresetExpression, Track, run_export, run_export_animation, run_import,
};
use omuzaki_io_ops::convert::animation::rest_humanoid;
use omuzaki_io_ops::gather::avatar::add_avatar;
use omuzaki_io_ops::gather::{GatherOptions, GatherPurpose, gather};
use omuzaki_io_vrm::{Vrm1Exporter, Vrm1Importer, VrmaExporter, read_vrma};
use omuzaki_ops_api::{
    Actor, CancelFlag, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, ProjectDir, ServicesBuilder,
};
use omuzaki_render_api::MtoonSupport;
use omuzaki_stage::body::{Body, BodyServices, load_body};
use omuzaki_stage::ops::motion::export_animation::ExportAnimation;
use omuzaki_store_api::{Commercial, Provenance, Source, Store};
use omuzaki_store_mock::MemStore;
use serde_json::{Map, Value, json};

/// How far a file may stray from the clip it was made from, radians (the converter's tolerance with room for `f32`).
const CLIP_TOLERANCE: f64 = 1e-5;

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

/// `built` exported to VRM 1.0 at `target_height` metres (life-size without one), with the rights a person sets.
fn vrm(built: &BuiltCharacter, target_height: Option<f32>) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut gathered = gather(built, &GatherOptions { target_height, purpose: GatherPurpose::Vrm })?;
    add_avatar(&mut gathered, built)?;
    let meta = &mut gathered.scene.meta;
    meta.name = "Kit".into();
    meta.authors = vec!["Omuzaki".into()];
    meta.license_url = "https://vrm.dev/licenses/1.0/".into();
    gathered.scene.provenance = Some(kit::provenance());
    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Off);
    let outcome = run_export(&Vrm1Exporter, &gathered.scene, &ExportCtx::new(&options))?;
    Ok(outcome.files.into_iter().next().ok_or("no .vrm file")?.bytes)
}

/// The rest hips height of a VRM as Omuzaki's importer and avatar preparation read it.
fn hips_height(bytes: &[u8]) -> Result<f32, Box<dyn Error>> {
    let scene = run_import(&Vrm1Importer, &ImportInput::new(bytes), &ImportCtx::new(&ImportOptions::default()))?.scene;
    let avatar = kit::avatar_of(&scene);
    rest_humanoid(&avatar.skeleton)?.hips_height().ok_or_else(|| "no hips".into())
}

/// Runs `io.export_animation` with `args` (the path relative to `dir`) as the executor runs a job.
fn export(doc: &MemDoc, dir: &Path, args: Value) -> Result<(), Box<dyn Error>> {
    let actor = Actor::Person { name: "fixtures".into() };
    let services = ServicesBuilder::new()
        .insert::<dyn Store>(Arc::new(MemStore::new()))
        .insert::<dyn CharacterBuilder>(Arc::new(RegistryBuilder))
        .insert::<BuildServices>(Arc::new(BuildServices::default()))
        .build();
    let project = ProjectDir::open(dir)?;
    let grants = Grants::person();
    let op: ExportAnimation = serde_json::from_value(args)?;
    let (job_doc, signal) = (Doc(doc), Signal(CancelFlag::new()));
    let job = JobCtx::new(JobId(1), &actor, &job_doc, &signal)
        .with_grants(&grants)
        .with_project_dir(&project)
        .with_services(&services);
    op.work(&job).map_err(|e| format!("{}: {e:?}", op.path))?;
    Ok(())
}

fn dquat(q: Quat) -> DQuat {
    DQuat::from_array(q.to_array().map(f64::from))
}

/// The angle between two rotations, precise for small angles.
fn angle(a: DQuat, b: DQuat) -> f64 {
    let d = a.conjugate() * b;
    2.0 * (d.x * d.x + d.y * d.y + d.z * d.z).sqrt().atan2(d.w.abs())
}

/// Where `t` falls in `keys`: the key before (or at) it, the key after and how far between, `None` past the ends.
fn segment<T>(keys: &Keys<T>, t: f32) -> (usize, Option<(usize, f64)>) {
    let i = keys.times.partition_point(|k| *k <= t);
    if i == 0 {
        return (0, None);
    }
    if i >= keys.times.len() || keys.interpolation == Interpolation::Step {
        return (i - 1, None);
    }
    let u = f64::from(t - keys.times[i - 1]) / f64::from(keys.times[i] - keys.times[i - 1]);
    (i - 1, Some((i, u)))
}

/// `keys` at `t` as a player reads them: shortest-arc slerp in `f64`, or the held key for `Step`.
fn quat_at(keys: &Keys<Quat>, t: f32) -> DQuat {
    let (i, next) = segment(keys, t);
    let from = dquat(keys.values[i]);
    let Some((j, u)) = next else { return from };
    let mut to = dquat(keys.values[j]);
    if from.dot(to) < 0.0 {
        to = -to;
    }
    let theta = from.dot(to).clamp(-1.0, 1.0).acos();
    if theta < 1e-12 {
        return from;
    }
    (from * (((1.0 - u) * theta).sin() / theta.sin()) + to * ((u * theta).sin() / theta.sin())).normalize()
}

fn vec_at(keys: &Keys<Vec3>, t: f32) -> [f64; 3] {
    let (i, next) = segment(keys, t);
    let from = keys.values[i].to_array().map(f64::from);
    let Some((j, u)) = next else { return from };
    let to = keys.values[j].to_array().map(f64::from);
    [0, 1, 2].map(|c| from[c] + (to[c] - from[c]) * u)
}

fn weight_at(keys: &Keys<f32>, t: f32) -> f64 {
    let (i, next) = segment(keys, t);
    let from = f64::from(keys.values[i]);
    next.map_or(from, |(j, u)| from + (f64::from(keys.values[j]) - from) * u)
}

/// What Omuzaki reads from `a` at each of `times`, as `expected.json` lists it for `file`.
fn expected(file: &str, a: &IoAnimation, times: &[f32]) -> Value {
    let mut bones = Map::new();
    for rest in &a.rest.bones {
        let track = a.tracks.iter().find_map(|t| match t {
            Track::Rotation { bone, keys, .. } if *bone == rest.bone => Some(keys),
            _ => None,
        });
        let values: Vec<[f64; 4]> =
            times.iter().map(|t| track.map_or(DQuat::IDENTITY, |k| quat_at(k, *t)).to_array()).collect();
        bones.insert(rest.bone.as_str().to_owned(), json!(values));
    }
    let hips = a.tracks.iter().find_map(|t| match t {
        Track::Hips(keys) => Some(times.iter().map(|t| vec_at(keys, *t)).collect::<Vec<_>>()),
        _ => None,
    });
    let mut expressions = Map::new();
    for track in &a.tracks {
        if let Track::Expression { name, keys, .. } = track {
            let values: Vec<f64> = times.iter().map(|t| weight_at(keys, *t)).collect();
            expressions.insert(name.as_str().to_owned(), json!(values));
        }
    }
    let look = a.tracks.iter().find_map(|t| match t {
        Track::LookAt(keys) => Some(
            times
                .iter()
                .map(|t| {
                    let q = quat_at(keys, *t);
                    let (yaw, pitch, _) = q.to_euler(EulerRot::YXZ);
                    json!({ "quaternion": q.to_array(), "yaw": yaw, "pitch": pitch })
                })
                .collect::<Vec<_>>(),
        ),
        _ => None,
    });
    json!({
        "file": file,
        "duration": a.duration(),
        "looped": a.looped,
        "rest_hips_y": a.rest.hips_height(),
        "times": times,
        "bones": bones,
        "hips": hips,
        "expressions": expressions,
        "look_at": look,
    })
}

/// Ten probes inside `duration`, clear of both ends.
fn probes(duration: f32) -> Vec<f32> {
    (0..10).map(|k| duration * (k as f32 + 0.5) / 10.0).collect()
}

/// Checks the file made from library clip `id` against the clip at every sample, on `skeleton`'s hands.
fn check_clip(a: &IoAnimation, id: &str, mirror: bool, skeleton: &Skeleton) -> Result<f64, Box<dyn Error>> {
    let library = ClipLibrary::builtin()?;
    let clip = library.get(id).ok_or("no such clip")?;
    let clip = if mirror { clip.mirrored() } else { clip.clone() };
    let hands = HandRest::of(skeleton).unwrap_or_else(|_| HandRest::nominal());
    let mut worst: f64 = 0.0;
    // At the samples (30 a second): between them the file is the slerp of its keys, which the sampling error reports.
    let frames = (f64::from(a.duration()) * 30.0).round() as u32;
    for k in 0..=frames {
        let t = k as f32 / 30.0;
        let pose = clip.sample(t, &hands);
        for track in &a.tracks {
            if let Track::Rotation { bone, keys, .. } = track {
                let humanoid = HumanoidBone::ALL
                    .into_iter()
                    .find(|b| b.name() == bone.as_str())
                    .ok_or_else(|| format!("{bone} is no humanoid bone"))?;
                worst = worst.max(angle(quat_at(keys, t), dquat(pose.get(humanoid))));
            }
        }
    }
    if worst > CLIP_TOLERANCE {
        return Err(format!("{id}: the file strays {worst} rad from the clip").into());
    }
    Ok(worst)
}

fn library_record() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: Some("Omuzaki".into()),
        licence: "LicenseRef-Omuzaki-User".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

/// Writes `a` with the VRMA exporter to `dir/file`.
fn write_vrma(dir: &Path, file: &str, a: &IoAnimation) -> Result<(), Box<dyn Error>> {
    let options = ExportOptions::default();
    let outcome = run_export_animation(&VrmaExporter, a, &ExportCtx::new(&options))?;
    std::fs::write(dir.join(file), &outcome.files.first().ok_or("no .vrma file")?.bytes)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let dir = PathBuf::from(std::env::args().nth(1).ok_or("usage: vrma_interop DIR")?);
    std::fs::create_dir_all(&dir)?;
    let (actor, clock) = (Actor::Person { name: "fixtures".into() }, FakeClock::default());
    let mut doc = MemDoc::from_value(json!({}));
    let create: CharacterCreate =
        serde_json::from_value(json!({ "id": "kit", "name": "Kit", "template": "standard", "seed": 7 }))?;
    doc.apply(&create.plan(&OpCtx::new(&doc, &actor, &clock))?.patch)?;

    let (store, kit_build) = (MemStore::new(), BuildServices::default());
    let body_services = BodyServices::new(&RegistryBuilder, &kit_build, &store, MtoonSupport::NONE, &clock);
    let body = load_body(&doc, &body_services, &CharId::new("kit")?, None, BuildQuality::Live)?;
    let height = body.landmarks()?.height_m;
    let Body::Kit(built) = body else { return Err("the kit character loaded as an import".into()) };
    let skeleton = built.rig.skeleton.clone();

    let mut models = Map::new();
    for (file, target) in [("kit.vrm", None), ("kit-short.vrm", Some(0.8 * height))] {
        let bytes = vrm(&built, target)?;
        let hips = hips_height(&bytes)?;
        std::fs::write(dir.join(file), &bytes)?;
        println!("{file}: {} bytes, rest hips {hips:.4} m", bytes.len());
        models.insert(file.to_owned(), json!({ "hips_height": hips }));
    }

    let mut animations = Vec::new();
    let made = [
        ("wave.vrma", json!({ "character": "kit", "clip": "wave", "path": "wave.vrma" }), Some(("wave", false))),
        (
            "walk.vrma",
            json!({ "character": "kit", "clip": "walk", "all_bones": true, "path": "walk.vrma" }),
            Some(("walk", false)),
        ),
        (
            "hi.vrma",
            json!({ "character": "kit", "emote": { "expression": "happy", "pose": { "id": "wave-right" } }, "path": "hi.vrma" }),
            None,
        ),
    ];
    for (file, args, clip) in made {
        export(&doc, &dir, args)?;
        let a = read_vrma(&std::fs::read(dir.join(file))?)?;
        if let Some((id, mirror)) = clip {
            let worst = check_clip(&a, id, mirror, &skeleton)?;
            println!("{file}: {} tracks, at most {worst:.2e} rad from the clip", a.tracks.len());
        }
        animations.push(expected(file, &a, &probes(a.duration())));
    }

    let rest = rest_humanoid(&skeleton)?;
    let forearm = omuzaki_io_api::HumanBoneName::new("rightLowerArm")?;
    let mut look = IoAnimation::new("look", rest.clone());
    let gaze = Quat::from_euler(EulerRot::YXZ, 0.3, 0.2, 0.0);
    look.tracks.push(Track::look_at(Keys::linear(vec![0.0, 1.0], vec![gaze, gaze])));
    look.provenance = Some(library_record());
    write_vrma(&dir, "look.vrma", &look)?;
    animations.push(expected("look.vrma", &look, &[0.25, 0.5, 0.75]));

    let mut held = IoAnimation::new("held", rest);
    let happy = ExpressionName::Preset(PresetExpression::Happy);
    held.tracks.push(Track::expression(happy, Keys::step(vec![0.0, 0.5, 1.0], vec![0.0, 1.0, 1.0])));
    let bent = Quat::from_rotation_z(-0.5);
    held.tracks.push(Track::rotation(forearm, Keys::step(vec![0.0, 0.5, 1.0], vec![Quat::IDENTITY, bent, bent])));
    held.provenance = Some(library_record());
    write_vrma(&dir, "held.vrma", &held)?;
    animations.push(expected("held.vrma", &held, &[0.25, 0.4999, 0.5002, 0.75]));

    let manifest = json!({ "models": models, "animations": animations });
    std::fs::write(dir.join("expected.json"), serde_json::to_vec_pretty(&manifest)?)?;
    println!("wrote {} animations and 2 models to {}", animations.len(), dir.display());
    Ok(())
}
