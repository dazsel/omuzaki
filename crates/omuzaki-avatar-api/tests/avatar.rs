//! `ImportedAvatar`: one test per `validate` error, `driven` per gaze route and partial look sets, `texture_bytes`.

mod common;

use common::{avatar, ix, skin};
use omuzaki_anim_api::{
    BoneId, Channel, ChannelMask, Collider, ColliderGroup, ColliderShape, HumanoidBone, SpringChain, SpringParams,
    SpringSet,
};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::avatar::texture_bytes;
use omuzaki_avatar_api::error::AvatarError;
use omuzaki_avatar_api::expression::{ColourBind, ExprIx, MorphBind, UvBind};
use omuzaki_avatar_api::gaze::{AvatarGaze, AvatarLookAt};
use omuzaki_avatar_api::retained::{Retained, RetainedMaterial};
use omuzaki_avatar_api::retained::{
    RetainedAnnotation, RetainedCollider, RetainedColliderGroup, RetainedFirstPerson, RetainedGeometry, RetainedMesh,
    RetainedNode, RetainedPrimitive, RetainedSkin, RetainedSpringChain, RetainedSpringJoint,
};
use omuzaki_char_api::{BoneName, FaceOverlay, JointRef, MaterialName, TextureData, TextureName};
use omuzaki_core::glam::Vec3;
use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec4};
use omuzaki_core::id::Slug;
use omuzaki_face_api::{FaceLayerId, PlanarWindow};
use omuzaki_io_api::{ColorBindKind, LookAtKind, RangeMap};
use omuzaki_io_api::{Material, Shading};
use omuzaki_render_api::{AlphaMode, MorphSet, SparseMorph, TextureDesc};

#[test]
fn a_well_formed_avatar_validates() {
    let mut a = avatar(true, 2, &["happy", "jawOpen"]);
    a.drivers.set(Channel::JAW_OPEN, Some(ix(&a, "jawOpen")));
    assert_eq!(a.validate(), Ok(()));
}

#[test]
fn an_unresolved_bone_reference_is_refused() {
    let mut a = avatar(false, 0, &[]);
    a.meshes[0].mesh.joint_bones = vec![JointRef::Named(BoneName::new("tail").unwrap())];
    assert!(matches!(a.validate(), Err(AvatarError::UnresolvedBone { joint: JointRef::Named(_), .. })));
    // The eyes are optional humanoid bones: this skeleton has none.
    let mut a = avatar(false, 0, &[]);
    a.meshes[0].mesh.joint_bones = vec![JointRef::Humanoid(omuzaki_anim_api::HumanoidBone::LeftEye)];
    let e = a.validate().unwrap_err();
    assert!(e.to_string().contains("mesh 0"), "{e}");
}

#[test]
fn inverse_binds_must_match_the_joint_list() {
    let mut a = avatar(false, 0, &[]);
    a.meshes[0].inverse_bind.clear();
    assert_eq!(a.validate(), Err(AvatarError::inverse_bind_length(0, 0, 1)));
}

#[test]
fn a_morph_bind_out_of_range_is_refused() {
    let mut a = avatar(false, 2, &["happy"]);
    a.expressions[0].morph.push(MorphBind::new(0, 2, 1.0));
    assert_eq!(a.validate(), Err(AvatarError::morph_out_of_range(0, 0, 2)));
    let mut a = avatar(false, 2, &["happy"]);
    a.expressions[0].morph = vec![MorphBind::new(1, 0, 1.0)];
    assert_eq!(a.validate(), Err(AvatarError::morph_out_of_range(0, 1, 0)), "a mesh that does not exist");
    let mut a = avatar(false, 0, &["happy"]);
    a.expressions[0].morph = vec![MorphBind::new(0, 0, 1.0)];
    assert_eq!(a.validate(), Err(AvatarError::morph_out_of_range(0, 0, 0)), "a mesh without morphs");
}

