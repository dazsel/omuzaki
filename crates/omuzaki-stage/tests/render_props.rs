//! Props on the GPU (#932, #1166, #1273, #2416): every interior kind at its default and an extreme on a contact sheet,
//! each stage preset with the kit's character in it (made through the ops, as a person or an agent would), and a
//! two-shot in the study with depth of field on the subject's face. Set `OMUZAKI_EVIDENCE_DIR` to keep the pictures.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![cfg(feature = "wgpu")]
#![allow(clippy::disallowed_methods, reason = "the evidence images are written beside the test, not by an op")]

use std::collections::BTreeMap;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_char_api::{BuiltCharacter, CharId};
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Op, OpCtx};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, Image, Instance, Projection, Quality, Renderer, Scene, TargetDesc,
};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::cast::CastStage;
use omuzaki_stage::frame::face::{FaceRig, InlineExecutor};
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::ops::render::still::encode_png;
use omuzaki_stage::recipe::LightRig;
use omuzaki_stage::scene::default::scene_for;
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage::scene::resolve::{CheckedScene, Figure, ResolvedScene};
use omuzaki_stage::stages::FrameLoop;
use omuzaki_stage_api::{CastMember, MemberId, SceneDef, SceneId, prop_kinds};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn renderer() -> WgpuRenderer {
    WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .expect("a software adapter (install mesa-vulkan-drivers)")
}

/// Writes `image` as `<name>.png` into `OMUZAKI_EVIDENCE_DIR`, when it is set.
fn keep(name: &str, image: &Image) {
    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join(format!("{name}.png")), encode_png(image).unwrap()).unwrap();
    }
}

/// `tiles` (all one size) laid out `columns` across.
fn sheet(tiles: &[Image], columns: usize) -> Image {
    let (w, h) = (tiles[0].size.x as usize, tiles[0].size.y as usize);
    let rows = tiles.len().div_ceil(columns);
    let mut rgba = vec![0_u8; w * columns * h * rows * 4];
    for (i, tile) in tiles.iter().enumerate() {
        let (cx, cy) = (i % columns, i / columns);
        for y in 0..h {
            let from = &tile.rgba8[y * w * 4..(y + 1) * w * 4];
            let at = ((cy * h + y) * w * columns + cx * w) * 4;
            rgba[at..at + w * 4].copy_from_slice(from);
        }
    }
    let size = UVec2::new(u32::try_from(w * columns).unwrap(), u32::try_from(h * rows).unwrap());
    Image::new(size, tiles[0].alpha, rgba).unwrap()
}

/// The share of `image`'s pixels that differ from its top-left corner by more than a few levels.
fn coverage(image: &Image) -> f64 {
    let corner = &image.rgba8[..4];
    let (pixels, _) = image.rgba8.as_chunks::<4>();
    let n = pixels.len();
    let differ = pixels.iter().filter(|p| p.iter().zip(corner).take(3).any(|(a, b)| a.abs_diff(*b) > 6)).count();
    #[allow(clippy::cast_precision_loss)] // pixel counts
    let share = differ as f64 / n as f64;
    share
}

