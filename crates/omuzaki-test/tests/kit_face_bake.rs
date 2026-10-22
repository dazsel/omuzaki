//! The face bake on real kit characters (#2057, FE-09 #33): `character.create` of each template, then `io.export` to
//! VRM 1.0 with the 52 perfect-sync channels, through the real kit, the real face baker and the real VRM exporter, all
//! linked here through the CLI (no implementation crate may depend on the others). The file is read back with the real
//! importer, and the faces its expressions show are drawn from the file alone. Set `OMUZAKI_EVIDENCE_DIR` to keep each
//! bake report, the face atlases and a sheet of those faces.

// Test helpers may panic: a panic is a clear test failure. The casts are pixel counts and coordinates, far inside
// f32's exact range, and the drawing names its triangle's corners a, b and c.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]
#![allow(clippy::disallowed_methods, reason = "the test reads the file the op wrote and keeps evidence beside it")]

use std::collections::HashMap;
use std::sync::Arc;

use image::{Rgba, RgbaImage};
use omuzaki_core::glam::{Vec2, Vec3, Vec4};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{
    ARKIT_52, AlphaMode, BakeReport, Expression, ExpressionName, ImageMime, ImportCtx, ImportInput, ImportOptions,
    IoScene, PresetExpression, Realisation, run_import,
};
use omuzaki_io_ops::ops::export::{Export, ExportResult};
use omuzaki_io_vrm::Vrm1Importer;
use omuzaki_ops_api::{
    Actor, CancelFlag, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, Plan, ProjectDir, Services,
    ServicesBuilder, find,
};
use omuzaki_store_api::Store;
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

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

/// A project holding the kit character `template`, with the licence record `licence.derive` proposes stored.
struct Project {
    doc: MemDoc,
    services: Services,
    dir: tempfile::TempDir,
}

impl Project {
    fn new(template: &str) -> Self {
        omuzaki_test::link();
        let mut project = Self {
            doc: MemDoc::from_value(json!({})),
            services: ServicesBuilder::new().insert::<dyn Store>(Arc::new(MemStore::new())).build(),
            dir: tempfile::tempdir().unwrap(),
        };
        let create = json!({ "id": template, "name": template, "template": template, "seed": 7 });
        project.apply("character.create", create);
        let derived = project.plan("licence.derive", json!({ "character": template }), true).output.unwrap();
        let mut record = derived["record"].clone();
        record["meta"]["authors"] = json!(["Omuzaki kit"]);
        project.apply("licence.set", json!({ "character": template, "record": record }));
        project
    }

    fn plan(&self, op: &str, args: Value, dry: bool) -> Plan {
        let decoded = (find(op).unwrap_or_else(|| panic!("{op} is registered")).decode)(args).unwrap();
        let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
        let ctx = OpCtx::new(&self.doc, &actor, &clock).with_services(&self.services).with_dry_run(dry);
        decoded.plan(&ctx).unwrap_or_else(|e| panic!("{op}: {e:?}"))
    }

    fn apply(&mut self, op: &str, args: Value) {
        let plan = self.plan(op, args, false);
        self.doc.apply(&plan.patch).unwrap();
    }

    /// `io.export` as the executor runs a job.
    fn export(&self, args: Value) -> ExportResult {
        let op: Export = serde_json::from_value(args).unwrap();
        let (actor, clock, grants) = (Actor::Person { name: "tester".into() }, FakeClock::default(), Grants::person());
        let project = ProjectDir::open(self.dir.path()).unwrap();
        let ctx = OpCtx::new(&self.doc, &actor, &clock)
            .with_grants(&grants)
            .with_project_dir(&project)
            .with_services(&self.services);
        op.plan(&ctx).unwrap();
        let (doc, signal) = (Doc(&self.doc), Signal(CancelFlag::new()));
        let job = JobCtx::new(JobId(1), &actor, &doc, &signal)
            .with_grants(&grants)
            .with_project_dir(&project)
            .with_services(&self.services);
        serde_json::from_value(op.work(&job).unwrap()).unwrap()
    }
}

/// Writes `bytes` to `OMUZAKI_EVIDENCE_DIR` as `name`, when it is set.
fn keep(name: &str, bytes: &[u8]) {
    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join(name), bytes).unwrap();
    }
}

fn kind(realisation: &Realisation) -> &'static str {
    match realisation {
        Realisation::Morph { .. } => "morph",
        Realisation::Flipbook { .. } => "flipbook",
        Realisation::Colour => "colour",
        Realisation::Mixed => "mixed",
        Realisation::Inert { .. } => "inert",
    }
}

