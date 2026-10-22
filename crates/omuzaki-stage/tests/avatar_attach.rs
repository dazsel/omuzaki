//! Attachments on an imported avatar (`docs/design/byom.md` D1, condition 2; #1385): the anchors derived from its
//! skeleton, a kit part built against them through the kit's own `Attach` path, and the part following its bone. The
//! model is the kit's own mock exported to VRM 1.0; no third-party file.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "avatar/kit.rs"]
mod kit;

use std::path::Path;

use omuzaki_anim_api::{HumanoidBone, HumanoidFrames, HumanoidPose, Pose, Transform};
use omuzaki_avatar_api::anchors::{GRIP_PAST_WRIST, HEAD_TOP, LEFT_GRIP, RIGHT_GRIP, bind_matrices, bone_anchor};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_char_api::{AnchorId, BuildServices, CharId, JointRef, Quality as BuildQuality, RegistryBuilder};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{ErrorCode, OpError};
use omuzaki_render_api::MtoonSupport;
use omuzaki_stage::body::{Body, BodyServices, load_body};
use omuzaki_store_api::Store;
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

/// The kit's mock character as a person's VRM, imported as `kit_vrm` with `parts` as its attachments.
fn load_with(parts: &Value) -> Result<ImportedAvatar, OpError> {
    let built = kit::kit();
    let store = MemStore::new();
    let hash = store.put(&kit::vrm_bytes(&built), kit::provenance()).expect("stored");
    let mut node =
        serde_json::to_value(AvatarDef::new(AvatarSource::new(hash, SourceFormat::Vrm1, "kit.vrm"))).unwrap();
    node["parts"] = parts.clone();
    let doc = MemDoc::from_value(json!({ "characters": { "kit_vrm": { "avatar": node } } }));
    let (services_build, clock) = (BuildServices::default(), FakeClock::default());
    let services = BodyServices::new(&RegistryBuilder, &services_build, &store, MtoonSupport::NONE, &clock);
    match load_body(&doc, &services, &CharId::new("kit_vrm").unwrap(), None, BuildQuality::Live)? {
        Body::Avatar(avatar) => Ok(*avatar),
        Body::Kit(_) => panic!("an imported character loads as an avatar"),
    }
}

/// A halo with no motion of its own, on the avatar's head-top.
fn halo() -> Value {
    json!({ "halo": {
        "kind": "halo",
        "layer": 50,
        "attach": { "to": "anchor", "part": "avatar", "anchor": HEAD_TOP },
        "params": { "bob": false },
    }})
}

fn anchor(avatar: &ImportedAvatar, name: &str) -> omuzaki_char_api::Anchor {
    avatar.anchors.get(&AnchorId::new(name).unwrap()).unwrap_or_else(|| panic!("no anchor {name}")).clone()
}

/// Every bone's model-space transform with the humanoid bones at `pose`.
fn world_at(avatar: &ImportedAvatar, pose: &HumanoidPose) -> Pose {
    let rest: Vec<Transform> = avatar.skeleton.bones().iter().map(|b| b.rest).collect();
    let mut local = Pose::IDENTITY;
    HumanoidFrames::new(&avatar.skeleton).apply_humanoid(&rest, pose, &mut local).unwrap();
    let mut world = Pose::IDENTITY;
    avatar.skeleton.world_pose(&local, &mut world);
    world
}

#[test]
fn every_humanoid_bone_has_an_anchor_and_the_head_top_and_grips_sit_where_they_should() {
    let avatar = load_with(&json!({})).unwrap();
    let map = avatar.skeleton.humanoid_map().expect("a humanoid");
    for (bone, _) in map.iter() {
        let a = anchor(&avatar, &bone_anchor(bone));
        assert_eq!(a.bone, JointRef::Humanoid(bone));
    }
    // The head-top is the model's own crown, from its meshes: at or over the top the kit's head published for the
    // character the file was exported from (its cranium), by no more than its hair.
    let (head, top) = (anchor(&avatar, "head"), anchor(&avatar, HEAD_TOP));
    assert_eq!(top.bone, JointRef::Humanoid(HumanoidBone::Head));
    let kit_top = kit::kit().anchors.iter().find(|((_, n), _)| n.as_str() == "top").map(|(_, a)| a.position).unwrap();
    println!("derived head-top {} vs the kit head's top {kit_top}, head joint {}", top.position, head.position);
    assert!((-0.005..0.08).contains(&(top.position.y - kit_top.y)), "{} vs {kit_top}", top.position);
    assert!(top.position.x.abs() < 1e-3, "on the centre line: {}", top.position);
    // The mock has neither eyes nor fingers, so the grips take the fallback (the finger rule is avatar-api's unit test):
    // past the wrist along the forearm.
    for (grip, hand, forearm) in
        [(LEFT_GRIP, "left-hand", "left-lower-arm"), (RIGHT_GRIP, "right-hand", "right-lower-arm")]
    {
        let (g, h, f) = (anchor(&avatar, grip), anchor(&avatar, hand), anchor(&avatar, forearm));
        assert_eq!(g.bone, h.bone);
        let expected = h.position + (h.position - f.position) * GRIP_PAST_WRIST;
        assert!(g.position.distance(expected) < 1e-6, "{grip}: {} vs {expected}", g.position);
    }
}

