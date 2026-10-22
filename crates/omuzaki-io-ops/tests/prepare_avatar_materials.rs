//! `to_avatar`'s materials and textures (#866): the `MToon` groups the renderer can't draw are one finding each, the
//! file's materials are retained bit for bit, textures decode to the file's pixels at full size, and edits apply to the
//! runtime only. Every material and image is generated here.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{add_skinned_mesh, def, humanoid, toon_material};
use omuzaki_avatar_api::def::MaterialEdit;
use omuzaki_char_api::{MaterialName, TextureData};
use omuzaki_core::glam::{UVec2, Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    AlphaMode, EncodedImage, ImageIx, Material, NormalTexture, OutlineMode, Report, Sampler, Shading, ShiftTexture,
    TexRef, Texture, TextureIx, Toon, UvTransform,
};
use omuzaki_io_ops::convert::texture::encode_png;
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_render_api::{ColourSpace, MtoonSupport, OutlineWidthMode, TextureDesc};

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

/// A toon material that uses the seven `MToon` groups of D7 and the lighting model (its rim lift), and a toony factor
/// out of range, which no group covers.
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
    toon.shading_toony = 1.5;
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
    assert_eq!((groups(0), groups(1)), (8, 0), "{report:?}");
    for field in [
        "rim_lift",
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
    let toony = |report: &Report| {
        report.findings.iter().any(|x| {
            x.rule == "material.approximated" && x.location.as_deref() == Some("materials/0/shading/shading_toony")
        })
    };
    assert!(toony(&report), "a field outside every group keeps its note");

    let (_, report) = to_avatar(&f.scene, &def(), MtoonSupport::ALL).expect("imports");
    assert!(!report.has_rule("look.mtoon_not_drawn"), "a renderer that draws every group reports none");
    assert!(!report.has_rule("material.not_drawn"), "nor any field of them: {report:?}");
    assert!(toony(&report), "the toony factor is still clamped");
    let (_, report) = to_avatar(&f.scene, &def(), MtoonSupport::MATCAP).expect("imports");
    assert_eq!(report.findings.iter().filter(|x| x.rule == "look.mtoon_not_drawn").count(), 7, "the matcap is drawn");
}

/// A toon material with every `MToon` lighting field off its default and texture `i` in each texture slot: shade 0,
/// shift 1, matcap 2, rim 3, emissive 4, normal 5, animation mask 6 (base colour 7).
fn lit(order: i32, zwrite: bool) -> Material {
    let mut toon = Toon::default();
    toon.shade_color = Vec3::new(0.2, 0.1, 0.4);
    toon.shade_multiply_texture = Some(TexRef::of(TextureIx(0)));
    toon.shading_shift = -0.3;
    toon.shading_shift_texture = Some(ShiftTexture { texture: TexRef::of(TextureIx(1)), scale: 0.5 });
    toon.shading_toony = 0.75;
    toon.gi_equalization = 0.25;
    toon.matcap_color = Vec3::new(0.5, 0.6, 0.7);
    toon.matcap_texture = Some(TexRef::of(TextureIx(2)));
    toon.rim_color = Vec3::new(0.3, 0.2, 0.1);
    toon.rim_multiply_texture = Some(TexRef::of(TextureIx(3)));
    toon.rim_fresnel_power = 2.5;
    toon.rim_lift = 0.15;
    toon.rim_lighting_mix = 0.35;
    toon.uv_scroll_x = 0.1;
    toon.uv_scroll_y = -0.2;
    toon.uv_rotation = 0.3;
    toon.uv_animation_mask = Some(TexRef::of(TextureIx(6)));
    toon.transparent_with_zwrite = zwrite;
    toon.render_queue_offset = order;
    let mut m = Material::new("Lit", Shading::Toon(Box::new(toon)));
    m.base_color_texture = Some(TexRef::of(TextureIx(7)));
    m.emissive_texture = Some(TexRef::of(TextureIx(4)));
    m.normal_texture = Some(NormalTexture { texture: TexRef::of(TextureIx(5)), scale: 0.8 });
    m.alpha = AlphaMode::Blend;
    m
}