#[test]
fn morph_targets_must_fit_their_own_mesh() {
    // Targets made for a 10-vertex mesh, moving vertex 5, on the 3-vertex test mesh: the renderer would refuse it.
    let target = |i: u32| SparseMorph {
        name: Slug::new("wide").unwrap(),
        indices: vec![i],
        positions: vec![Vec3::Z],
        normals: Vec::new(),
    };
    let mut a = avatar(false, 0, &[]);
    a.meshes[0].morphs = Some(MorphSet::from_targets(10, vec![target(5)]).unwrap());
    assert!(matches!(a.validate(), Err(AvatarError::Morphs { mesh: 0, .. })), "{:?}", a.validate());
    a.meshes[0].morphs = Some(MorphSet::from_targets(10, vec![target(2)]).unwrap());
    assert_eq!(a.validate(), Ok(()), "the same targets with an index inside the mesh fit");
}

#[test]
fn materials_and_textures_are_checked_as_upload_will_check_them() {
    let skin = MaterialName::new("skin").unwrap();
    let mut a = avatar(false, 0, &[]);
    a.materials.get_mut(&skin).unwrap().alpha = AlphaMode::Mask { cutoff: 2.0 };
    assert!(matches!(a.validate(), Err(AvatarError::Material { .. })), "a renderer rule: {:?}", a.validate());

    let atlas = TextureName::new("atlas").unwrap();
    let mut a = avatar(false, 0, &[]);
    a.materials.get_mut(&skin).unwrap().base_texture = Some(atlas.clone());
    assert_eq!(a.validate(), Err(AvatarError::unknown_texture(skin.clone(), atlas.clone())));
    a.textures.insert(atlas.clone(), texture(4, false));
    assert!(matches!(a.validate(), Err(AvatarError::Texture { .. })), "no pixels for a 4x4: {:?}", a.validate());
    a.textures.get_mut(&atlas).unwrap().rgba8 = vec![0; 4 * 4 * 4];
    assert_eq!(a.validate(), Ok(()), "with its pixels the texture uploads");

    let mut a = avatar(false, 0, &[]);
    let window = PlanarWindow { min: Vec2::ZERO, max: Vec2::ONE, fade_from: 0.0, fade_to: 0.1 };
    let overlay = FaceOverlay { layer: FaceLayerId::Base, window, strength: 1.0 };
    a.materials.get_mut(&skin).unwrap().overlays.push(overlay);
    assert_eq!(a.validate(), Err(AvatarError::face_overlay(skin)), "an imported avatar has no painted face");
}

/// A change that breaks one index of a retained graph.
type Breaks = fn(&mut Retained);

/// A retained graph with one source mesh per entry of `sources` (its name, the runtime meshes its primitives became and
/// its target names), each drawn by a node of its own: node `i` draws mesh `i`.
fn sources(sources: &[(&str, &[u16], &[&str])]) -> Retained {
    let mut r = Retained::new();
    for (i, (name, runtime, names)) in (0u32..).zip(sources) {
        r.nodes.push(RetainedNode::new(*name).with_mesh(i, None));
        let names = names.iter().map(|n| (*n).to_owned()).collect();
        let mesh = RetainedMesh::new(*name, vec![RetainedPrimitive::new(); runtime.len()]);
        r.meshes.push(mesh.with_instance(i, runtime.to_vec()).with_morph_target_names(names));
    }
    r
}

/// One source mesh of one primitive, drawn by two nodes: node 0 made runtime mesh 0 and node 1 made runtime mesh 1.
fn instanced() -> Retained {
    let mut r = Retained::new();
    r.nodes = vec![RetainedNode::new("Hand.L").with_mesh(0, None), RetainedNode::new("Hand.R").with_mesh(0, None)];
    r.meshes = vec![
        RetainedMesh::new("Glove", vec![RetainedPrimitive::new()]).with_instance(0, vec![0]).with_instance(1, vec![1]),
    ];
    r
}

/// A spring set of one chain, `hair`, over the hips and spine, colliding with the group `head`.
fn hair_springs(a: &ImportedAvatar) -> SpringSet {
    let bone = |b| a.skeleton.humanoid_bone(b).unwrap_or(BoneId(u16::MAX));
    let mut chain =
        SpringChain::new("hair", &[bone(HumanoidBone::Hips), bone(HumanoidBone::Spine)], SpringParams::default());
    chain.collider_groups = vec!["head".to_owned()];
    let sphere = ColliderShape::Sphere { offset: Vec3::ZERO, radius: 0.1 };
    let group = ColliderGroup {
        name: "head".to_owned(),
        colliders: vec![Collider { bone: bone(HumanoidBone::Head), shape: sphere }],
    };
    SpringSet { chains: vec![chain], groups: vec![group] }
}

