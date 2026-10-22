//! Materials and their textures: ufbx's PBR reading of each FBX material (it maps Lambert, Phong and the PBR shaders onto
//! one set of maps) as a glTF-style metallic-roughness material, with its base colour, emissive and normal maps.
//!
//! A texture's image is the file's embedded copy when it has one, otherwise the file it names, read through the
//! [`Resolver`](omuzaki_io_api::Resolver): its relative name first, then each path's final file name, since an FBX often
//! records the absolute path on the machine that wrote it and the texture travels beside the file. The resolver confines
//! every read to the importing file's folder, so a name from the file never reaches anything else.
//!
//! PNG and JPEG are kept byte for byte (`IoScene` stores encoded images). TGA and BMP, common in game and DCC assets, are
//! decoded and re-encoded as PNG, after their headers are checked against [`MAX_SIDE`]. Any other format, or a file that
//! can't be read, leaves the slot empty with a warning, and the material keeps its colour.
//!
//! The texture's UV translation and scale become the slot's `UvTransform`, carried into glTF's flipped `v`; a rotation
//! has no place in the contract (io-api D9) and is reported.

use std::collections::HashMap;

use omuzaki_core::glam::{Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    AlphaMode, Blob, EncodedImage, Finding, ImageIx, ImageMime, ImportCtx, IoError, IoScene, Material, MaterialIx,
    NormalTexture, Pbr, Report, Sampler, Shading, TexRef, Texture, TextureIx, UvTransform, Wrap,
};
use ufbx::SceneRoot;

/// The longest a name from the file is kept, in characters, in a finding.
const NAME_CHARS: usize = 80;
/// Below this opacity a material is blended rather than opaque.
const OPAQUE_FROM: f64 = 0.999;
/// The widest or tallest TGA or BMP decoded, in pixels: its header is read first, so a larger one is refused before any
/// memory goes to its pixels.
pub(crate) const MAX_SIDE: u32 = 16_384;
/// A UV rotation smaller than this, in matrix terms, is rounding, not a rotation.
const ROTATION_EPS: f64 = 1e-6;

/// The scene's materials, by ufbx's `typed_id` (its index in `materials`).
pub(crate) fn convert(
    loaded: &SceneRoot,
    scene: &mut IoScene,
    report: &mut Report,
    ctx: &ImportCtx<'_>,
) -> Result<Vec<MaterialIx>, IoError> {
    let mut images = Images { by_texture: HashMap::new(), ctx, max: ctx.options.limits.max_images };
    let mut out = Vec::with_capacity(loaded.materials.len());
    for material in loaded.materials.iter() {
        let pbr = &material.pbr;
        let mut converted = Material::new(material.element.name.to_string(), Shading::Pbr(Pbr::default()));
        let factor = if pbr.base_factor.has_value { pbr.base_factor.value_vec4.x } else { 1.0 };
        let colour = pbr.base_color.value_vec4;
        let opacity = if pbr.opacity.has_value { pbr.opacity.value_vec4.x.clamp(0.0, 1.0) } else { 1.0 };
        #[allow(clippy::cast_possible_truncation)] // colour factors need no more than f32 digits
        let base = Vec4::new(
            (colour.x * factor) as f32,
            (colour.y * factor) as f32,
            (colour.z * factor) as f32,
            opacity as f32,
        );
        converted.base_color = base.clamp(Vec4::ZERO, Vec4::ONE);
        if opacity < OPAQUE_FROM {
            converted.alpha = AlphaMode::Blend;
        }
        #[allow(clippy::cast_possible_truncation)]
        let pbr_values = Pbr {
            metallic: if pbr.metalness.has_value { pbr.metalness.value_vec4.x.clamp(0.0, 1.0) as f32 } else { 0.0 },
            roughness: if pbr.roughness.has_value { pbr.roughness.value_vec4.x.clamp(0.0, 1.0) as f32 } else { 1.0 },
            metallic_roughness_texture: None,
        };
        converted.shading = Shading::Pbr(pbr_values);
        if pbr.emission_color.has_value {
            let e = pbr.emission_color.value_vec4;
            let f = if pbr.emission_factor.has_value { pbr.emission_factor.value_vec4.x } else { 1.0 };
            #[allow(clippy::cast_possible_truncation)]
            let emissive = Vec3::new((e.x * f) as f32, (e.y * f) as f32, (e.z * f) as f32);
            converted.emissive = emissive.clamp(Vec3::ZERO, Vec3::ONE);
        }
        let name = &converted.name;
        converted.base_color_texture =
            images.slot(scene, report, name, "base colour", pbr.base_color.texture.as_deref())?;
        converted.emissive_texture =
            images.slot(scene, report, name, "emission", pbr.emission_color.texture.as_deref())?;
        let normal = images.slot(scene, report, name, "normal", pbr.normal_map.texture.as_deref())?;
        converted.normal_texture = normal.map(|texture| NormalTexture { texture, scale: 1.0 });
        out.push(scene.add_material(converted));
    }
    Ok(out)
}

