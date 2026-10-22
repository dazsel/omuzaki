//! The import timing test (#1368; `docs/design/byom.md` D6, "Time budget"): a generated VRM 1.0 goes from file drop
//! to performing (`io.inspect`, then `io.import … as: avatar`, then the first stage frame on the mock renderer) and
//! every stage of D6's machine-time table is timed. The full file is 40 MB (twenty 2048-pixel textures of seeded
//! noise, a 20 000-vertex face with the 52 `ARKit` shapes, 150 bones) and runs in the nightly with `--release --
//! --ignored`; a small one (four 256-pixel textures) runs always. The file is the kit's own mock character with the
//! rest generated here; no third-party file.
//!
//! Class (spec §10): every stage is printed as `reported`. Only with `OMUZAKI_BUDGET_TIER=Dev` set does an overrun
//! fail, and then a stage's time is compared with its budget directly.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "the test writes its generated file to a temporary project folder")]
#![allow(clippy::cast_precision_loss, reason = "grid coordinates and file sizes, far below f32's and f64's range")]

#[path = "avatar/kit.rs"]
mod kit;

use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};
use omuzaki_avatar_api::summary::{AvatarSummary, Stage};
use omuzaki_char_api::CharId;
use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_core::time::{Clock, SystemClock};
use omuzaki_doc_api::{DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{
    ARKIT_52, AvatarPermission, Blob, CommercialUsage, EncodedImage, ExportCtx, ExportOptions, HumanBoneName,
    ImageMime, IoScene, Material, Mesh, MorphTarget, Node, NodeIx, PerfectSync, Primitive, Sampler, Skin, TexRef,
    Texture, run_export,
};
use omuzaki_io_ops::ops::avatar::load_avatar;
use omuzaki_io_ops::ops::import::Import;
use omuzaki_ops_api::{
    Actor, CancelFlag, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, ProjectDir, Services,
    ServicesBuilder, find,
};
use omuzaki_render_api::{AlphaEncoding, Camera, MtoonSupport, Projection, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::avatar::AvatarStage;
use omuzaki_stage::frame::{FrameInput, StageClock};
use omuzaki_store_api::Store;
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

/// D6's machine-time budget per stage, for a machine of tier `R`.
const BUDGETS: [(Stage, Duration); 5] = [
    (Stage::Read, Duration::from_millis(1500)),
    (Stage::Prepare, Duration::from_millis(1000)),
    (Stage::Decode, Duration::from_millis(3000)),
    (Stage::Store, Duration::from_millis(1000)),
    (Stage::Upload, Duration::from_millis(1500)),
];
/// D6's budget for the whole machine side.
const MACHINE_TOTAL: Duration = Duration::from_secs(8);
/// How far past its budget a stage may run before it counts as an overrun: 25 %.
const SLACK_PERCENT: u32 = 25;

/// A stage, or the machine total (`stage: None`), that ran more than [`SLACK_PERCENT`] over its budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StageOverrun {
    stage: Option<Stage>,
    took: Duration,
    budget: Duration,
}

/// The stages in `timings` that ran more than 25 % over their budget, then the machine total as a row of its own when
/// it is over its 8 s. A stage without a budget is not judged.
fn stage_overruns(timings: &[(Stage, Duration)]) -> Vec<StageOverrun> {
    let over = |took: Duration, budget: Duration| took * 100 > budget * (100 + SLACK_PERCENT);
    let mut overruns: Vec<StageOverrun> = timings
        .iter()
        .filter_map(|&(stage, took)| {
            let budget = BUDGETS.iter().find(|(s, _)| *s == stage)?.1;
            over(took, budget).then_some(StageOverrun { stage: Some(stage), took, budget })
        })
        .collect();
    let total: Duration = timings.iter().map(|(_, took)| *took).sum();
    if total > MACHINE_TOTAL {
        overruns.push(StageOverrun { stage: None, took: total, budget: MACHINE_TOTAL });
    }
    overruns
}

/// What the generated file holds.
#[derive(Clone, Copy)]
struct Shape {
    /// Textures, each `size` pixels square.
    textures: usize,
    size: u32,
    /// Vertices of the face mesh, which carries the 52 `ARKit` shapes.
    face_vertices: usize,
    /// Bones of the whole skeleton.
    bones: usize,
    /// The noise in each texture, in levels per channel: what sets how well the PNGs compress, so the file's size.
    noise_levels: u8,
}

/// The D6 file: 40 MB.
const FULL: Shape = Shape { textures: 20, size: 2048, face_vertices: 20_000, bones: 150, noise_levels: 9 };
/// The small file that runs in every test run.
const SMALL: Shape = Shape { textures: 4, size: 256, face_vertices: 2_000, bones: 150, noise_levels: 6 };

/// A seeded generator (`SplitMix64`): the same file every run.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// A `size`-pixel square grey PNG of seeded noise, `levels` levels round a mid grey.
fn noise_png(size: u32, levels: u8, seed: u64) -> Vec<u8> {
    let mut noise = Noise(seed);
    let count = usize::try_from(size).unwrap().pow(2);
    let pixels: Vec<u8> =
        (0..count).map(|_| 100 + u8::try_from(noise.next() % u64::from(levels)).unwrap() * 8).collect();
    let mut png = Vec::new();
    PngEncoder::new_with_quality(Cursor::new(&mut png), CompressionType::Default, FilterType::NoFilter)
        .write_image(&pixels, size, size, ExtendedColorType::L8)
        .unwrap();
    png
}

/// World matrices of every node of `scene`.
fn worlds(scene: &IoScene) -> Vec<Mat4> {
    let mut world = vec![Mat4::IDENTITY; scene.nodes.len()];
    let mut stack: Vec<(NodeIx, Mat4)> = scene.roots.iter().map(|&r| (r, Mat4::IDENTITY)).collect();
    while let Some((node, parent)) = stack.pop() {
        let here = parent * {
            let t = scene.nodes[node.index()].transform;
            Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation)
        };
        world[node.index()] = here;
        stack.extend(scene.nodes[node.index()].children.iter().map(|&c| (c, here)));
    }
    world
}