fn bake(template: &str) -> (BakeReport, Vec<u8>) {
    let project = Project::new(template);
    let path = format!("out/{template}.vrm");
    let result = project.export(json!({ "character": template, "format": "vrm1", "path": path, "purpose": "private" }));
    let bytes = std::fs::read(project.dir.path().join(&path)).unwrap();
    (result.bake.expect("the kit character's face is baked"), bytes)
}

/// The faces drawn from a baked file: the head mesh (the one whose primitives carry the `face.*` atlases), seen from
/// the front with an orthographic camera, the way a VRM viewer poses an expression at full weight: its morph binds
/// move the vertices, its UV binds move each atlas's cell and its colour binds fade the patches. It reads only the
/// imported scene.
struct Viewer<'a> {
    scene: &'a IoScene,
    /// The head mesh's node.
    node: usize,
    images: HashMap<usize, RgbaImage>,
    /// The face patches' centre and the frame's half size, metres.
    centre: Vec2,
    half: f32,
}

/// The side of a drawn face, pixels.
const FACE_PX: u32 = 320;
/// How far behind the skin a blended patch may sit and still show, metres: patches are lifted a millimetre or so.
const LIFT_TOLERANCE: f32 = 0.003;

impl<'a> Viewer<'a> {
    fn new(scene: &'a IoScene) -> Self {
        let is_face = |material: usize| {
            scene.materials[material].base_color_texture.as_ref().is_some_and(|t| {
                scene.images[scene.textures[t.texture.index()].image.index()].name.starts_with("face.")
            })
        };
        let mesh = scene
            .meshes
            .iter()
            .position(|m| m.primitives.iter().any(|p| p.material.is_some_and(|m| is_face(m.index()))))
            .expect("a mesh carries the face patches");
        let node = scene.nodes.iter().position(|n| n.mesh.is_some_and(|m| m.index() == mesh)).expect("a node draws it");
        let (mut min, mut max) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for primitive in &scene.meshes[mesh].primitives {
            if primitive.material.is_some_and(|m| is_face(m.index())) {
                for p in &primitive.mesh.positions {
                    min = min.min(p.truncate());
                    max = max.max(p.truncate());
                }
            }
        }
        // The frame holds the patches with room around them for the head's outline.
        let half = (max - min).max_element() * 0.75;
        Self { scene, node, images: HashMap::new(), centre: (min + max) / 2.0, half }
    }

    fn image(&mut self, index: usize) -> &RgbaImage {
        let scene = self.scene;
        self.images
            .entry(index)
            .or_insert_with(|| image::load_from_memory(scene.images[index].bytes.as_slice()).unwrap().to_rgba8())
    }

    /// `primitive`'s vertices moved by `expression`'s morph binds on the head.
    fn posed(&self, primitive: &omuzaki_io_api::Primitive, expression: Option<&Expression>) -> Vec<Vec3> {
        let mut positions = primitive.mesh.positions.clone();
        for bind in expression.map(|e| e.morph_binds.as_slice()).unwrap_or_default() {
            if bind.node.index() == self.node
                && let Some(target) = primitive.morph_targets.get(bind.index as usize)
            {
                for (p, d) in positions.iter_mut().zip(&target.positions) {
                    *p += *d * bind.weight;
                }
            }
        }
        positions
    }