#[test]
fn on_the_gpu_every_prop_kind_at_its_default_and_an_extreme() {
    omuzaki_stage::link();
    let mut renderer = renderer();
    let size = UVec2::new(240, 240);
    let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight }).unwrap();
    let rig = LightRig::load("viewer-studio").unwrap();
    let extremes: BTreeMap<&str, Value> = BTreeMap::from([
        ("bed", json!({"width": 0.9, "pillows": 1, "blanket": "#7aa6d6"})),
        ("cafe-table", json!({"radius": 0.3, "height": 1.05})),
        ("chair", json!({"back": 0.0, "cushion": null})),
        ("cup", json!({"height": 0.14, "radius": 0.03, "saucer": true, "colour": "#f4efe6"})),
        ("desk", json!({"width": 2.2, "drawers": false})),
        ("lamp", json!({"style": "table", "height": 0.5, "shade": "#b9d8c8"})),
        ("plant", json!({"height": 1.8, "leaves": 50, "seed": 2})),
        ("room", json!({"width": 3.0, "depth": 3.0, "wainscot": null, "walls": "#c9d8e8"})),
        ("rug", json!({"width": 1.0, "length": 2.0, "colour": "#3a5a8a"})),
        ("shelf", json!({"width": 1.8, "height": 1.0, "shelves": 2, "books": 1.0, "seed": 7})),
        ("window", json!({"panes": [1, 1], "curtains": null, "sky": "#f2a07a", "horizon": "#ffd8a8"})),
    ]);
    let mut tiles = Vec::new();
    for kind in prop_kinds().into_iter().filter(|k| extremes.contains_key(k.id)) {
        for params in [json!({}), extremes[kind.id].clone()] {
            let built = kind.build(&kind.check(&params).unwrap()).unwrap();
            // Framed on its bounds from the front and a little above, three-quarters on.
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for p in built.parts.iter().flat_map(|p| p.mesh.positions.iter()) {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
            let centre = (lo + hi) * 0.5;
            let radius = (hi - lo).length() * 0.5;
            let eye = centre + Vec3::new(0.55, 0.45, 1.0).normalize() * radius * 3.3;
            let camera = Camera::look_at(
                eye,
                centre,
                Vec3::Y,
                Projection::Perspective { fov_y_deg: 30.0, near: radius * 0.05, far: radius * 8.0 },
            );
            let mut scene = Scene::new(camera);
            scene.lights = rig.to_lights().unwrap();
            scene.lights.extend(built.lights.iter().copied());
            scene.lights.truncate(omuzaki_render_api::limits::MAX_LIGHTS);
            scene.background = Background::Gradient { top: Colour::hex(0x00f4_f1ec), bottom: Colour::hex(0x00d8_d2c8) };
            scene.post = rig.post(false);
            let mut made = Vec::new();
            for part in &built.parts {
                let mesh = renderer.create_mesh(&part.mesh).unwrap();
                let material = renderer.create_material(&part.material).unwrap();
                let mut instance = Instance::new(mesh, material, Mat4::IDENTITY);
                instance.casts_shadow = part.casts_shadow;
                scene.instances.push(instance);
                made.push((mesh, material));
            }
            renderer.render(target, &scene, &Quality::high()).unwrap();
            let image = renderer.read_target(target).unwrap();
            let share = coverage(&image);
            println!("{} {params}: {} triangles, covers {share:.3}", kind.id, built.triangles());
            assert!(share > 0.05, "{} {params}: the prop shows ({share:.3})", kind.id);
            tiles.push(image);
            for (mesh, material) in made {
                renderer.release(mesh.into()).unwrap();
                renderer.release(material.into()).unwrap();
            }
        }
    }
    keep("props-kinds-sheet", &sheet(&tiles, 6));
    renderer.release(target.into()).unwrap();
}

/// The kit's default character from `template`, made by `character.create` as a person would.
fn kit(template: &str) -> BuiltCharacter {
    use omuzaki_char::builder::CachingBuilder;
    use omuzaki_char_api::{BuildOptions, BuildServices, CharacterBuilder, CharacterDef};
    use omuzaki_doc_api::{DocPath, DocRead};
    use omuzaki_ops_api::find;

    omuzaki_stage::link();
    let mut doc = MemDoc::new();
    let registration = find("character.create").expect("character.create is registered");
    let op = (registration.decode)(json!({"id": "kit", "name": "Kit", "template": template, "seed": 7}))
        .expect("the arguments decode");
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).expect("character.create plans");
    doc.apply(&plan.patch).expect("the patch applies");
    let node = doc.get(&DocPath::parse("characters/kit").unwrap()).expect("the character is stored").clone();
    let def = CharacterDef::from_node(&node).expect("the stored character reads back");
    CachingBuilder::new()
        .build(&CharId::new("kit").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the default character builds")
}