#[test]
fn an_accessory_attaches_to_the_head_and_follows_a_head_turn() {
    let bare = load_with(&json!({})).unwrap();
    let avatar = load_with(&halo()).unwrap();
    assert!(avatar.meshes.len() > bare.meshes.len(), "the halo's meshes join the avatar's");
    let added = &avatar.meshes[bare.meshes.len()..];
    let head = JointRef::Humanoid(HumanoidBone::Head);
    for mesh in added {
        assert!(mesh.mesh.material.as_str().starts_with("halo"), "{}", mesh.mesh.material);
        assert_eq!(mesh.mesh.joint_bones, std::slice::from_ref(&head));
        assert!(avatar.materials.contains_key(&mesh.mesh.material));
    }

    // The ring floats over the head-top, as the halo's own `height` puts it.
    let top = anchor(&avatar, HEAD_TOP);
    let positions = added.iter().flat_map(|m| m.mesh.geometry.positions.iter().copied());
    let (min, max) = positions.fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(p), hi.max(p)));
    let centre = (min + max) * 0.5;
    assert!((centre.x - top.position.x).abs() < 1e-3 && (centre.z - top.position.z).abs() < 1e-3, "{centre} {top:?}");
    assert!((centre.y - top.position.y - 0.07).abs() < 1e-3, "{centre} is 7 cm over {}", top.position);

    // Turn the head 90 degrees about the vertical, in the normalised space a performer drives.
    let yaw = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
    let mut pose = avatar.bind;
    pose.set(HumanoidBone::Head, yaw * pose.get(HumanoidBone::Head));
    let world = world_at(&avatar, &pose);
    let bind = bind_matrices(&avatar.skeleton, &avatar.bind);
    let id = avatar.skeleton.humanoid_bone(HumanoidBone::Head).unwrap().index();
    let skin = world.local[id].to_mat4() * added[0].inverse_bind[0];
    assert!(skin.abs_diff_eq(world.local[id].to_mat4() * bind[id].inverse(), 0.0));
    // Expected from the normalised pose alone: the head turns by `yaw` in its parent's frame, about its own joint.
    let neck = anchor(&avatar, "neck").rotation;
    let turn = neck * yaw * neck.conjugate();
    let pivot = anchor(&avatar, "head").position;
    for (what, at) in [("the anchor", top.position), ("the ring's centre", centre)] {
        let expected = pivot + turn * (at - pivot);
        let moved = skin.transform_point3(at);
        assert!(moved.abs_diff_eq(expected, 1e-5), "{what}: {moved} vs {expected}");
    }
    let rise = top.position - pivot;
    assert!(rise.y > 0.0 && (skin.transform_point3(top.position) - pivot).y > 0.0, "the top stays over the head");
}

#[test]
fn a_part_with_bones_of_its_own_or_the_reserved_id_is_refused_with_what_to_do() {
    let mut bobbing = halo();
    bobbing["halo"]["params"]["bob"] = json!(true);
    let error = load_with(&bobbing).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.message.contains("halo") && error.message.contains("bone"), "{error:?}");
    assert!(error.hint.as_deref().is_some_and(|h| h.contains("bob")), "{error:?}");

    let reserved = json!({ "avatar": halo()["halo"].clone() });
    let error = load_with(&reserved).unwrap_err();
    assert!(error.message.contains("the model itself"), "{error:?}");
}