#[test]
fn every_index_of_the_retained_scene_graph_is_checked() {
    let mut a = avatar(false, 0, &[]);
    let r = &mut a.retained;
    r.nodes = vec![RetainedNode::new("Root").with_children(vec![1]), RetainedNode::new("Face").with_mesh(0, Some(0))];
    r.roots = vec![0];
    r.meshes =
        vec![RetainedMesh::new("Face", vec![RetainedPrimitive::new().with_material(0)]).with_instance(1, vec![0])];
    r.skins = vec![RetainedSkin::new("skin", vec![0, 1], vec![Mat4::IDENTITY; 2]).with_skeleton(0)];
    r.materials = vec![RetainedMaterial::new(skin(), Material::new("Skin", Shading::Unlit))];
    let sphere = ColliderShape::Sphere { offset: Vec3::ZERO, radius: 0.1 };
    r.springs.colliders = vec![RetainedCollider::new(1, sphere)];
    r.springs.groups = vec![RetainedColliderGroup::new("", vec![0])];
    let joints = vec![
        RetainedSpringJoint::new(0, SpringParams::default()),
        RetainedSpringJoint::new(1, SpringParams::default()),
    ];
    r.springs.chains = vec![RetainedSpringChain::new("", joints).with_groups(vec![0]).with_center(0)];
    assert_eq!(a.validate(), Ok(()), "a consistent graph");

    let broken: [(&str, Breaks); 15] = [
        ("node 0's child", |r| r.nodes[0].children = vec![2]),
        ("node 1's mesh", |r| r.nodes[1].mesh = Some(1)),
        ("node 1's skin", |r| r.nodes[1].skin = Some(1)),
        ("node 0's bone", |r| r.nodes[0].bone = Some(BoneId(999))),
        ("a root", |r| r.roots = vec![5]),
        ("skin 0's joint", |r| r.skins[0].joints = vec![0, 7]),
        ("skin 0's joint", |r| r.skins[0].skeleton = Some(9)),
        ("mesh 0's material", |r| r.meshes[0].primitives[0].material = Some(3)),
        ("mesh 0's instance node", |r| r.meshes[0].instances[0].node = 7),
        ("spring collider 0's node", |r| r.springs.colliders[0].node = 2),
        ("collider group 0's collider", |r| r.springs.groups[0].colliders = vec![0, 1]),
        ("spring chain 0's joint", |r| r.springs.chains[0].joints[1].node = 2),
        ("spring chain 0's collider group", |r| r.springs.chains[0].groups = vec![1]),
        ("spring chain 0's center", |r| r.springs.chains[0].center = Some(5)),
        ("a first-person annotation's node", |r| {
            r.first_person = Some(vec![RetainedAnnotation::new(4, RetainedFirstPerson::Auto)]);
        }),
    ];
    for (at, break_it) in broken {
        let mut b = a.clone();
        break_it(&mut b.retained);
        let e = b.validate().unwrap_err();
        assert!(matches!(&e, AvatarError::RetainedIndex { at: got, .. } if got == at), "{at}: {e:?}");
    }
    let mut b = a.clone();
    b.retained.first_person = Some(vec![RetainedAnnotation::new(1, RetainedFirstPerson::ThirdPersonOnly)]);
    assert_eq!(b.validate(), Ok(()), "an annotation on an existing node");
    b.retained.first_person = Some(Vec::new());
    assert_eq!(b.validate(), Ok(()), "an empty firstPerson block is kept as such");

    let mut b = a.clone();
    b.expressions = vec![omuzaki_avatar_api::expression::AvatarExpression::new(common::name("happy"))];
    b.meshes[0] = common::head_mesh(1);
    b.expressions[0].morph.push(MorphBind::new(0, 0, 1.0).with_node(1));
    assert_eq!(b.validate(), Ok(()), "a bind naming the instance it moves");
    b.expressions[0].morph[0] = MorphBind::new(0, 0, 1.0).with_node(9);
    let e = b.validate().unwrap_err();
    assert!(matches!(&e, AvatarError::RetainedIndex { at, .. } if at == "expression 0's morph bind node"), "{e:?}");

    let mut b = a.clone();
    b.retained.skins[0].inverse_bind.pop();
    assert_eq!(b.validate(), Err(AvatarError::skin_inverse_bind(0, 1, 2)));
    b.retained.skins[0].inverse_bind.clear();
    assert_eq!(b.validate(), Ok(()), "no inverse binds in the file means identity");
}