    /// The face with `expression` at full weight (`None`: at rest), and the share of the patches' pixels that sit on
    /// the head's front (not buried under the skin).
    fn draw(&mut self, expression: Option<&'a Expression>) -> (RgbaImage, f32) {
        let scene = self.scene;
        let mesh = &scene.meshes[scene.nodes[self.node].mesh.unwrap().index()];
        let mut canvas = RgbaImage::from_pixel(FACE_PX, FACE_PX, Rgba([40, 44, 52, 255]));
        let mut depth = vec![f32::MIN; (FACE_PX * FACE_PX) as usize];
        let (mut shown, mut covered) = (0_usize, 0_usize);
        // Opaque parts first, then the blended patches over them in the file's order.
        for blended in [false, true] {
            for primitive in &mesh.primitives {
                let material_ix = primitive.material.expect("the kit gives every primitive a material");
                let material = &scene.materials[material_ix.index()];
                if (material.alpha == AlphaMode::Blend) != blended {
                    continue;
                }
                let binds = |e: &'a Expression| e.uv_binds.iter().filter(move |b| b.material == material_ix);
                let offset: Vec2 = expression.map(|e| binds(e).map(|b| b.offset).sum()).unwrap_or_default();
                let scale: Vec2 = expression.map_or(Vec2::ONE, |e| binds(e).map(|b| b.scale).product());
                let tint: Vec4 = expression
                    .and_then(|e| e.color_binds.iter().find(|b| b.material == material_ix))
                    .map_or(material.base_color, |b| b.target);
                let data = &primitive.mesh;
                let positions = self.posed(primitive, expression);
                let texture =
                    material.base_color_texture.as_ref().map(|t| scene.textures[t.texture.index()].image.index());
                if let Some(index) = texture {
                    self.image(index);
                }
                let atlas = texture.map(|index| &self.images[&index]);
                let to_px = |p: Vec3| {
                    let q = (p.truncate() - self.centre) / self.half;
                    Vec2::new(f32::midpoint(q.x, 1.0) * FACE_PX as f32, f32::midpoint(1.0, -q.y) * FACE_PX as f32)
                };
                for triangle in data.indices.as_chunks::<3>().0 {
                    let [a, b, c] = [0, 1, 2].map(|k| triangle[k] as usize);
                    let (pa, pb, pc) = (to_px(positions[a]), to_px(positions[b]), to_px(positions[c]));
                    let area = (pb - pa).perp_dot(pc - pa);
                    if area.abs() < 1e-9 {
                        continue;
                    }
                    let lo = pa.min(pb).min(pc).max(Vec2::ZERO).floor();
                    let hi = pa.max(pb).max(pc).min(Vec2::splat(FACE_PX as f32 - 1.0)).ceil();
                    for y in lo.y as u32..=hi.y.max(lo.y) as u32 {
                        for x in lo.x as u32..=hi.x.max(lo.x) as u32 {
                            let q = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                            let w = Vec3::new(
                                (pc - pb).perp_dot(q - pb),
                                (pa - pc).perp_dot(q - pc),
                                (pb - pa).perp_dot(q - pa),
                            ) / area;
                            if w.min_element() < 0.0 {
                                continue;
                            }
                            let z = w.x * positions[a].z + w.y * positions[b].z + w.z * positions[c].z;
                            let at = (y * FACE_PX + x) as usize;
                            let mut colour = tint;
                            if let Some(atlas) = atlas
                                && !data.uvs.is_empty()
                            {
                                let uv = (data.uvs[a] * w.x + data.uvs[b] * w.y + data.uvs[c] * w.z) * scale + offset;
                                let (ax, ay) =
                                    ((uv.x * atlas.width() as f32) as i64, (uv.y * atlas.height() as f32) as i64);
                                let texel = u32::try_from(ax)
                                    .ok()
                                    .zip(u32::try_from(ay).ok())
                                    .and_then(|(ax, ay)| atlas.get_pixel_checked(ax, ay));
                                let Some(texel) = texel else { continue };
                                let srgb = Vec4::from_array(texel.0.map(|c| f32::from(c) / 255.0));
                                colour *= Vec4::new(srgb.x.powf(2.2), srgb.y.powf(2.2), srgb.z.powf(2.2), srgb.w);
                            }
                            let pixel = canvas.get_pixel_mut(x, y);
                            if blended {
                                if colour.w <= 0.0 {
                                    continue;
                                }
                                covered += 1;
                                if z < depth[at] - LIFT_TOLERANCE {
                                    continue;
                                }
                                shown += 1;
                                for k in 0..3 {
                                    let over = colour[k].powf(1.0 / 2.2) * 255.0;
                                    pixel[k] = (over * colour.w + f32::from(pixel[k]) * (1.0 - colour.w)).round() as u8;
                                }
                            } else if z > depth[at] {
                                depth[at] = z;
                                // A soft key light from the front, so the head's shape reads.
                                let n = (data.normals[a] * w.x + data.normals[b] * w.y + data.normals[c] * w.z)
                                    .normalize_or_zero();
                                let light = 0.45 + 0.55 * n.z.max(0.0);
                                for k in 0..3 {
                                    pixel[k] = ((colour[k] * light).powf(1.0 / 2.2) * 255.0).round().min(255.0) as u8;
                                }
                            }
                        }
                    }
                }
            }
        }
        (canvas, shown as f32 / covered.max(1) as f32)
    }
}

fn mean_difference(a: &RgbaImage, b: &RgbaImage) -> f32 {
    let total: u64 = a.as_raw().iter().zip(b.as_raw()).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    total as f32 / a.as_raw().len() as f32
}

/// The expressions on the evidence sheet, in its order.
const SHEET: [Option<PresetExpression>; 10] = [
    None,
    Some(PresetExpression::Blink),
    Some(PresetExpression::BlinkLeft),
    Some(PresetExpression::Happy),
    Some(PresetExpression::Angry),
    Some(PresetExpression::Sad),
    Some(PresetExpression::Relaxed),
    Some(PresetExpression::Surprised),
    Some(PresetExpression::Aa),
    Some(PresetExpression::Oh),
];

