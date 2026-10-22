//! An FBX character as an avatar (spec v3 §27 EX-09, EX-12; `docs/design/byom.md`): the kit's mock character, written
//! as the FBX a DCC tool would export (centimetres, Euler bones, skin clusters, blend shapes, texture files beside it,
//! no humanoid block), imported, auto-mapped and prepared like any dropped model, and staged. It must come out as the
//! same avatar its VRM 1.0 copy makes: the same bones, the same geometry, the same pixels. No third-party model.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::disallowed_methods, reason = "the evidence images are written beside the test, not by an op")]

#[path = "avatar/fbx.rs"]
mod fbx;
#[path = "avatar/kit.rs"]
mod kit;

use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_char_api::{BuildServices, Quality as BuildQuality, RegistryBuilder};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{ImportCtx, ImportInput, ImportOptions, ImportOutcome, IoScene, run_import};
use omuzaki_io_fbx::FbxImporter;
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_render_api::MtoonSupport;
use omuzaki_stage::body::{Body, BodyServices, load_body};
use omuzaki_store_api::{ContentHash, Store};
use omuzaki_store_mock::MemStore;
use serde_json::json;

/// The kit character's VRM copy, and the same scene written as FBX and read back.
fn both() -> (IoScene, ImportOutcome, Vec<u8>) {
    let vrm = kit::as_vrm(&kit::kit());
    let (bytes, folder) = fbx::fbx(&vrm);
    let options = ImportOptions::default();
    let read = run_import(
        &FbxImporter,
        &ImportInput::new(&bytes).named("kit.fbx"),
        &ImportCtx::new(&options).with_resolver(&folder),
    )
    .expect("the FBX reads");
    (vrm, read, bytes)
}

fn avatar(scene: &IoScene, format: SourceFormat, file: &str) -> ImportedAvatar {
    let def = AvatarDef::new(AvatarSource::new(ContentHash([7; 32]), format, file));
    to_avatar(scene, &def, MtoonSupport::NONE).expect("prepares as an avatar").0
}

/// The axis-aligned bounds of every vertex of every mesh of `avatar`, at bind.
fn bounds(avatar: &ImportedAvatar) -> (Vec3, Vec3) {
    avatar
        .meshes
        .iter()
        .flat_map(|m| m.mesh.geometry.positions.iter())
        .fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(*p), hi.max(*p)))
}

#[test]
fn the_kit_as_fbx_reads_back_with_every_mesh_skin_material_and_texture() {
    let (vrm, read, _) = both();
    let fbx = &read.scene;
    let errors: Vec<_> = read.report.errors().collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert!(fbx.avatar.is_none(), "FBX has no humanoid block");
    assert_eq!(fbx.nodes.len(), vrm.nodes.len());
    assert_eq!(fbx.meshes.len(), vrm.meshes.len());
    // FBX skins each mesh on its own (a skin deformer belongs to one geometry), so a skin the VRM shares is one per
    // mesh here, over the same joints.
    let joints = |s: &IoScene, node: &omuzaki_io_api::Node| {
        node.skin.map(|k| s.skins[k.index()].joints.iter().map(|j| s.nodes[j.index()].name.clone()).collect::<Vec<_>>())
    };
    // ufbx lists nodes in its own order, so nodes are matched by name (the kit's names are unique).
    let named = |s: &IoScene, name: &str| s.nodes.iter().find(|n| n.name == name).expect("the node is there").clone();
    for b in &vrm.nodes {
        assert_eq!(joints(fbx, &named(fbx, &b.name)), joints(&vrm, b), "{}", b.name);
    }
    assert_eq!(fbx.materials.len(), vrm.materials.len());
    assert_eq!(fbx.images.len(), vrm.images.len(), "every texture file was found beside the model");
    let triangles =
        |s: &IoScene| s.meshes.iter().flat_map(|m| &m.primitives).map(|p| p.mesh.indices.len() / 3).sum::<usize>();
    assert_eq!(triangles(fbx), triangles(&vrm));
    let targets = |s: &IoScene| s.meshes.iter().map(omuzaki_io_api::Mesh::morph_target_count).sum::<usize>();
    assert_eq!(targets(fbx), targets(&vrm));
    // Every node is where the VRM puts it, to a tenth of a millimetre (the file is in centimetres, Euler degrees).
    for b in &vrm.nodes {
        let a = named(fbx, &b.name);
        assert!(a.transform.translation.abs_diff_eq(b.transform.translation, 1e-4), "{}", a.name);
        assert!(a.transform.rotation.dot(b.transform.rotation).abs() > 1.0 - 1e-6, "{}", a.name);
    }
}

#[test]
fn the_kit_as_fbx_prepares_as_the_same_avatar_as_its_vrm() {
    let (vrm, read, _) = both();
    let from_vrm = avatar(&vrm, SourceFormat::Vrm1, "kit.vrm");
    let from_fbx = avatar(&read.scene, SourceFormat::Fbx, "kit.fbx");
    // The humanoid the VRM declares is the one the auto-mapping finds in the FBX, bone for bone.
    let names =
        |a: &ImportedAvatar| a.skeleton.bones().iter().map(|b| (b.name.clone(), b.humanoid)).collect::<Vec<_>>();
    assert_eq!(names(&from_fbx), names(&from_vrm));
    let vertices = |a: &ImportedAvatar| a.meshes.iter().map(|m| m.mesh.geometry.positions.len()).sum::<usize>();
    assert_eq!(vertices(&from_fbx), vertices(&from_vrm));
    let (lo, hi) = bounds(&from_fbx);
    let (vlo, vhi) = bounds(&from_vrm);
    assert!(lo.abs_diff_eq(vlo, 1e-4) && hi.abs_diff_eq(vhi, 1e-4), "{lo} {hi} against {vlo} {vhi}");
}