#[test]
fn retained_source_meshes_group_every_mesh_once_and_name_its_targets() {
    const NAMES: &[&str] = &["Fcl_ALL_A", "Fcl_EYE_Close"];
    let mut a = avatar(false, 2, &[]);
    // A second runtime mesh, from the same source mesh's second primitive: the two share one name table.
    a.meshes.push(a.meshes[0].clone());
    a.retained = sources(&[("Face", &[0, 1], NAMES)]);
    assert_eq!(a.validate(), Ok(()), "two primitives of one mesh share its names");

    a.retained = sources(&[("Face", &[0], NAMES)]);
    assert_eq!(a.validate(), Err(AvatarError::mesh_grouping(1, 0)), "a mesh from no source mesh");
    a.retained = sources(&[("Face", &[0, 1], &[]), ("Body", &[1], &[])]);
    assert_eq!(a.validate(), Err(AvatarError::mesh_grouping(1, 2)), "a mesh from two");
    a.retained = sources(&[("Face", &[0, 1, 2], &[])]);
    assert_eq!(a.validate(), Err(AvatarError::unknown_primitive(0, 2)), "a mesh that isn't there");

    a.retained = sources(&[("Face", &[0, 1], &["Fcl_ALL_A"])]);
    assert_eq!(a.validate(), Err(AvatarError::morph_names(0, 1, 2)), "a name missing");
    a.retained = sources(&[("Face", &[0], &[]), ("Hair", &[1], &[])]);
    assert_eq!(a.validate(), Ok(()), "a file that names no targets");

    // A primitive without targets beside one with them, under one name table (glTF allows it).
    a.meshes[1].morphs = None;
    a.retained = sources(&[("Face", &[0, 1], NAMES)]);
    assert_eq!(a.validate(), Ok(()));
    a.retained = sources(&[("Face", &[0], &[]), ("Hair", &[1], NAMES)]);
    assert_eq!(a.validate(), Err(AvatarError::morph_names(1, 2, 0)), "names for a mesh with no targets");

    a.retained = Retained::new();
    assert_eq!(a.validate(), Ok(()), "nothing kept is fine for a hand-built avatar");
}

#[test]
fn a_mesh_drawn_by_two_nodes_is_one_runtime_mesh_per_node() {
    let mut a = avatar(false, 1, &["happy"]);
    a.meshes.push(a.meshes[0].clone());
    a.expressions[0].morph = vec![MorphBind::new(0, 0, 0.25).with_node(0), MorphBind::new(1, 0, 0.75).with_node(1)];
    a.retained = instanced();
    assert_eq!(a.validate(), Ok(()), "each node's instance is its own runtime mesh, and each bind names its own");

    let mut b = a.clone();
    b.expressions[0].morph[1] = MorphBind::new(1, 0, 0.75).with_node(0);
    assert_eq!(b.validate(), Err(AvatarError::morph_bind_instance(0, 1, 0)), "a bind naming the other instance");

    let mut b = a.clone();
    b.retained.meshes[0].instances[1].meshes = vec![0, 1];
    assert_eq!(b.validate(), Err(AvatarError::instance_primitives(0, 1, 2, 1)), "one runtime mesh per primitive");
    let mut b = a.clone();
    b.retained.meshes[0].instances.pop();
    assert_eq!(b.validate(), Err(AvatarError::node_instances(1, 0, 0)), "a drawing node with no instance");
    let mut b = a.clone();
    b.retained.meshes[0].instances[1].node = 0;
    assert_eq!(b.validate(), Err(AvatarError::node_instances(0, 0, 2)), "a node listed twice");
    let mut b = a.clone();
    b.retained.nodes[1].mesh = None;
    assert_eq!(b.validate(), Err(AvatarError::instance_node(0, 1)), "an instance at a node that draws nothing");
    let mut b = a.clone();
    b.retained.meshes[0].instances[1].meshes = vec![0];
    assert_eq!(b.validate(), Err(AvatarError::mesh_grouping(0, 2)), "two instances sharing one runtime mesh");
}

