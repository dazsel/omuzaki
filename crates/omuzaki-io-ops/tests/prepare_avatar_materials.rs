//! `to_avatar`'s materials and textures (#866): the `MToon` groups the renderer can't draw are one finding each, the
//! file's materials are retained bit for bit, textures decode to the file's pixels at full size, and edits apply to the
//! runtime only. Every material and image is generated here.

mod common;

use common::{add_skinned_mesh, def, humanoid, toon_material};
use omuzaki_avatar_api::def::MaterialEdit;
use omuzaki_char_api::{MaterialName, TextureData};
use omuzaki_core::glam::{UVec2, Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    EncodedImage, ImageIx, Material, OutlineMode, Report, Sampler, Shading, TexRef, Texture, TextureIx, Toon,
    UvTransform,
};
use omuzaki_io_ops::convert::texture::encode_png;
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_render_api::{MtoonSupport, TextureDesc};

/// A `size`-square PNG whose pixels are a fixed pattern, and those pixels.
fn png(name: &str, size: u32) -> (EncodedImage, Vec<u8>) {
    let rgba8: Vec<u8> = (0..size * size * 4).map(|i| u8::try_from((i * 7 + 3) % 251).unwrap()).collect();
    let texture = TextureData { desc: TextureDesc::colour(UVec2::splat(size)), rgba8: rgba8.clone() };
    (encode_png(name, &texture, "test", &mut Report::new()).unwrap(), rgba8)
}

/// A humanoid whose body meshes draw `materials`, one mesh each, with `images` as its images and one texture per
/// image (the scene's texture `i` shows image `i`).
fn scene_with(materials: Vec<Material>, images: Vec<EncodedImage>) -> common::Fixture {
    let mut f = humanoid(0.0);
    for (i, image) in images.into_iter().enumerate() {
        f.scene.add_image(image);
        f.scene.add_texture(Texture { image: ImageIx(u32::try_from(i).unwrap()), sampler: Sampler::default() });
    }
    let count = materials.len();
    for m in materials {
        f.scene.add_material(m);
    }
    for i in 0..count {
        add_skinned_mesh(&mut f, &format!("Part{i}"), &["hips", "head"], 0, Some(u32::try_from(i).unwrap()));
    }
    f
}

/// A toon material that uses all seven `MToon` groups the renderer doesn't draw yet, and the rim lift, which is no
/// group's.
fn every_group(matcap: TextureIx) -> Material {
    let mut toon = Toon::default();
    toon.matcap_texture = Some(TexRef::of(matcap));
    toon.gi_equalization = 0.5;
    toon.uv_scroll_x = 0.25;
    toon.render_queue_offset = 2;
    toon.outline_mode = OutlineMode::ScreenCoordinates;
    toon.outline_width = 0.01;
    toon.rim_lighting_mix = 0.3;
    toon.outline_lighting_mix = 0.4;
    toon.rim_lift = 0.2;
    let mut m = Material::new("Everything", Shading::Toon(Box::new(toon)));
    m.base_color = Vec4::ONE;
    m
}

#[test]
fn each_mtoon_group_the_renderer_cant_draw_is_one_finding() {
    let (image, _) = png("matcap", 4);
    let f = scene_with(vec![every_group(TextureIx(0)), toon_material("Plain", Vec4::ONE)], vec![image]);
    let (_, report) = to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("imports");
    let at = |m: usize| format!("materials/{m}/shading");
    let groups = |m: usize| {
        report
            .findings
            .iter()
            .filter(|x| x.rule == "look.mtoon_not_drawn" && x.location.as_deref() == Some(&at(m)))
            .count()
    };
    assert_eq!((groups(0), groups(1)), (7, 0), "{report:?}");
    for field in [
        "gi_equalization",
        "matcap_texture",
        "uv_animation",
        "render_queue_offset",
        "rim_lighting_mix",
        "outline_lighting_mix",
        "outline_mode",
    ] {
        let location = format!("materials/0/shading/{field}");
        assert!(
            !report.findings.iter().any(|x| x.location.as_deref() == Some(&location)),
            "{field} has no note of its own"
        );
    }
    let rim_lift = report
        .findings
        .iter()
        .any(|x| x.rule == "material.not_drawn" && x.location.as_deref() == Some("materials/0/shading/rim_lift"));
    assert!(rim_lift, "a field outside the seven groups keeps its note");

    let all = [
        MtoonSupport::MATCAP,
        MtoonSupport::GI_EQUALISATION,
        MtoonSupport::UV_ANIMATION,
        MtoonSupport::RENDER_QUEUE_OFFSET,
        MtoonSupport::OUTLINE_WIDTH_SCREEN,
        MtoonSupport::RIM_LIGHTING_MIX,
        MtoonSupport::OUTLINE_LIGHTING_MIX,
    ]
    .into_iter()
    .fold(MtoonSupport::NONE, MtoonSupport::with);
    let (_, report) = to_avatar(&f.scene, &def(), all).expect("imports");
    assert!(!report.has_rule("look.mtoon_not_drawn"), "a renderer that draws every group reports none");
    let (_, report) = to_avatar(&f.scene, &def(), MtoonSupport::MATCAP).expect("imports");
    assert_eq!(report.findings.iter().filter(|x| x.rule == "look.mtoon_not_drawn").count(), 6, "the matcap is drawn");
}

