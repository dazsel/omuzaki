//! `Retained` round-trips through serde bit for bit: off-grid linear colours, the whole `MToon` block, passthrough,
//! the meta (with its 0.x extras) and the thumbnail.
#![allow(clippy::unwrap_used, clippy::expect_used)] // fixture helpers outside #[test] functions

use omuzaki_anim_api::BoneId;
use omuzaki_avatar_api::retained::{Retained, RetainedImage, RetainedMaterial, RetainedNode};
use omuzaki_char_api::MaterialName;
use omuzaki_core::glam::{Vec3, Vec4};
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
    r.nodes = vec![RetainedNode::new("Root"), RetainedNode::new("J_Bip_C_Hips").with_bone(BoneId(0))];
    r.morph_names = vec![vec!["Fcl_ALL_A".into(), "Fcl_EYE_Close_L".into()], Vec::new()];
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
    assert_eq!(back.morph_names[0], ["Fcl_ALL_A", "Fcl_EYE_Close_L"], "the file's names, not the runtime slugs");
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