#[test]
fn retained_spring_records_keep_the_files_shape_and_name_runtime_entries_once() {
    let mut a = avatar(false, 0, &[]);
    a.springs = hair_springs(&a);
    a.retained.nodes = vec![RetainedNode::new("Hips"), RetainedNode::new("Spine"), RetainedNode::new("Head")];
    let sphere = ColliderShape::Sphere { offset: Vec3::ZERO, radius: 0.1 };
    let plane = ColliderShape::Plane { offset: Vec3::ZERO, normal: Vec3::Y };
    let joints = || {
        vec![RetainedSpringJoint::new(0, SpringParams::default()), RetainedSpringJoint::new(1, SpringParams::default())]
    };
    let s = &mut a.retained.springs;
    // Collider 0 is shared by two groups, collider 1 is in none, and both groups and both chains share a name.
    s.colliders = vec![RetainedCollider::new(2, sphere), RetainedCollider::new(2, plane)];
    s.groups =
        vec![RetainedColliderGroup::new("", vec![0]).with_runtime("head"), RetainedColliderGroup::new("", vec![0])];
    s.chains = vec![
        RetainedSpringChain::new("Hair", joints()).with_groups(vec![0, 1]).with_runtime("hair"),
        RetainedSpringChain::new("Hair", vec![RetainedSpringJoint::new(2, SpringParams::default())]),
    ];
    assert_eq!(a.validate(), Ok(()), "shared and ungrouped colliders, repeated names, a chain the solver can't run");

    let mut b = a.clone();
    b.retained.springs.chains[1].runtime = Some("hair".to_owned());
    assert_eq!(b.validate(), Err(AvatarError::shared_runtime_spring("chain", "hair")));
    let mut b = a.clone();
    b.retained.springs.chains[0].runtime = Some("fringe".to_owned());
    assert_eq!(b.validate(), Err(AvatarError::unknown_runtime_spring("chain", "fringe")));
    let mut b = a.clone();
    b.retained.springs.groups[1].runtime = Some("head".to_owned());
    assert_eq!(b.validate(), Err(AvatarError::shared_runtime_spring("collider group", "head")));
    let mut b = a.clone();
    b.retained.springs.groups[0].runtime = Some("body".to_owned());
    assert_eq!(b.validate(), Err(AvatarError::unknown_runtime_spring("collider group", "body")));
}

#[test]
fn a_driver_out_of_range_is_refused() {
    let mut a = avatar(false, 1, &["happy"]);
    a.drivers.set(Channel::CUSTOM_00, Some(ExprIx(1)));
    assert_eq!(a.validate(), Err(AvatarError::driver_out_of_range(Channel::CUSTOM_00, ExprIx(1), 1)));
}

#[test]
fn a_gaze_driver_is_refused_off_the_native_route() {
    let mut a = avatar(true, 1, &["eyeLookUpLeft"]);
    a.drivers.set(Channel::EYE_LOOK_UP_LEFT, Some(ExprIx(0)));
    a.gaze = AvatarGaze::Bone;
    assert_eq!(a.validate(), Err(AvatarError::gaze_driver_off_route(Channel::EYE_LOOK_UP_LEFT)));
    a.gaze = AvatarGaze::NativeArkit;
    assert_eq!(a.validate(), Ok(()));
}

#[test]
fn binds_and_meshes_must_name_existing_materials() {
    let other = MaterialName::new("other").unwrap();
    let mut a = avatar(false, 0, &["happy"]);
    a.expressions[0].colour.push(ColourBind::new(other.clone(), ColorBindKind::Color, Vec4::ONE, Vec4::ZERO));
    assert_eq!(a.validate(), Err(AvatarError::unknown_material("expression 0 (happy)", other.clone())));
    let mut a = avatar(false, 0, &["happy"]);
    a.expressions[0].uv.push(UvBind::new(other.clone(), Vec2::ONE, Vec2::ZERO));
    assert!(matches!(a.validate(), Err(AvatarError::UnknownMaterial { .. })));
    let mut a = avatar(false, 0, &[]);
    a.meshes[0].mesh.material = other.clone();
    assert_eq!(a.validate(), Err(AvatarError::unknown_material("mesh 0", other)));
}