#[test]
fn materials_are_retained_bit_for_bit() {
    let mut m = toon_material("Body_00", Vec4::new(0.5, 0.123, 0.9999, 1.0));
    m.emissive = Vec3::new(0.123, 0.5, 0.9999);
    if let Shading::Toon(t) = &mut m.shading {
        t.shade_color = Vec3::new(0.9999, 0.123, 0.5);
        t.rim_color = Vec3::splat(0.123);
    }
    let f = scene_with(vec![m.clone()], Vec::new());
    let (avatar, _) = to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("imports");
    let retained = &avatar.retained.materials[0];
    assert_eq!(retained.name, MaterialName::new("body_00").unwrap());
    assert_eq!(retained.source, m, "the file's own values");
    let bits = |v: Vec4| v.to_array().map(f32::to_bits);
    assert_eq!(bits(retained.source.base_color), bits(m.base_color));
}

#[test]
fn textures_decode_to_the_files_pixels_at_full_size() {
    let (image, pixels) = png("body", 512);
    let mut m = toon_material("Body", Vec4::ONE);
    m.base_color_texture = Some(TexRef::of(TextureIx(0)));
    let f = scene_with(vec![m], vec![image]);
    let (avatar, report) = to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("imports");
    let texture = avatar.textures.values().next().expect("the base texture");
    assert_eq!(texture.desc.size, UVec2::splat(512), "never downscaled");
    assert_eq!(texture.rgba8, pixels);
    assert_eq!(avatar.texture_bytes(), 1_398_100, "512 squared with its mips");
    let spec = avatar.materials.values().next().expect("the material");
    assert_eq!(spec.base_texture.as_ref(), avatar.textures.keys().next());
    assert!(!report.has_rule("material.texture_missing"), "{report:?}");
}

#[test]
fn an_unreadable_texture_is_reported_missing_without_a_panic() {
    let broken = EncodedImage::new("broken", omuzaki_io_api::ImageMime::Png, omuzaki_io_api::Blob::new(vec![1, 2, 3]));
    let mut m = toon_material("Body", Vec4::ONE);
    m.base_color_texture = Some(TexRef::of(TextureIx(0)));
    let f = scene_with(vec![m], vec![broken]);
    let (avatar, report) = to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("still imports");
    assert!(report.has_rule("texture.unreadable") && report.has_rule("material.texture_missing"), "{report:?}");
    assert!(avatar.textures.is_empty());
    assert_eq!(avatar.materials.values().next().and_then(|m| m.base_texture.clone()), None);
}

#[test]
fn the_base_textures_transform_is_kept_for_uv_binds() {
    let (image, _) = png("atlas", 4);
    let mut m = toon_material("Eyes", Vec4::ONE);
    let mut tex = TexRef::of(TextureIx(0));
    tex.transform = Some(UvTransform { offset: Vec2::new(0.5, 0.0), scale: Vec2::new(0.5, 1.0) });
    m.base_color_texture = Some(tex);
    let f = scene_with(vec![m], vec![image]);
    let (avatar, report) = to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("imports");
    let spec = avatar.materials.values().next().expect("the material");
    assert_eq!((spec.uv_transform.scale, spec.uv_transform.offset), (Vec2::new(0.5, 1.0), Vec2::new(0.5, 0.0)));
    let dropped = report.findings.iter().any(|x| x.location.as_deref() == Some("materials/0/base_color_texture"));
    assert!(!dropped, "the transform is kept, so no note says it is dropped: {report:?}");
}

#[test]
fn an_edit_changes_the_runtime_and_leaves_the_file_retained() {
    let m = toon_material("Body", Vec4::new(0.5, 0.5, 0.5, 1.0));
    let f = scene_with(vec![m.clone()], Vec::new());
    let mut def = def();
    let mut edit = MaterialEdit::new();
    edit.base = Some([1.0, 0.0, 0.0, 1.0]);
    def.materials.insert(MaterialName::new("body").unwrap(), edit.clone());
    def.materials.insert(MaterialName::new("gone").unwrap(), edit);
    let (avatar, report) = to_avatar(&f.scene, &def, MtoonSupport::NONE).expect("imports");
    let spec = &avatar.materials[&MaterialName::new("body").unwrap()];
    assert_eq!(spec.base, omuzaki_core::colour::Colour::rgb(255, 0, 0), "the edit is what the stage shows");
    assert_eq!(avatar.retained.materials[0].source, m, "export applies the edit over the file's own values");
    assert!(report.has_rule("avatar.material_edit_unknown"), "an edit naming no material is reported");
}