#[test]
fn mtoon_lighting_is_kept_as_the_file_has_it_where_the_renderer_draws_it() {
    let images = (0..8).map(|i| png(&format!("t{i}"), 2).0).collect();
    let f = scene_with(vec![lit(3, false), lit(-12, true)], images);
    let (avatar, report) = to_avatar(&f.scene, &def(), MtoonSupport::ALL).expect("imports");
    assert!(!report.has_rule("material.not_drawn") && !report.has_rule("look.mtoon_not_drawn"), "{report:?}");
    let names: Vec<MaterialName> = avatar.materials.keys().cloned().collect();
    let m = &avatar.mtoon[&names[0]];
    let space = |t: &Option<omuzaki_char_api::TextureName>| avatar.textures[t.as_ref().unwrap()].desc.colour_space;
    assert_eq!(
        [&m.shade_texture, &m.shading_shift_texture, &m.matcap_texture, &m.rim_texture, &m.emissive_texture].map(space),
        [ColourSpace::Srgb, ColourSpace::Linear, ColourSpace::Srgb, ColourSpace::Srgb, ColourSpace::Srgb],
        "each texture in its slot's colour space"
    );
    assert_eq!([&m.normal_texture, &m.uv_animation_mask].map(space), [ColourSpace::Linear; 2]);
    let numbers = (
        m.shading_shift,
        m.shading_shift_scale,
        m.shading_toony,
        m.gi_equalisation,
        m.rim_fresnel_power,
        m.rim_lift,
        m.rim_lighting_mix,
        m.normal_scale,
        m.uv_scroll,
        m.uv_rotation,
    );
    assert_eq!(numbers, (-0.3, 0.5, 0.75, 0.25, 2.5, 0.15, 0.35, 0.8, Vec2::new(0.1, -0.2), 0.3));
    assert_eq!(m.shade, omuzaki_core::colour::Colour::from_linear(Vec3::new(0.2, 0.1, 0.4), 1.0));
    assert!(!m.blend_writes_depth && avatar.mtoon[&names[1]].blend_writes_depth);
    // three-vrm's render order: 19 without depth writes, 0 with, plus the offset clamped to -9..=9.
    let orders: Vec<i32> = avatar.meshes.iter().map(|mesh| mesh.mesh.render_order).collect();
    assert_eq!(orders, [22, -9]);

    // A renderer without the lighting model keeps the toon approximation and says so.
    let (avatar, report) = to_avatar(&f.scene, &def(), MtoonSupport::OUTLINE_STYLE).expect("imports");
    assert!(avatar.mtoon.is_empty());
    assert!(report.has_rule("look.mtoon_not_drawn"));
    assert!(avatar.meshes.iter().all(|mesh| mesh.mesh.render_order == 0));
    assert_eq!(avatar.textures.len(), 1, "only the base colour texture is decoded");
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

/// A toon material with an `MToon` outline of `mode`, `width` wide, mixed `mix` with the light, its width multiplied
/// by the scene's texture `width_texture`.
fn outlined(name: &str, mode: OutlineMode, width: f32, mix: f32, width_texture: Option<TextureIx>) -> Material {
    let mut toon = Toon::default();
    toon.outline_mode = mode;
    toon.outline_width = width;
    toon.outline_lighting_mix = mix;
    toon.outline_color = Vec3::new(0.1, 0.05, 0.2);
    toon.outline_width_texture = width_texture.map(TexRef::of);
    let mut m = Material::new(name, Shading::Toon(Box::new(toon)));
    m.base_color = Vec4::ONE;
    m
}

#[test]
fn the_mtoon_outline_is_kept_as_the_file_has_it_where_the_renderer_draws_it() {
    // #647: a screen-space outline with a width texture and a lighting mix, and a world-space one.
    let (image, pixels) = png("outline_mask", 4);
    let screen = outlined("Screen", OutlineMode::ScreenCoordinates, 0.012, 0.7, Some(TextureIx(0)));
    let world = outlined("World", OutlineMode::WorldCoordinates, 0.004, 1.0, None);
    let f = scene_with(vec![screen, world, toon_material("Plain", Vec4::ONE)], vec![image]);
    let name = |n: &str| MaterialName(omuzaki_core::id::Slug::new(n).unwrap());

    let (avatar, report) = to_avatar(&f.scene, &def(), MtoonSupport::OUTLINE_STYLE).expect("imports");
    let outline = avatar.outlines.get(&name("screen")).expect("the screen outline is kept");
    assert_eq!((outline.width_mode, outline.width, outline.lighting_mix), (OutlineWidthMode::Screen, 0.012, 0.7));
    let texture = outline.width_texture.as_ref().and_then(|t| avatar.textures.get(t)).expect("the mask is decoded");
    assert_eq!(texture.desc.colour_space, ColourSpace::Linear, "a width mask is linear data");
    assert_eq!(texture.rgba8, pixels, "at full size, as the file has it");
    let outline = avatar.outlines.get(&name("world")).expect("the world outline is kept");
    assert_eq!((outline.width_mode, outline.width, outline.lighting_mix), (OutlineWidthMode::World, 0.004, 1.0));
    assert!(!avatar.outlines.contains_key(&name("plain")), "no outline, no entry");
    assert!(!report.has_rule("look.mtoon_not_drawn"), "every outline group is drawn: {report:?}");
    let approximated =
        report.findings.iter().any(|x| x.location.as_deref() == Some("materials/0/shading/outline_mode"));
    assert!(!approximated, "a drawn screen width is not approximated: {report:?}");

    // A renderer that draws none of it: the screen width stays the kit line's approximation, and the world width is
    // drawn in metres with no mix and no mask.
    let (avatar, report) = to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("imports");
    assert!(!avatar.outlines.contains_key(&name("screen")));
    let outline = avatar.outlines.get(&name("world")).expect("a world width is drawn by every renderer");
    assert_eq!(
        (outline.width_mode, outline.lighting_mix, outline.width_texture.is_some()),
        (OutlineWidthMode::World, 0.0, false)
    );
    assert!(avatar.textures.is_empty(), "the mask is not decoded for a renderer that can't draw it");
    let groups = report.findings.iter().filter(|x| x.rule == "look.mtoon_not_drawn").count();
    assert_eq!(groups, 3, "the screen width, its lighting mix and the mask: {report:?}");
}