#[test]
fn springs_must_fit_the_skeleton() {
    let mut a = avatar(false, 0, &[]);
    a.springs = SpringSet {
        chains: vec![SpringChain::new("hair", &[BoneId(900), BoneId(901)], SpringParams::default())],
        ..SpringSet::default()
    };
    assert!(matches!(a.validate(), Err(AvatarError::Springs(_))));
}

/// The gaze part of what the avatar drives.
fn gaze(a: &omuzaki_avatar_api::avatar::ImportedAvatar) -> ChannelMask {
    a.driven() & ChannelMask::GAZE
}

fn channels(list: &[Channel]) -> ChannelMask {
    ChannelMask::from_channels(list)
}

#[test]
fn expression_drivers_and_the_skeleton_reach_the_model() {
    let mut a = avatar(false, 2, &["jawOpen", "happy"]);
    a.drivers.set(Channel::JAW_OPEN, Some(ix(&a, "jawOpen")));
    a.drivers.set(Channel::CUSTOM_00, Some(ix(&a, "happy")));
    let driven = a.driven();
    assert!(driven.contains(Channel::JAW_OPEN) && driven.contains(Channel::CUSTOM_00));
    assert!(!driven.contains(Channel::CHEEK_PUFF), "an undriven channel is inert");
    assert_eq!(driven & ChannelMask::HEAD, ChannelMask::HEAD, "the head moves through the skeleton");
    assert_eq!(driven & ChannelMask::BODY, ChannelMask::BODY, "the body moves through the skeleton");
    assert_eq!(gaze(&a), ChannelMask::EMPTY, "the inert route shows no gaze");
}

#[test]
fn the_bone_route_shows_every_direction_by_default() {
    let mut a = avatar(true, 0, &[]);
    a.gaze = AvatarGaze::Bone;
    assert_eq!(gaze(&a), ChannelMask::GAZE, "no lookAt in the file: the default maps have output");
    a.skeleton = common::skeleton(false);
    assert_eq!(gaze(&a), ChannelMask::EMPTY, "without eye bones nothing turns");
}

#[test]
fn a_bone_route_with_a_flat_vertical_map_gives_the_four_horizontal_channels() {
    let mut a = avatar(true, 0, &[]);
    a.gaze = AvatarGaze::Bone;
    let mut look = AvatarLookAt::default_for(LookAtKind::Bone);
    look.vertical_up.output_scale = 0.0;
    look.vertical_down.output_scale = 0.0;
    a.look_at = Some(look);
    assert_eq!(
        gaze(&a),
        channels(&[
            Channel::EYE_LOOK_IN_LEFT,
            Channel::EYE_LOOK_IN_RIGHT,
            Channel::EYE_LOOK_OUT_LEFT,
            Channel::EYE_LOOK_OUT_RIGHT
        ])
    );
}

#[test]
fn presets_show_only_the_directions_they_have() {
    let mut a = avatar(false, 1, &["lookUp"]);
    a.gaze = AvatarGaze::Presets;
    assert_eq!(gaze(&a), channels(&[Channel::EYE_LOOK_UP_LEFT, Channel::EYE_LOOK_UP_RIGHT]), "lookUp only: two");

    let mut a = avatar(false, 2, &["lookUp", "lookLeft"]);
    a.gaze = AvatarGaze::Presets;
    assert_eq!(
        gaze(&a),
        channels(&[
            Channel::EYE_LOOK_UP_LEFT,
            Channel::EYE_LOOK_UP_RIGHT,
            Channel::EYE_LOOK_OUT_LEFT,
            Channel::EYE_LOOK_IN_RIGHT
        ]),
        "lookUp and lookLeft: four"
    );

    let mut flat = AvatarLookAt::default_for(LookAtKind::Expression);
    flat.vertical_up = RangeMap { input_max_value: 90.0, output_scale: 0.0 };
    a.look_at = Some(flat);
    assert_eq!(
        gaze(&a),
        channels(&[Channel::EYE_LOOK_OUT_LEFT, Channel::EYE_LOOK_IN_RIGHT]),
        "a preset whose range map has no output cannot show"
    );
}