#[test]
fn there_is_no_attachment_code_outside_the_kits_attach_path() {
    // Placing a part is the kit's `Attach` resolution; the avatar side only feeds it joints and anchors.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut offenders = Vec::new();
    for dir in ["omuzaki-stage/src", "omuzaki-avatar-api/src", "omuzaki-avatar/src", "omuzaki-io-ops/src"] {
        scan(&root.join(dir), &mut offenders);
    }
    assert!(offenders.is_empty(), "attachment placement outside omuzaki-char: {offenders:#?}");
}

#[allow(clippy::disallowed_methods)] // a test reading this repository's own sources, not an op reaching a person's files
fn scan(dir: &Path, offenders: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan(&path, offenders);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for token in ["Attach::Bone", "Attach::Anchor", "AttachOffset", "ResolvedAttach", "to_bind"] {
                if text.contains(token) {
                    offenders.push(format!("{} uses {token}", path.display()));
                }
            }
        }
    }
}

/// On the GPU: the halo floats over the imported model's own crown and tilts with its head. With
/// `OMUZAKI_EVIDENCE_DIR` set, the frames are written there.
#[cfg(feature = "wgpu")]
#[test]
fn on_the_gpu_the_halo_floats_over_the_crown_and_tilts_with_the_head() {
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    use omuzaki_render_api::{
        AlphaEncoding, Background, Camera, Colour, Light, Projection, Quality, Renderer, Scene, TargetDesc,
    };
    use omuzaki_stage::avatar::AvatarStage;
    use omuzaki_stage::frame::{FrameInput, StageClock};

    let size = omuzaki_core::glam::UVec2::new(256, 320);
    let mut renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.62, 2.4),
        Vec3::new(0.0, 0.62, 0.0),
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
    let width = size.x as usize;
    // The highest covered row: 0 is the top of the frame.
    let top_row = |rgba: &[u8]| rgba.as_chunks::<4>().0.iter().position(|p| p[3] > 127).map(|i| i / width);
    // The mean column of the covered pixels above `row`: where the halo is across the frame.
    #[allow(clippy::cast_precision_loss)] // pixel counts
    let column_above = |rgba: &[u8], row: usize| {
        let (sum, n) = rgba.as_chunks::<4>().0[..row * width]
            .iter()
            .enumerate()
            .filter(|(_, p)| p[3] > 127)
            .fold((0usize, 0usize), |(sum, n), (i, _)| (sum + i % width, n + 1));
        sum as f32 / n.max(1) as f32
    };
    let mut images = Vec::new();
    for (name, parts, tilted) in [("bare", json!({}), false), ("halo", halo(), false), ("halo-tilted", halo(), true)] {
        let avatar = load_with(&parts).unwrap();
        let mut pose = avatar.bind;
        if tilted {
            // The head leans 25 degrees to the model's left (the viewer's right).
            let lean = Quat::from_rotation_z(-0.436);
            pose.set(HumanoidBone::Head, lean * pose.get(HumanoidBone::Head));
        }
        let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight }).unwrap();
        let mut stage =
            AvatarStage::new(&mut renderer, avatar, scene.clone(), target, Quality::offline(), StageClock::fixed(60.0))
                .expect("stages");
        stage.tick(&mut renderer, &FrameInput { pose, ..FrameInput::default() }).expect("renders");
        let image = renderer.read_target(target).expect("reads");
        if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
            let png = omuzaki_stage::ops::render::still::encode_png(&image).expect("encodes");
            #[allow(clippy::disallowed_methods)] // evidence for a person to look at, written where they asked
            std::fs::write(Path::new(&dir).join(format!("avatar-attach-{name}.png")), png).expect("writes");
        }
        images.push(image.rgba8);
        stage.release(&mut renderer).unwrap();
        renderer.release(target.into()).unwrap();
    }
    let tops: Vec<usize> = images.iter().map(|i| top_row(i).expect("the character is in frame")).collect();
    let bare_top = tops[0];
    let (upright, leaning) = (column_above(&images[1], bare_top), column_above(&images[2], bare_top));
    println!("top rows {tops:?}; the halo's mean column {upright:.1} upright, {leaning:.1} with the head leaning");
    // The ring, 7 cm over the crown, shows about 20 rows above the bare model here.
    assert!(tops[1] + 10 <= bare_top, "the halo shows over the head: {tops:?}");
    assert!((upright - size.as_vec2().x / 2.0).abs() < 3.0, "centred over the upright head: {upright}");
    assert!(leaning > upright + 10.0, "and it goes with the head as it leans: {upright} then {leaning}");
}