/// Draws every expression of [`SHEET`] from `scene`, checks each one shows on the face and differs from rest, and
/// keeps the sheet.
fn check_the_faces(template: &str, scene: &IoScene) {
    let avatar = scene.avatar.as_ref().unwrap();
    let mut viewer = Viewer::new(scene);
    let faces: Vec<(String, RgbaImage, f32)> = SHEET
        .iter()
        .map(|preset| {
            let expression = preset.map(|p| &avatar.expressions[&ExpressionName::Preset(p)]);
            let (face, on_front) = viewer.draw(expression);
            (preset.map_or_else(|| "rest".to_owned(), |p| format!("{p:?}")), face, on_front)
        })
        .collect();
    let mut sheet = RgbaImage::new(FACE_PX * 5, FACE_PX * 2);
    for (at, (name, face, on_front)) in faces.iter().enumerate() {
        let d = mean_difference(face, &faces[0].1);
        println!("  {name}: {:.1}% of the patches on the head's front, {d:.2} from rest", on_front * 100.0);
        // The face sits on the head, not inside it: the bug this bake fixed put the eyes and the mouth under the skin.
        assert!(*on_front > 0.95, "{template} {name}: only {on_front} of the patches show");
        if at > 0 {
            assert!(d > 0.2, "{template} {name} looks like rest from the file (mean difference {d})");
        }
        let (x, y) = ((at as u32 % 5) * FACE_PX, (at as u32 / 5) * FACE_PX);
        image::imageops::replace(&mut sheet, face, i64::from(x), i64::from(y));
    }
    let mut png = std::io::Cursor::new(Vec::new());
    sheet.write_to(&mut png, image::ImageFormat::Png).unwrap();
    keep(&format!("{template}-faces.png"), png.get_ref());
}

#[test]
fn every_kit_template_bakes_its_face_where_the_renderer_draws_it() {
    for template in ["standard", "mini"] {
        let (report, bytes) = bake(template);
        let mut counts = std::collections::BTreeMap::new();
        for channel in &report.channels {
            *counts.entry(kind(&channel.realisation)).or_insert(0) += 1;
        }
        let warnings: Vec<String> = report
            .findings
            .findings
            .iter()
            .filter(|f| f.severity == omuzaki_io_api::Severity::Warning)
            .map(|f| format!("{}: {}", f.rule, f.message))
            .collect();
        println!("{template}: {} channels {counts:?}", report.channels.len());
        for finding in &report.findings.findings {
            println!("  {:?} {}: {}", finding.severity, finding.rule, finding.message);
        }
        keep(&format!("bake-{template}.json"), serde_json::to_string_pretty(&report).unwrap().as_bytes());

        // Every feature finds the head under it, and every morph moves some of it: the face sits where the renderer
        // draws it, not at the head bone (the head's base), which put the eyes and the mouth below the head.
        for rule in ["face.bake_no_patch", "face.bake_morph_empty"] {
            assert!(!warnings.iter().any(|w| w.starts_with(rule)), "{template}: {warnings:#?}");
        }
        // The only channels with no form are the gaze channels, which the look-at block turns.
        let inert: Vec<&str> = report
            .channels
            .iter()
            .filter(|c| matches!(c.realisation, Realisation::Inert { .. }))
            .map(|c| c.channel.as_str())
            .collect();
        assert!(inert.iter().all(|c| c.starts_with("eyeLook")), "{template}: inert {inert:?}");
        assert_eq!(inert.len(), 8, "{template}: inert {inert:?}");

        // The file reads back with every channel, every preset and the atlases.
        let scene = run_import(&Vrm1Importer, &ImportInput::new(&bytes), &ImportCtx::new(&ImportOptions::default()))
            .expect("the file reads back")
            .scene;
        let avatar = scene.avatar.as_ref().expect("a VRM has an avatar");
        for channel in ARKIT_52 {
            assert!(avatar.expressions.contains_key(&ExpressionName::parse(channel).unwrap()), "{template}: {channel}");
        }
        for preset in PresetExpression::ALL {
            assert!(avatar.expressions.contains_key(&ExpressionName::Preset(preset)), "{template}: {preset:?}");
        }
        let atlases: Vec<_> = scene.images.iter().filter(|i| i.name.starts_with("face.")).collect();
        for name in ["face.eye-left", "face.eye-right", "face.mouth", "face.brow-left", "face.brow-right"] {
            assert!(atlases.iter().any(|i| i.name == name), "{template}: {name} is in the file");
        }
        for image in atlases {
            assert_eq!(image.mime, ImageMime::Png);
            keep(&format!("{template}-{}.png", image.name), image.bytes.as_slice());
        }
        check_the_faces(template, &scene);
    }
}
