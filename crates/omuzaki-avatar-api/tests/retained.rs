//! `Retained` round-trips through serde bit for bit: off-grid linear colours, the whole `MToon` block, passthrough,
//! the meta (with its 0.x extras) and the thumbnail.
#![allow(clippy::unwrap_used, clippy::expect_used)] // fixture helpers outside #[test] functions

use omuzaki_anim_api::BoneId;
use omuzaki_avatar_api::retained::{
    Retained, RetainedAnnotation, RetainedFirstPerson, RetainedImage, RetainedMaterial, RetainedMesh, RetainedNode,
    RetainedPrimitive, RetainedSkin, RetainedTransform,
};
use omuzaki_char_api::MaterialName;
use omuzaki_core::glam::{Mat4, Quat, Vec3, Vec4};
use omuzaki_io_api::{
    ImageIx, ImageMime, Material, Passthrough, Sampler, Shading, TexRef, Texture, TextureIx, Toon, VrmMeta,
};
use omuzaki_store_api::ContentHash;
use serde_json::json;

const OFF_GRID: [f32; 3] = [0.5, 0.123, 0.9999];

fn toon_material() -> Material {
    let mut toon = Toon::default();
    toon.shade_color = Vec3::from_array(OFF_GRID);
    toon.rim_color = Vec3::new(0.123, 0.0, 0.9999);
    toon.matcap_texture = Some(TexRef::of(TextureIx(0)));
    toon.uv_scroll_x = 0.1;
    toon.render_queue_offset = -3;
    let mut m = Material::new("Body_00", Shading::Toon(Box::new(toon)));
    m.base_color = Vec4::new(0.5, 0.123, 0.9999, 0.5);
    m.emissive = Vec3::splat(0.123);
    m.base_color_texture = Some(TexRef::of(TextureIx(0)));
    m.passthrough.extensions.insert("KHR_materials_sheen".into(), json!({ "sheenColorFactor": [0.1, 0.2, 0.3] }));
    m.passthrough.extras = Some(json!(null));
    m
}

fn retained() -> Retained {
    let mut meta = VrmMeta::default();
    meta.name = "Ava".into();
    meta.authors = vec!["the project".into()];
    meta.thumbnail_image = Some(ImageIx(1));
    meta.extras.insert("noAiTraining".into(), json!(true));
    meta.extras_vrm0.insert("violentUssageName".into(), json!("Disallow"));
    let mut passthrough = Passthrough::default();
    passthrough.extensions.insert("EXT_studio".into(), json!({ "take": 3 }));
    let mut r = Retained::new();
    r.materials = vec![RetainedMaterial::new(MaterialName::new("body_00").unwrap(), toon_material())];
    r.textures = vec![Texture { image: ImageIx(0), sampler: Sampler::default() }];
    r.images = vec![
        RetainedImage::new("body", ImageMime::Png, ContentHash([1; 32])),
        RetainedImage::new("thumbnail", ImageMime::Jpeg, ContentHash([2; 32])),
    ];
    let turned = RetainedTransform {
        translation: Vec3::new(0.0, 0.123, 0.0),
        rotation: Quat::from_rotation_y(0.5),
        scale: Vec3::splat(0.9999),
    };
    r.nodes = vec![
        RetainedNode::new("Root").with_children(vec![1, 2]),
        RetainedNode::new("J_Bip_C_Hips").with_bone(BoneId(0)).with_transform(turned),
        RetainedNode::new("Face").with_mesh(0, Some(0)),
    ];
    r.roots = vec![0];
    r.first_person = Some(vec![
        RetainedAnnotation::new(2, RetainedFirstPerson::ThirdPersonOnly),
        RetainedAnnotation::new(1, RetainedFirstPerson::Auto),
    ]);
    r.skins = vec![
        RetainedSkin::new("skin", vec![1], vec![Mat4::from_translation(Vec3::new(0.0, -0.123, 0.0))]).with_skeleton(0),
    ];
    let primitive = |m: u16| RetainedPrimitive::new(m).with_material(0);
    r.meshes = vec![
        RetainedMesh::new("Face", vec![primitive(0), RetainedPrimitive::new(1)])
            .with_morph_target_names(vec!["Fcl_ALL_A".into(), "Fcl_EYE_Close_L".into()]),
        RetainedMesh::new("Body", vec![primitive(2)]),
    ];
    r.meta = Some(meta);
    r.passthrough = passthrough;
    r
}