/// The textures already read, so a texture several materials use is one image.
struct Images<'a, 'c> {
    by_texture: HashMap<u32, Option<TextureIx>>,
    ctx: &'a ImportCtx<'c>,
    max: u32,
}

impl Images<'_, '_> {
    /// The texture for one material slot, or `None` (with a warning) when its image can't be kept.
    fn slot(
        &mut self,
        scene: &mut IoScene,
        report: &mut Report,
        material: &str,
        slot: &str,
        texture: Option<&ufbx::Texture>,
    ) -> Result<Option<TexRef>, IoError> {
        let Some(texture) = texture else { return Ok(None) };
        let reference =
            |made: Option<TextureIx>| made.map(|ix| TexRef { transform: uv_transform(texture), ..TexRef::of(ix) });
        if let Some(known) = self.by_texture.get(&texture.element.typed_id) {
            return Ok(reference(*known));
        }
        if texture.has_uv_transform && rotates(texture) {
            let message = format!(
                "the {slot} texture of material '{}' is rotated in UV space, which isn't imported; its offset and scale are",
                material.chars().take(NAME_CHARS).collect::<String>()
            );
            report.push(
                Finding::warning("fbx.texture_uv_rotation", message)
                    .at(format!(
                        "Objects/Texture/{}",
                        texture.element.name.chars().take(NAME_CHARS).collect::<String>()
                    ))
                    .hint("bake the rotation into the UVs in the modelling tool"),
            );
        }
        let made = match image_bytes(texture, self.ctx) {
            Ok((bytes, mime)) => {
                if scene.images.len() >= self.max as usize {
                    return Err(IoError::Limit {
                        what: "image count",
                        limit: u64::from(self.max),
                        got: scene.images.len() as u64 + 1,
                    });
                }
                let image: ImageIx = scene.add_image(EncodedImage::new(texture.element.name.to_string(), mime, bytes));
                let sampler =
                    Sampler { wrap_u: wrap(texture.wrap_u), wrap_v: wrap(texture.wrap_v), ..Sampler::default() };
                Some(scene.add_texture(Texture { image, sampler }))
            }
            Err(why) => {
                let short = |s: &str| s.chars().take(NAME_CHARS).collect::<String>();
                let message = format!("the {slot} texture of material '{}' is not imported: {why}", short(material));
                report.push(
                    Finding::warning("fbx.texture_dropped", message)
                        .at(format!("Objects/Texture/{}", short(&texture.element.name)))
                        .hint("embed the texture as PNG, JPEG, TGA or BMP, or import the file with its textures beside it"),
                );
                None
            }
        };
        self.by_texture.insert(texture.element.typed_id, made);
        Ok(reference(made))
    }
}

/// ufbx's wrap mode as the contract's.
fn wrap(mode: ufbx::WrapMode) -> Wrap {
    match mode {
        ufbx::WrapMode::Clamp => Wrap::ClampToEdge,
        ufbx::WrapMode::Repeat => Wrap::Repeat,
    }
}