#[test]
fn the_native_route_counts_its_own_expressions_whatever_the_range_maps_say() {
    let mut a = avatar(false, 1, &["eyeLookUpLeft"]);
    a.gaze = AvatarGaze::NativeArkit;
    a.drivers.set(Channel::EYE_LOOK_UP_LEFT, Some(ix(&a, "eyeLookUpLeft")));
    let mut look = AvatarLookAt::default_for(LookAtKind::Bone);
    look.vertical_up.output_scale = 0.0;
    a.look_at = Some(look);
    assert_eq!(gaze(&a), channels(&[Channel::EYE_LOOK_UP_LEFT]));
    assert_eq!(a.validate(), Ok(()), "nothing to report");
}

#[test]
fn the_inert_route_shows_no_gaze_even_with_presets_and_eyes() {
    let mut a = avatar(true, 4, &["lookUp", "lookDown", "lookLeft", "lookRight"]);
    a.gaze = AvatarGaze::Inert;
    assert_eq!(gaze(&a), ChannelMask::EMPTY);
}

fn texture(side: u32, mips: bool) -> TextureData {
    let mut desc = TextureDesc::colour(UVec2::splat(side));
    desc.mips = mips;
    // Only the description counts; no pixels needed.
    TextureData { rgba8: Vec::new(), desc }
}

#[test]
fn export_keeps_whether_the_file_had_a_look_at() {
    let mut a = avatar(true, 0, &[]);
    a.gaze = AvatarGaze::Bone;
    assert_eq!(a.export().look_at, None, "absent in the file: absent for export");
    assert_eq!(a.face().look_at, AvatarLookAt::default_for(LookAtKind::Bone), "the face reads the defaults");
    let authored = AvatarLookAt::default_for(LookAtKind::Bone);
    a.look_at = Some(authored);
    assert_eq!(a.export().look_at, Some(&authored), "authored defaults stay authored");
}

#[test]
fn texture_bytes_count_rgba8_and_the_mip_chain() {
    assert_eq!(texture_bytes(&texture(2048, true)), 22_369_620);
    assert_eq!(texture_bytes(&texture(2048, false)), 16_777_216);
    assert_eq!(texture_bytes(&texture(1, true)), 4);
    let mut wide = texture(1, true);
    wide.desc.size = UVec2::new(4, 1);
    assert_eq!(texture_bytes(&wide), (4 + 2 + 1) * 4, "a non-square chain halves each side down to 1");

    let mut a = avatar(false, 0, &[]);
    for i in 0..20 {
        a.textures.insert(TextureName::new(format!("t{i}")).unwrap(), texture(2048, true));
    }
    assert_eq!(a.texture_bytes(), 447_392_400);
}

/// A valid triangle with `targets` morph targets of `deltas` position deltas each.
fn triangle(targets: usize, deltas: usize) -> RetainedGeometry {
    let mut p = omuzaki_io_api::Primitive::new(omuzaki_core::mesh::MeshData {
        positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
        normals: vec![Vec3::Z; 3],
        indices: vec![0, 1, 2],
        ..Default::default()
    });
    p.morph_targets = vec![omuzaki_io_api::MorphTarget::new(vec![Vec3::Z; deltas]); targets];
    RetainedGeometry::from_primitive(&p)
}

/// An avatar whose retained scene has a drawn source mesh (the runtime mesh 0) and, after it, one source mesh that no
/// node draws, with `primitive`.
fn with_undrawn(primitive: RetainedPrimitive) -> ImportedAvatar {
    let mut a = avatar(false, 0, &[]);
    a.retained.nodes = vec![RetainedNode::new("Face").with_mesh(0, None)];
    a.retained.meshes = vec![
        RetainedMesh::new("Face", vec![RetainedPrimitive::new()]).with_instance(0, vec![0]),
        RetainedMesh::new("Spare", vec![primitive]),
    ];
    a
}

