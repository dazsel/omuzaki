//! Materials and their textures: ufbx's PBR reading of each FBX material (it maps Lambert, Phong and the PBR shaders onto
//! one set of maps) as a glTF-style metallic-roughness material, with its base colour, emissive and normal maps.
//!
//! A texture's image is the file's embedded copy when it has one, otherwise the file it names, read through the
//! [`Resolver`](omuzaki_io_api::Resolver) (relative name first). Only PNG and JPEG are kept, byte for byte (`IoScene`
//! stores encoded images and never re-encodes); any other format, or a file that can't be read, leaves the slot empty with
//! a warning, and the material keeps its colour.

use std::collections::HashMap;

use omuzaki_core::glam::{Vec3, Vec4};
use omuzaki_io_api::{
    AlphaMode, Blob, EncodedImage, Finding, ImageIx, ImageMime, ImportCtx, IoError, IoScene, Material, MaterialIx,
    NormalTexture, Pbr, Report, Sampler, Shading, TexRef, Texture, TextureIx, Wrap,
};
use ufbx::SceneRoot;

/// The longest a name from the file is kept, in characters, in a finding.
const NAME_CHARS: usize = 80;
/// Below this opacity a material is blended rather than opaque.
const OPAQUE_FROM: f64 = 0.999;

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
        if let Some(known) = self.by_texture.get(&texture.element.typed_id) {
            return Ok(known.map(TexRef::of));
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
                        .hint("embed the texture as PNG or JPEG, or import the file with its textures beside it"),
                );
                None
            }
        };
        self.by_texture.insert(texture.element.typed_id, made);
        Ok(made.map(TexRef::of))
    }
}

/// ufbx's wrap mode as the contract's.
fn wrap(mode: ufbx::WrapMode) -> Wrap {
    match mode {
        ufbx::WrapMode::Clamp => Wrap::ClampToEdge,
        ufbx::WrapMode::Repeat => Wrap::Repeat,
    }
}

/// A texture's encoded image: its embedded content, its video's, or the file it names through the resolver.
fn image_bytes(texture: &ufbx::Texture, ctx: &ImportCtx<'_>) -> Result<(Blob, ImageMime), String> {
    let embedded = if texture.content.is_empty() {
        texture.video.as_ref().map(|v| &v.content[..])
    } else {
        Some(&texture.content[..])
    };
    let bytes = if let Some(bytes) = embedded.filter(|b| !b.is_empty()) {
        bytes.to_vec()
    } else {
        let names = [&texture.relative_filename, &texture.filename];
        let name = names.iter().map(|n| n.trim()).find(|n| !n.is_empty()).ok_or("it names no file")?;
        ctx.resolver.read(&name.replace('\\', "/")).map_err(|e| e.to_string())?
    };
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ImageMime::Png
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ImageMime::Jpeg
    } else {
        return Err("its image is neither PNG nor JPEG".to_owned());
    };
    Ok((Blob::new(bytes), mime))
}