/// Plans `args` as op `T` on `doc` and applies it.
#[allow(clippy::needless_pass_by_value, reason = "every call site passes a json! literal")]
fn apply<T: Op + DeserializeOwned>(doc: &mut MemDoc, args: Value) -> Value {
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let op: T = serde_json::from_value(args.clone()).expect("the arguments read");
    let plan = op.plan(&OpCtx::new(doc, &actor, &clock)).unwrap_or_else(|e| panic!("{} {args}: {e:?}", T::meta().name));
    doc.apply(&plan.patch).expect("applies");
    plan.output.unwrap_or(Value::Null)
}

/// A project with characters `kiri` and `aiko` and a scene `main` casting `kiri`, put in stage preset `preset`, lit by
/// the rig it pairs with, and shot from a free camera `wide`: every step an op.
#[allow(clippy::needless_pass_by_value, reason = "every call site passes a json! literal")]
fn staged(preset: &str, wide: Value) -> MemDoc {
    use omuzaki_stage::ops::scene::camera::{AddCamera, SetActiveCamera};
    use omuzaki_stage::ops::scene::create::CreateScene;
    use omuzaki_stage::ops::scene::light::SetLightRig;
    use omuzaki_stage::ops::scene::stage_preset::SetStagePreset;

    omuzaki_stage::link();
    let mut doc = MemDoc::from_value(json!({
        "characters": { "kiri": { "name": "Kiri", "parts": {} }, "aiko": { "name": "Aiko", "parts": {} } },
        "project": { "default_character": "kiri" },
    }));
    apply::<CreateScene>(&mut doc, json!({ "id": "main" }));
    let out = apply::<SetStagePreset>(&mut doc, json!({ "preset": preset }));
    if let Some(rig) = out["light_rig"].as_str() {
        apply::<SetLightRig>(&mut doc, json!({ "rig": rig }));
    }
    let mut camera = json!({ "id": "wide", "framing": "free", "anchor": "stage" });
    camera.as_object_mut().unwrap().extend(wide.as_object().unwrap().clone());
    apply::<AddCamera>(&mut doc, camera);
    apply::<SetActiveCamera>(&mut doc, json!({ "camera": "wide" }));
    doc
}

/// Renders scene `def` with `bodies` (by member id, the subject first) as one cast, posed at rest with relaxed arms,
/// after a second of frames so the springs settle.
fn render_cast(def: &SceneDef, bodies: Vec<(MemberId, BuiltCharacter)>, size: UVec2) -> Image {
    let mut renderer = renderer();
    let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight }).unwrap();
    let figures: BTreeMap<MemberId, Figure> = bodies
        .iter()
        .map(|(id, built)| (id.clone(), Figure::new(&def.cast[id], Landmarks::measure(built).unwrap())))
        .collect();
    let mut resolved = ResolvedScene::new();
    #[allow(clippy::cast_precision_loss)] // a frame's size
    let aspect = size.x as f32 / size.y as f32;
    resolved.resolve_into(CheckedScene::new(def).unwrap(), &figures, aspect).unwrap();
    let options = UploadOptions { face_px_per_m: 2000.0 };
    let mut inputs = Vec::new();
    let mut cast: Option<CastStage<Stage>> = None;
    for (id, built) in bodies {
        let mut pose = HumanoidPose::REST;
        seed_base_pose(&built.rig, &mut pose);
        inputs.push((id.clone(), FrameInput { pose, dt: 1.0 / 60.0, ..FrameInput::default() }));
        let stage_target = if cast.is_none() {
            target
        } else {
            renderer.create_target(&TargetDesc { size: UVec2::splat(8), alpha: AlphaEncoding::Straight }).unwrap()
        };
        let mut stage = Stage::new(
            &mut renderer,
            built,
            options,
            Scene::new(Camera::default()),
            stage_target,
            Quality::offline(),
            StageClock::fixed(60.0),
        )
        .unwrap();
        stage.set_face(&mut renderer, FaceRig::standard().with_executor(Box::new(InlineExecutor))).unwrap();
        match cast.as_mut() {
            None => cast = Some(CastStage::new(id, stage)),
            Some(cast) => assert!(cast.add_member(id, stage, Mat4::IDENTITY).is_none()),
        }
    }
    let mut cast = cast.unwrap();
    cast.set_member_roots(&figures);
    let lead = cast.lead_id().clone();
    cast.set_scene(&resolved, Mat4::from(figures[&lead].root));
    cast.pay_scene(&mut renderer).unwrap();
    assert!(cast.owed().is_empty(), "the scene is paid");
    let refs: Vec<(MemberId, &FrameInput)> = inputs.iter().map(|(id, input)| (id.clone(), input)).collect();
    for _ in 0..60 {
        cast.tick_members(&mut renderer, &refs).unwrap();
    }
    let image = renderer.read_target(target).unwrap();
    FrameLoop::release(cast, &mut renderer).unwrap();
    image
}