#[test]
fn a_project_with_an_fbx_character_loads_it_as_a_body() {
    omuzaki_io_fbx::link();
    let (_, _, bytes) = both();
    let store = MemStore::new();
    let hash = store.put(&bytes, kit::provenance()).expect("stored");
    let def = AvatarDef::new(AvatarSource::new(hash, SourceFormat::Fbx, "kit.fbx"));
    let doc =
        MemDoc::from_value(json!({ "characters": { "kit_fbx": { "avatar": serde_json::to_value(&def).unwrap() } } }));
    let build = BuildServices::default();
    let clock = FakeClock::default();
    let services = BodyServices::new(&RegistryBuilder, &build, &store, MtoonSupport::NONE, &clock);
    let id = omuzaki_char_api::CharId::new("kit_fbx").unwrap();
    let body = load_body(&doc, &services, &id, None, BuildQuality::Live).expect("the FBX character loads");
    assert!(matches!(body, Body::Avatar(_)), "an imported body");
    assert!(body.landmarks().expect("measures").body.height() > 0.5, "the character stands");
}

/// On the GPU: the FBX copy covers the same pixels as the VRM copy.
#[cfg(feature = "wgpu")]
#[test]
fn on_the_gpu_the_fbx_kit_has_the_vrm_kits_silhouette() {
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    use omuzaki_render_api::{
        AlphaEncoding, Background, Camera, Colour, Light, Projection, Quality, Renderer, Scene, TargetDesc,
    };
    use omuzaki_stage::avatar::AvatarStage;
    use omuzaki_stage::frame::{FrameInput, StageClock};

    let (vrm, read, _) = both();
    let size = omuzaki_core::glam::UVec2::new(192, 256);
    let mut renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.6, 2.2),
        Vec3::new(0.0, 0.6, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    ));
    scene.background = Background::Solid { colour: Colour::TRANSPARENT };
    scene.lights.push(Light::Directional {
        to_light: Vec3::new(0.3, 0.8, 1.0),
        colour: Colour::WHITE,
        intensity: 2.5,
        casts_shadow: false,
    });
    let mut images = Vec::new();
    for (name, avatar) in [
        ("vrm", avatar(&vrm, SourceFormat::Vrm1, "kit.vrm")),
        ("fbx", avatar(&read.scene, SourceFormat::Fbx, "kit.fbx")),
    ] {
        let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight }).unwrap();
        let bind = avatar.bind;
        let mut stage =
            AvatarStage::new(&mut renderer, avatar, scene.clone(), target, Quality::offline(), StageClock::fixed(60.0))
                .expect("stages");
        stage.tick(&mut renderer, &FrameInput { pose: bind, ..FrameInput::default() }).expect("renders");
        let image = renderer.read_target(target).expect("reads");
        if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
            let png = omuzaki_stage::ops::render::still::encode_png(&image).expect("encodes");
            std::fs::write(std::path::Path::new(&dir).join(format!("fbx-avatar-{name}.png")), png).expect("writes");
        }
        images.push(image);
        stage.release(&mut renderer).unwrap();
        renderer.release(target.into()).unwrap();
    }
    let covered = |rgba: &[u8]| rgba.as_chunks::<4>().0.iter().map(|p| p[3] > 127).collect::<Vec<_>>();
    let (a, b) = (covered(&images[0].rgba8), covered(&images[1].rgba8));
    let both_px = a.iter().zip(&b).filter(|(x, y)| **x && **y).count();
    let either = a.iter().zip(&b).filter(|(x, y)| **x || **y).count();
    assert!(either > 1000, "the character is in frame: {either} pixels");
    let iou = both_px as f64 / either as f64;
    // Not 1: the VRM's MToon materials draw a one-pixel outline, which FBX has no way to carry. Every pixel the FBX copy
    // covers, the VRM copy covers too.
    assert!(iou > 0.98, "the FBX copy covers the VRM copy's pixels: intersection over union {iou:.4}");
    let extra = a.iter().zip(&b).filter(|(x, y)| !**x && **y).count();
    assert_eq!(extra, 0, "the FBX copy covers nothing the VRM copy leaves empty");
    println!("silhouette: intersection over union {iou:.4}");
    // And looks the same: the mean colour difference over the covered pixels is small.
    let diff: u64 = images[0]
        .rgba8
        .as_chunks::<4>()
        .0
        .iter()
        .zip(images[1].rgba8.as_chunks::<4>().0)
        .filter(|(p, q)| p[3] > 127 && q[3] > 127)
        .map(|(p, q)| p[..3].iter().zip(&q[..3]).map(|(x, y)| u64::from(x.abs_diff(*y))).sum::<u64>())
        .sum();
    let mean = diff as f64 / (both_px as f64 * 3.0);
    println!("colour: mean difference {mean:.2} of 255 over {both_px} pixels");
    assert!(mean < 6.0, "the same colours: mean difference {mean:.2} of 255");
}