/// A texture's encoded image: its embedded content, its video's, or the file it names through the resolver; TGA and BMP
/// as PNG.
fn image_bytes(texture: &ufbx::Texture, ctx: &ImportCtx<'_>) -> Result<(Blob, ImageMime), String> {
    let embedded = if texture.content.is_empty() {
        texture.video.as_ref().map(|v| &v.content[..])
    } else {
        Some(&texture.content[..])
    };
    let (bytes, name) = if let Some(bytes) = embedded.filter(|b| !b.is_empty()) {
        (bytes.to_vec(), texture.filename.to_string())
    } else {
        read_named(texture, ctx)?
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok((Blob::new(bytes), ImageMime::Png));
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok((Blob::new(bytes), ImageMime::Jpeg));
    }
    let format = if bytes.starts_with(b"BM") {
        image::ImageFormat::Bmp
    } else if name.to_ascii_lowercase().ends_with(".tga") || bytes.ends_with(b"TRUEVISION-XFILE.\0") {
        // TGA has no signature at the front: the extension, or version 2's footer, names it.
        image::ImageFormat::Tga
    } else {
        return Err("its image is not PNG, JPEG, TGA or BMP".to_owned());
    };
    as_png(&bytes, format).map(|png| (Blob::new(png), ImageMime::Png))
}

/// The bytes of the file `texture` names, and the name that was read: the relative name, then ufbx's combined name and
/// the absolute one, then each one's final file name (either separator), the first the resolver can read.
fn read_named(texture: &ufbx::Texture, ctx: &ImportCtx<'_>) -> Result<(Vec<u8>, String), String> {
    let recorded = [texture.relative_filename.trim(), texture.filename.trim(), texture.absolute_filename.trim()];
    let base = |path: &str| path.rsplit(['/', '\\']).next().unwrap_or("").to_owned();
    let mut names: Vec<String> = recorded.iter().map(|n| n.replace('\\', "/")).collect();
    names.extend(recorded.iter().map(|n| base(n)));
    names.retain(|n| !n.is_empty());
    names.dedup();
    let mut last = "it names no file".to_owned();
    for name in names.iter().filter(|n| !n.is_empty()) {
        match ctx.resolver.read(name) {
            Ok(bytes) => return Ok((bytes, name.clone())),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// A TGA or BMP file as PNG bytes. The header's size is checked against [`MAX_SIDE`] before anything is decoded.
fn as_png(bytes: &[u8], format: image::ImageFormat) -> Result<Vec<u8>, String> {
    use image::ImageDecoder;

    let name = if format == image::ImageFormat::Tga { "TGA" } else { "BMP" };
    let broken = |e: image::ImageError| format!("its {name} image doesn't decode: {e}");
    let cursor = std::io::Cursor::new(bytes);
    let (width, height) = match format {
        image::ImageFormat::Tga => image::codecs::tga::TgaDecoder::new(cursor).map_err(broken)?.dimensions(),
        _ => image::codecs::bmp::BmpDecoder::new(cursor).map_err(broken)?.dimensions(),
    };
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!("its {name} image is {width} by {height} pixels, over the {MAX_SIDE} a side this reads"));
    }
    let rgba = image::load_from_memory_with_format(bytes, format).map_err(broken)?.to_rgba8();
    let mut png = Vec::new();
    rgba.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("its {name} image doesn't re-encode as PNG: {e}"))?;
    Ok(png)
}

/// Whether the texture's UV matrix turns as well as scales.
fn rotates(texture: &ufbx::Texture) -> bool {
    let m = &texture.uv_to_texture;
    m.m01.abs() > ROTATION_EPS || m.m10.abs() > ROTATION_EPS
}

/// The texture's UV translation and scale in glTF's UV space, or `None` when it has none. ufbx gives the matrix from a
/// mesh UV to the texture coordinate (the inverse of the placement the file records). FBX's `v` grows upward and glTF's
/// downward (`v' = 1 - v`), so its scale `s` and offset `o` along `v` become the offset `1 - s - o`.
#[allow(clippy::cast_possible_truncation)] // UV scales and offsets are small; f32 holds them
fn uv_transform(texture: &ufbx::Texture) -> Option<UvTransform> {
    if !texture.has_uv_transform {
        return None;
    }
    let m = &texture.uv_to_texture;
    let scale = Vec2::new(m.m00 as f32, m.m11 as f32);
    let offset = Vec2::new(m.m03 as f32, (1.0 - m.m11 - m.m13) as f32);
    let transform = UvTransform { offset, scale };
    (transform != UvTransform::default()).then_some(transform)
}