#[test]
fn on_the_gpu_each_stage_preset_with_the_kit_character() {
    let presets = ["study", "bedroom", "cafe", "night-library", "desk", "green-screen"];
    let mut tiles = Vec::new();
    for preset in presets {
        // The desk is a bust shot's set; the rooms are seen whole.
        let wide = if preset == "desk" {
            json!({ "position": [0.0, 1.3, 2.2], "target": [0.0, 1.0, 0.0], "fov": 35.0 })
        } else {
            json!({ "position": [0.4, 1.45, 4.4], "target": [0.0, 0.95, -0.6], "fov": 45.0 })
        };
        let doc = staged(preset, wide);
        let def = scene_for(&doc, Some(&SceneId::new("main").unwrap())).unwrap().1;
        let subject = def.cast.keys().next().unwrap().clone();
        let image = render_cast(&def, vec![(subject, kit("standard"))], UVec2::new(480, 270));
        let share = coverage(&image);
        println!("{preset}: {} props, covers {share:.3}", def.props.len());
        if preset != "green-screen" {
            assert!(share > 0.5, "{preset}: the room fills the frame ({share:.3})");
        }
        keep(&format!("props-preset-{preset}"), &image);
        tiles.push(image);
    }
    keep("props-presets-sheet", &sheet(&tiles, 2));
}

#[test]
fn on_the_gpu_a_two_shot_in_the_study_with_depth_of_field() {
    use omuzaki_stage::ops::scene::look::SetPost;

    let wide = json!({ "position": [0.2, 1.2, 3.1], "target": [0.05, 0.92, 0.0], "fov": 34.0 });
    let mut doc = staged("study", wide);
    apply::<SetPost>(
        &mut doc,
        json!({ "depth_of_field": { "focus": "subject", "range": 0.25, "max_blur": 0.012 }, "bloom": true }),
    );
    let mut def = scene_for(&doc, Some(&SceneId::new("main").unwrap())).unwrap().1;
    // Two of the kit's characters side by side, turned toward each other; the subject on the left is in focus.
    let (left, right) = (MemberId::new("a-kiri").unwrap(), MemberId::new("b-aiko").unwrap());
    let mut kiri = CastMember::new(CharId::new("kiri").unwrap());
    (kiri.position, kiri.yaw) = ([-0.38, 0.0], 18.0);
    let mut aiko = CastMember::new(CharId::new("aiko").unwrap());
    (aiko.position, aiko.yaw) = ([0.42, -0.35], -24.0);
    def.cast = BTreeMap::from([(left.clone(), kiri), (right.clone(), aiko)]);
    def.scene.subject = Some(left.clone());
    let image = render_cast(&def, vec![(left, kit("standard")), (right, kit("soft-sd"))], UVec2::new(1280, 720));
    assert!(coverage(&image) > 0.5);
    keep("props-two-shot-study", &image);
}