#[test]
fn an_undrawn_mesh_must_keep_its_geometry_and_a_drawn_one_must_not() {
    let kept = RetainedPrimitive::new().with_geometry(triangle(2, 3));
    assert_eq!(with_undrawn(kept.clone()).validate(), Ok(()), "geometry and morph targets kept");
    assert_eq!(with_undrawn(RetainedPrimitive::new()).validate(), Err(AvatarError::undrawn_geometry(1, 0)));

    let mut a = avatar(false, 0, &[]);
    a.retained.nodes = vec![RetainedNode::new("Face").with_mesh(0, None)];
    a.retained.meshes = vec![RetainedMesh::new("Face", vec![kept]).with_instance(0, vec![0])];
    assert_eq!(a.validate(), Err(AvatarError::drawn_geometry(0, 0)));
}

#[test]
fn kept_geometry_must_be_a_valid_mesh_with_fitting_morph_targets() {
    let invalid = |a: &ImportedAvatar| matches!(a.validate(), Err(AvatarError::RetainedGeometryInvalid { .. }));
    let short = with_undrawn(RetainedPrimitive::new().with_geometry(triangle(1, 2)));
    assert!(invalid(&short), "a target of 2 deltas on 3 vertices: {:?}", short.validate());

    let mut geometry = triangle(1, 3);
    geometry.morph_targets[0].normals = vec![Vec3::Z; 2];
    assert!(
        invalid(&with_undrawn(RetainedPrimitive::new().with_geometry(geometry))),
        "normal deltas of the wrong length"
    );

    let mut geometry = triangle(0, 0);
    geometry.indices.clear();
    assert!(invalid(&with_undrawn(RetainedPrimitive::new().with_geometry(geometry))), "a mesh without triangles");

    let mut geometry = triangle(0, 0);
    geometry.extra = vec![omuzaki_avatar_api::retained::RetainedAttribute::new("Bad Name", 1, vec![0.0; 3])];
    let a = with_undrawn(RetainedPrimitive::new().with_geometry(geometry));
    assert!(matches!(a.validate(), Err(AvatarError::ExtraAttributeName { .. })), "{:?}", a.validate());
}

#[test]
fn an_undrawn_meshs_target_names_must_match_its_kept_targets() {
    let mut a = with_undrawn(RetainedPrimitive::new().with_geometry(triangle(2, 3)));
    a.retained.meshes[1] = a.retained.meshes[1].clone().with_morph_target_names(vec!["A".into(), "B".into()]);
    assert_eq!(a.validate(), Ok(()));
    a.retained.meshes[1].morph_target_names.pop();
    assert_eq!(a.validate(), Err(AvatarError::morph_names(1, 1, 2)));
}

/// An avatar whose undrawn source mesh has one primitive per entry of `targets`, each with that many morph targets,
/// and the target names `names`.
fn undrawn_primitives(targets: &[usize], names: &[&str]) -> ImportedAvatar {
    let mut a = with_undrawn(RetainedPrimitive::new());
    let primitives = targets.iter().map(|&t| RetainedPrimitive::new().with_geometry(triangle(t, 3))).collect();
    a.retained.meshes[1] =
        RetainedMesh::new("Spare", primitives).with_morph_target_names(names.iter().map(|n| (*n).to_owned()).collect());
    a
}

#[test]
fn every_primitive_of_an_undrawn_mesh_with_targets_has_the_same_number() {
    let invalid = |a: &ImportedAvatar| matches!(a.validate(), Err(AvatarError::RetainedGeometryInvalid { .. }));
    // One and two targets: invalid with no names, and with two names (which would match the larger count alone).
    assert!(invalid(&undrawn_primitives(&[1, 2], &[])), "{:?}", undrawn_primitives(&[1, 2], &[]).validate());
    assert!(invalid(&undrawn_primitives(&[2, 1], &["A", "B"])), "the smaller count after the larger one too");
    assert!(invalid(&undrawn_primitives(&[1, 2], &["A", "B"])));
    // A primitive without targets beside ones with two is glTF-valid, as `IoScene::check` has it.
    assert_eq!(undrawn_primitives(&[2, 0, 2], &["A", "B"]).validate(), Ok(()));
    assert_eq!(undrawn_primitives(&[0, 0], &[]).validate(), Ok(()));
    // A name table must name exactly the targets.
    assert_eq!(undrawn_primitives(&[2, 2], &["A"]).validate(), Err(AvatarError::morph_names(1, 1, 2)));
    assert_eq!(undrawn_primitives(&[0], &["A"]).validate(), Err(AvatarError::morph_names(1, 1, 0)));
}