fn colours(m: &Material) -> Vec<u32> {
    let Shading::Toon(t) = &m.shading else { panic!("toon") };
    m.base_color
        .to_array()
        .into_iter()
        .chain(m.emissive.to_array())
        .chain(t.shade_color.to_array())
        .chain(t.rim_color.to_array())
        .map(f32::to_bits)
        .collect()
}

#[test]
fn retained_data_round_trips_through_json_bit_for_bit() {
    let r = retained();
    let text = serde_json::to_string(&r).unwrap();
    let back: Retained = serde_json::from_str(&text).unwrap();
    assert_eq!(back, r);
    assert_eq!(colours(&back.materials[0].source), colours(&r.materials[0].source), "the bits, not just equality");
    assert_eq!(serde_json::to_string(&back).unwrap(), text, "writing again gives the same text");
}

#[test]
fn the_meta_keeps_its_vrm0_extras_and_the_thumbnail_resolves() {
    let back: Retained = serde_json::from_value(serde_json::to_value(retained()).unwrap()).unwrap();
    let meta = back.meta.as_ref().unwrap();
    assert_eq!(meta.extras_vrm0.get("violentUssageName"), Some(&json!("Disallow")));
    assert_eq!(meta.extras.get("noAiTraining"), Some(&json!(true)));
    assert_eq!(back.thumbnail().map(|i| i.name.as_str()), Some("thumbnail"));
    assert_eq!(back.material(&MaterialName::new("body_00").unwrap()).map(|m| m.name.as_str()), Some("Body_00"));
}

#[test]
fn passthrough_and_a_present_null_extras_survive() {
    let back: Retained = serde_json::from_str(&serde_json::to_string(&retained()).unwrap()).unwrap();
    assert_eq!(back.materials[0].source.passthrough.extras, Some(json!(null)));
    assert_eq!(back.meshes[0].morph_target_names, ["Fcl_ALL_A", "Fcl_EYE_Close_L"], "the file's names, not slugs");
    assert_eq!(back.meshes[0].primitives, [RetainedPrimitive::new(0).with_material(0), RetainedPrimitive::new(1)]);
    assert_eq!((back.nodes[0].children.as_slice(), back.roots.as_slice()), (&[1, 2][..], &[0][..]), "the scene graph");
    assert_eq!(back.nodes[1].transform, r_transform(&retained()), "a transform, bit for bit");
    assert_eq!((back.nodes[2].mesh, back.nodes[2].skin), (Some(0), Some(0)));
    assert_eq!(back.skins, retained().skins, "joints, inverse binds and the skeleton root");
    assert_eq!(back.first_person, retained().first_person, "annotations by node, an explicit auto kept");
    let text = serde_json::to_string(&back).unwrap();
    assert!(text.contains("\"thirdPersonOnly\""), "flags as the file writes them: {text}");
    let mut none = back.clone();
    none.first_person = None;
    let read: Retained = serde_json::from_str(&serde_json::to_string(&none).unwrap()).unwrap();
    assert_eq!(read.first_person, None, "no firstPerson block stays absent, not empty");
    assert!(back.passthrough.extensions.contains_key("EXT_studio"));
}

#[test]
fn an_empty_retained_writes_an_empty_table_and_unknown_keys_are_refused() {
    assert_eq!(
        serde_json::to_value(Retained::new()).unwrap(),
        json!({ "materials": [], "textures": [], "images": [], "nodes": [] })
    );
    assert_eq!(serde_json::from_value::<Retained>(json!({})).unwrap(), Retained::new());
    assert!(serde_json::from_value::<Retained>(json!({ "materails": [] })).is_err());
    assert!(serde_json::from_value::<Retained>(json!({ "meta": { "vrm1": {}, "vrm2": {} } })).is_err());
}

/// The transform of the hips node in [`retained`].
fn r_transform(r: &Retained) -> RetainedTransform {
    r.nodes[1].transform
}