/// The kit's mock character as a VRM 1.0 with `shape`'s textures, face and bones added, written as a person's file.
fn generated(shape: Shape) -> Vec<u8> {
    let mut scene = kit::as_vrm(&kit::kit());
    let head = scene.avatar.as_ref().unwrap().humanoid.bones[&HumanBoneName::new("head").unwrap()];

    // Bones: a chain of ribbons on the head, joints of the face's skin so they are bones, up to the skeleton's count.
    let bones = kit::avatar_of(&scene).skeleton.len();
    let mut face_joints = vec![head];
    for ribbon in 0..shape.bones.saturating_sub(bones) {
        let node = scene.add_node(Node::new(format!("ribbon_{ribbon}")).at(Vec3::new(0.0, -0.01, -0.01)));
        scene.attach(*face_joints.last().unwrap(), node);
        face_joints.push(node);
    }
    let worlds = worlds(&scene);

    // The textures, each on a material of its own.
    let materials: Vec<_> = (0..shape.textures)
        .map(|t| {
            let png = noise_png(shape.size, shape.noise_levels, t as u64 + 1);
            let image = scene.add_image(EncodedImage::new(format!("noise_{t}"), ImageMime::Png, Blob::new(png)));
            let texture = scene.add_texture(Texture { image, sampler: Sampler::default() });
            let mut material = Material::new(format!("Face_{t}"), omuzaki_io_api::Shading::Unlit);
            material.base_color_texture = Some(TexRef::of(texture));
            scene.add_material(material)
        })
        .collect();

    // The face: a grid in front of the head, one strip per material, skinned to the head, with the 52 shapes.
    let world = worlds[head.index()];
    let centre = world.w_axis.truncate() + Vec3::new(0.0, 0.08, 0.09);
    let columns = 100;
    let rows = shape.face_vertices / columns;
    let strips = materials.len().max(1);
    let mut mesh = Mesh::new("Face");
    for strip in 0..strips {
        let (first_row, last_row) = (rows * strip / strips, rows * (strip + 1) / strips);
        let mut data = MeshData::default();
        for row in first_row..=last_row.min(rows - 1) {
            for column in 0..columns {
                let uv = Vec2::new(column as f32 / (columns - 1) as f32, row as f32 / (rows - 1) as f32);
                data.positions.push(centre + Vec3::new((uv.x - 0.5) * 0.14, (0.5 - uv.y) * 0.16, 0.0));
                data.normals.push(Vec3::Z);
                data.uvs.push(uv);
                // Every vertex on one joint of the face's skin, round the ribbons, so each is a bone in use.
                let joint = u16::try_from((row * columns + column) % face_joints.len()).unwrap();
                data.joints.push([joint, 0, 0, 0]);
                data.weights.push([1.0, 0.0, 0.0, 0.0]);
            }
        }
        let strip_rows = data.positions.len() / columns;
        for row in 0..strip_rows.saturating_sub(1) {
            for column in 0..columns - 1 {
                let at = u32::try_from(row * columns + column).unwrap();
                let (right, below) = (at + 1, at + u32::try_from(columns).unwrap());
                data.indices.extend([at, below, right, right, below, below + 1]);
            }
        }
        let count = data.positions.len();
        let mut primitive = Primitive::new(data);
        primitive.material = materials.get(strip).copied();
        // Each shape moves its own band of the face, 4 mm forward, as a blendshape moves its region.
        primitive.morph_targets = (0..ARKIT_52.len())
            .map(|s| {
                MorphTarget::new(
                    (0..count)
                        .map(|v| if (v * 52 / count.max(1)) == s { Vec3::Z * 0.004 } else { Vec3::ZERO })
                        .collect(),
                )
            })
            .collect();
        mesh.primitives.push(primitive);
    }
    mesh.morph_target_names = ARKIT_52.iter().map(|n| (*n).to_owned()).collect();
    let mesh = scene.add_mesh(mesh);
    let inverse_bind = face_joints.iter().map(|j| worlds[j.index()].inverse()).collect();
    let skin = scene.add_skin(Skin::new("Face_skin", face_joints, inverse_bind));
    let mut node = Node::new("Face");
    node.mesh = Some(mesh);
    node.skin = Some(skin);
    let node = scene.add_node(node);
    scene.roots.push(node);

    // A model anyone may perform as, so the import needs no acknowledgement.
    scene.meta.terms.avatar_permission = AvatarPermission::Everyone;
    scene.meta.terms.commercial_usage = CommercialUsage::PersonalProfit;
    scene.provenance = Some(kit::provenance());
    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Off);
    let outcome = run_export(&omuzaki_io_vrm::Vrm1Exporter, &scene, &ExportCtx::new(&options)).expect("writes");
    outcome.files.into_iter().next().expect("the .vrm file").bytes
}

