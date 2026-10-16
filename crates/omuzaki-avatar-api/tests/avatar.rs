//! `ImportedAvatar`: one test per `validate` error, `driven` per gaze route and partial look sets, `texture_bytes`.

mod common;

use common::{avatar, ix};
use omuzaki_anim_api::{BoneId, Channel, ChannelMask, SpringChain, SpringParams, SpringSet};
use omuzaki_avatar_api::avatar::texture_bytes;
use omuzaki_avatar_api::error::AvatarError;
use omuzaki_avatar_api::expression::{ColourBind, ExprIx, MorphBind, UvBind};
use omuzaki_avatar_api::gaze::{AvatarGaze, AvatarLookAt};
use omuzaki_avatar_api::retained::RetainedMesh;
use omuzaki_char_api::{BoneName, FaceOverlay, JointRef, MaterialName, TextureData, TextureName};
use omuzaki_core::glam::Vec3;
use omuzaki_core::glam::{UVec2, Vec2, Vec4};
use omuzaki_core::id::Slug;
use omuzaki_face_api::{FaceLayerId, PlanarWindow};
use omuzaki_io_api::{ColorBindKind, LookAtKind, RangeMap};
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

#[test]
fn retained_source_meshes_group_every_mesh_once_and_name_its_targets() {
    let names = || vec!["Fcl_ALL_A".to_owned(), "Fcl_EYE_Close".to_owned()];
    let mut a = avatar(false, 2, &[]);
    // A second runtime mesh, from the same source mesh's second primitive: the two share one name table.
    a.meshes.push(a.meshes[0].clone());
    a.retained.meshes = vec![RetainedMesh::new("Face", vec![0, 1]).with_morph_target_names(names())];
    assert_eq!(a.validate(), Ok(()), "two primitives of one mesh share its names");

    a.retained.meshes = vec![RetainedMesh::new("Face", vec![0]).with_morph_target_names(names())];
    assert_eq!(a.validate(), Err(AvatarError::mesh_grouping(1, 0)), "a mesh from no source mesh");
    a.retained.meshes = vec![RetainedMesh::new("Face", vec![0, 1]), RetainedMesh::new("Body", vec![1])];
    assert_eq!(a.validate(), Err(AvatarError::mesh_grouping(1, 2)), "a mesh from two");
    a.retained.meshes = vec![RetainedMesh::new("Face", vec![0, 1, 2])];
    assert_eq!(a.validate(), Err(AvatarError::unknown_primitive(0, 2)), "a mesh that isn't there");

    a.retained.meshes = vec![RetainedMesh::new("Face", vec![0, 1]).with_morph_target_names(vec!["Fcl_ALL_A".into()])];
    assert_eq!(a.validate(), Err(AvatarError::morph_names(0, 1, 2)), "a name missing");
    a.retained.meshes = vec![RetainedMesh::new("Face", vec![0]), RetainedMesh::new("Hair", vec![1])];
    assert_eq!(a.validate(), Ok(()), "a file that names no targets");

    // A primitive without targets beside one with them, under one name table (glTF allows it).
    a.meshes[1].morphs = None;
    a.retained.meshes = vec![RetainedMesh::new("Face", vec![0, 1]).with_morph_target_names(names())];
    assert_eq!(a.validate(), Ok(()));
    a.retained.meshes =
        vec![RetainedMesh::new("Face", vec![0]), RetainedMesh::new("Hair", vec![1]).with_morph_target_names(names())];
    assert_eq!(a.validate(), Err(AvatarError::morph_names(1, 2, 0)), "names for a mesh with no targets");

    a.retained.meshes = Vec::new();
    assert_eq!(a.validate(), Ok(()), "nothing kept is fine for a hand-built avatar");
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