/// What a running job is told: never cancelled.
struct Signal(CancelFlag);

impl JobSignal for Signal {
    fn progress(&self, _stage: &str, _fraction: f32) {}

    fn is_cancelled(&self) -> bool {
        omuzaki_ops_api::Cancel::is_cancelled(&self.0)
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

/// What dropping a file and performing as it took.
struct Dropped {
    /// The import's summary, with the first frame's upload added.
    summary: AvatarSummary,
    /// The stages `io.inspect` timed before the person confirmed.
    inspected: Vec<(Stage, Duration)>,
    /// Rebuilding the avatar from the stored original, which the stage does today before it uploads.
    reload: Duration,
    /// Bones of the staged avatar.
    bones: usize,
    /// Joint matrices in the first frame's scene.
    palette: usize,
}

/// `bytes` dropped on a new project as `Avatar.vrm`: inspected, imported as an avatar, and staged on the mock renderer
/// up to its first performing frame, with a real clock.
fn drop_to_perform(bytes: &[u8]) -> Dropped {
    let clock = Arc::new(SystemClock::new());
    let store = Arc::new(MemStore::new());
    let services: Services =
        ServicesBuilder::new().insert::<dyn Store>(store.clone()).insert::<dyn Clock>(clock.clone()).build();
    let mut doc = MemDoc::from_value(json!({ "characters": {} }));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Avatar.vrm");
    std::fs::write(&path, bytes).unwrap();
    let path = path.to_str().unwrap();
    let (actor, grants) = (Actor::Person { name: "tester".into() }, Grants::person());
    let project = ProjectDir::open(dir.path()).unwrap();
    let ctx =
        OpCtx::new(&doc, &actor, &*clock).with_grants(&grants).with_project_dir(&project).with_services(&services);

    let inspect = (find("io.inspect").unwrap().decode)(json!({ "path": path, "as": "avatar" })).unwrap();
    let inspected = inspect.plan(&ctx).expect("inspects").output.unwrap();
    let confirm = inspected["avatar"]["confirm"].clone();
    let inspected: AvatarSummary = serde_json::from_value(inspected["avatar"]["summary"].clone()).unwrap();

    let import: Import = serde_json::from_value(json!({ "path": path, "as": "avatar", "confirm": confirm })).unwrap();
    let plan = import.plan(&ctx).expect("plans the import");
    let output = plan.output.clone().unwrap_or(Value::Null);
    let signal = Signal(CancelFlag::new());
    let job_doc = Doc(&doc);
    let job = JobCtx::new(JobId(1), &actor, &job_doc, &signal)
        .with_grants(&grants)
        .with_project_dir(&project)
        .with_services(&services)
        .with_plan_output(&output);
    let result = import.work(&job).expect("imports");
    let finish = import.finish(&ctx, result.clone()).expect("finishes");
    doc.apply(&finish.patch).unwrap();
    let summary: AvatarSummary = serde_json::from_value(result["summary"].clone()).unwrap();
    let id: CharId = serde_json::from_value(result["id"].clone()).unwrap();

    let started = Instant::now();
    let avatar = load_avatar(&doc, &*store, &id, MtoonSupport::NONE, &*clock).expect("loads from the store");
    let reload = started.elapsed();
    let bones = avatar.skeleton.len();

    let started = Instant::now();
    let mut renderer = MockRenderer::new();
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::new(256, 256), alpha: AlphaEncoding::Straight }).unwrap();
    let camera = Camera::look_at(
        Vec3::new(0.0, 1.4, 1.2),
        Vec3::new(0.0, 1.4, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    );
    let mut stage =
        AvatarStage::new(&mut renderer, avatar, Scene::new(camera), target, Quality::live(), StageClock::fixed(60.0))
            .expect("stages");
    let pose = stage.base_pose();
    stage.tick(&mut renderer, &FrameInput { pose, ..FrameInput::default() }).expect("the first frame");
    let upload = started.elapsed();
    let palette = stage.scene().joints.len();
    stage.release(&mut renderer).unwrap();

    Dropped {
        summary: summary.with_timing(Stage::Upload, upload),
        inspected: inspected.timings,
        reload,
        bones,
        palette,
    }
}

/// Prints every stage as `reported` (spec §10's class for these rows) and, on `OMUZAKI_BUDGET_TIER=Dev`, fails on an
/// overrun.
fn report(name: &str, dropped: &Dropped, file_bytes: usize) {
    println!(
        "{name}: {:.1} MB, {} bones, {} joint matrices a frame",
        file_bytes as f64 / 1e6,
        dropped.bones,
        dropped.palette
    );
    for (stage, took) in &dropped.summary.timings {
        let budget = BUDGETS.iter().find(|(s, _)| s == stage).map_or(Duration::ZERO, |b| b.1);
        println!("  {stage:?}: {took:.3?} (budget {budget:.1?}) reported");
    }
    let total: Duration = dropped.summary.timings.iter().map(|(_, t)| *t).sum();
    println!("  machine total: {total:.3?} (budget {MACHINE_TOTAL:.1?}) reported");
    println!("  io.inspect before the confirm: {:.3?}", dropped.inspected);
    println!("  rebuilding from the store before the upload: {:.3?}", dropped.reload);
    if std::env::var("OMUZAKI_BUDGET_TIER").is_ok_and(|tier| tier == "Dev") {
        let overruns = stage_overruns(&dropped.summary.timings);
        assert!(overruns.is_empty(), "over budget on Dev: {overruns:?}");
    }
}

/// The stages of `dropped`, in order.
fn stages(dropped: &Dropped) -> Vec<Stage> {
    dropped.summary.timings.iter().map(|(s, _)| *s).collect()
}

#[test]
fn a_small_file_reports_all_five_stages() {
    let bytes = generated(SMALL);
    let dropped = drop_to_perform(&bytes);
    report("small", &dropped, bytes.len());
    assert_eq!(stages(&dropped), BUDGETS.map(|(s, _)| s), "every stage of D6's table, in order");
    assert_eq!(dropped.bones, SMALL.bones);
    // The face's four primitives share their skin's run of 131 joints: one run each would be four times that.
    assert!(dropped.palette < 2 * SMALL.bones, "{} joint matrices for {} bones", dropped.palette, SMALL.bones);
}

/// D6's file: run in the nightly with `cargo test --release -p omuzaki-stage --test byom_timing -- --ignored`.
#[test]
#[ignore = "40 MB of PNG work: the nightly runs it in release"]
fn a_40_mb_file_drops_to_performing_within_its_budget() {
    let bytes = generated(FULL);
    let size = bytes.len() as f64;
    assert!((size / 40e6 - 1.0).abs() <= 0.1, "the file is {:.1} MB, not 40 MB within 10 %", size / 1e6);
    let dropped = drop_to_perform(&bytes);
    report("full", &dropped, bytes.len());
    assert_eq!(stages(&dropped), BUDGETS.map(|(s, _)| s), "every stage of D6's table, in order");
    assert_eq!(dropped.bones, FULL.bones);
    assert_eq!(dropped.summary.texture_bytes, 447_392_400, "twenty 2048-pixel textures with their mips");
}

#[test]
fn a_stage_26_percent_over_its_budget_is_an_overrun_and_24_percent_is_not() {
    let read = Duration::from_millis(1500);
    let at = |percent: u32| vec![(Stage::Read, read * percent / 100)];
    assert_eq!(
        stage_overruns(&at(126)),
        [StageOverrun { stage: Some(Stage::Read), took: read * 126 / 100, budget: read }]
    );
    assert_eq!(stage_overruns(&at(124)), []);
}

#[test]
fn a_machine_total_over_8_s_is_its_own_row() {
    // Every stage within its slack, the sum over 8 s.
    let timings: Vec<(Stage, Duration)> = BUDGETS.iter().map(|&(s, b)| (s, b * 120 / 100)).collect();
    let total: Duration = timings.iter().map(|(_, t)| *t).sum();
    assert!(total > MACHINE_TOTAL);
    assert_eq!(stage_overruns(&timings), [StageOverrun { stage: None, took: total, budget: MACHINE_TOTAL }]);
    let within: Vec<(Stage, Duration)> = BUDGETS.to_vec();
    assert_eq!(stage_overruns(&within), [], "the budgets themselves sum to exactly 8 s");
}
