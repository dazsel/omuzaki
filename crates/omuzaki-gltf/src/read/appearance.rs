//! Images, textures and materials.
//!
//! Images keep their original PNG or JPEG bytes, recognised by their signature (the declared `mimeType` is often
//! wrong). An image or texture the scene can't hold (a WebP, a texture whose only source is an extension) is, by
//! default, dropped with a warning and the arenas are compacted: [`TextureMap`] keeps the file-index to scene-index
//! map so materials that referred to a dropped texture lose just that slot. A format that refers to images and
//! textures by file index asks for [`Unusable::Refuse`] instead, and then nothing is ever dropped or renumbered.

use omuzaki_core::glam::{Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    AlphaMode, Blob, EncodedImage, Filter, ImageIx, ImageMime, IoError, Material, NormalTexture, Passthrough, Pbr,
    Sampler, Shading, TexRef, Texture, TextureIx, UvTransform, Wrap,
};

use super::extensions;
use crate::accessor::Accessors;
use crate::buffers::Loader;
use crate::hooks::{Carrier, ImportHooks, Unusable};
use crate::ix32;
use crate::schema::{self as gltf, Document, code};
use crate::tally::Tally;
use crate::uri;

/// The scene's images and textures, and how file indices map onto them.
#[derive(Default)]
pub(crate) struct Appearance {
    pub images: Vec<EncodedImage>,
    pub textures: Vec<Texture>,
    pub map: TextureMap,
}

/// How the file's texture indices map onto the scene's. With [`Unusable::Refuse`] (or when nothing was dropped) it is
/// the identity.
#[derive(Debug, Default, Clone)]
pub struct TextureMap {
    /// Per file texture: its scene index, or `None` if it was dropped.
    map: Vec<Option<TextureIx>>,
}

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The image format of `bytes`, from the file signature (the declared `mimeType` is often wrong or missing).
fn sniff_image(bytes: &[u8]) -> Option<ImageMime> {
    if bytes.starts_with(&PNG_MAGIC) {
        Some(ImageMime::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageMime::Jpeg)
    } else {
        None
    }
}

impl Appearance {
    /// Reads images, then textures.
    ///
    /// # Errors
    /// Whatever loading an image's bytes returns; a texture or image that merely can't be used is dropped and reported.
    pub(crate) fn load(
        doc: &Document,
        acc: &Accessors<'_>,
        loader: &mut Loader<'_>,
        policy: Unusable,
        tally: &mut Tally,
    ) -> Result<Self, IoError> {
        let mut out = Self::default();
        let mut image_map: Vec<Option<ImageIx>> = Vec::with_capacity(doc.images.len());
        for (i, def) in doc.images.iter().enumerate() {
            let at = format!("/images/{i}");
            let bytes = match (&def.uri, def.buffer_view) {
                (Some(uri), _) => Some(loader.read(uri)?),
                (None, Some(view)) => {
                    let slice = acc.view_bytes(view)?;
                    loader.charge(slice.len())?;
                    Some(slice.to_vec())
                }
                (None, None) if policy == Unusable::Refuse => {
                    return Err(IoError::Malformed {
                        message: "the image has neither a bufferView nor a uri".into(),
                        at: Some(at),
                    });
                }
                (None, None) => None,
            };
            let kept = bytes.and_then(|b| sniff_image(&b).map(|mime| (mime, b)));
            if let Some((mime, bytes)) = kept {
                let name = if def.name.is_empty() { uri::file_name(def.uri.as_deref()) } else { def.name.clone() };
                image_map.push(Some(ImageIx(ix32(out.images.len()))));
                out.images.push(EncodedImage::new(name, mime, Blob::new(bytes)));
            } else if policy == Unusable::Refuse {
                return Err(IoError::unsupported(
                    format!("image {i} isn't a PNG or JPEG"),
                    "convert the image to PNG or JPEG",
                ));
            } else {
                let msg =
                    "an image that isn't a PNG or JPEG (or has no data) was dropped, with the textures that use it";
                tally.warn("gltf.image_dropped", msg, at, Some("convert the image to PNG or JPEG"));
                image_map.push(None);
            }
        }
        for (i, def) in doc.textures.iter().enumerate() {
            let sampler = def.sampler.and_then(|s| doc.samplers.get(s)).map_or_else(Sampler::default, sampler_of);
            let image = match def.source {
                None => None, // a texture whose only source is an extension
                // Out of range: keep the index so the scene check reports it with its location.
                Some(source) => match image_map.get(source) {
                    None => Some(ImageIx(ix32(source))),
                    Some(mapped) => *mapped,
                },
            };
            let kept = image.map(|image| {
                out.textures.push(Texture { image, sampler });
                TextureIx(ix32(out.textures.len() - 1))
            });
            if kept.is_none() && policy == Unusable::Refuse {
                return Err(IoError::unsupported(
                    format!("texture {i} has no usable image source"),
                    "textures that only name KHR_texture_basisu or webp sources can't be read",
                ));
            }
            if kept.is_none() {
                let msg = "a texture with no usable image was dropped, and the material slots that use it are cleared";
                tally.warn("gltf.texture_dropped", msg, format!("/textures/{i}"), None);
            }
            out.map.map.push(kept);
        }
        Ok(out)
    }
}

impl TextureMap {
    /// A material slot's texture, or `None` if it was dropped. An index past the file's textures stays out of range so
    /// the scene check reports it with its location. Texture references inside extension bodies (parse them as
    /// [`gltf::TextureInfo`](crate::schema::TextureInfo)) map the same way.
    pub fn tex_ref(&self, info: &gltf::TextureInfo, at: &str, tally: &mut Tally) -> Option<TexRef> {
        let texture = match self.map.get(info.index) {
            None => TextureIx(ix32(info.index)),
            Some(Some(t)) => *t,
            Some(None) => return None,
        };
        let mut uv_set = info.tex_coord.unwrap_or(0);
        let mut uv = None;
        if let Some(t) = &info.extensions.transform {
            uv_set = t.tex_coord.unwrap_or(uv_set);
            if t.rotation.is_some_and(|r| r != 0.0) {
                let msg = "KHR_texture_transform rotation isn't kept (VRM 1.0 recommends against it)";
                tally.warn(
                    "gltf.texture_transform_dropped",
                    msg,
                    format!("{at}/extensions/KHR_texture_transform"),
                    None,
                );
            }
            uv = Some(UvTransform {
                offset: t.offset.map_or(Vec2::ZERO, Vec2::from_array),
                scale: t.scale.map_or(Vec2::ONE, Vec2::from_array),
            });
        }
        for ext in info.extensions.others.keys() {
            tally.info(
                "gltf.extension_dropped",
                format!("texture extension {ext} has no place in the scene"),
                format!("{at}/extensions/{ext}"),
            );
        }
        Some(TexRef { texture, uv_set: u8::try_from(uv_set).unwrap_or(u8::MAX), transform: uv })
    }
}

impl Appearance {
    /// Builds material `index`. The format's own extensions are left for its [`ImportHooks::material`].
    pub(crate) fn material(
        &self,
        index: usize,
        def: &gltf::Material,
        hooks: &mut dyn ImportHooks,
        tally: &mut Tally,
    ) -> Material {
        let at = format!("/materials/{index}");
        let pbr = def.pbr_metallic_roughness.as_ref();
        let slot = |name: &str, info: Option<&gltf::TextureInfo>, tally: &mut Tally| {
            info.and_then(|i| self.map.tex_ref(i, &format!("{at}/{name}"), tally))
        };
        let strength = def.extensions.emissive_strength.as_ref().and_then(|e| e.emissive_strength).unwrap_or(1.0);
        let shading = if def.extensions.unlit.is_some() {
            Shading::Unlit
        } else {
            Shading::Pbr(Pbr {
                metallic: pbr.and_then(|p| p.metallic_factor).unwrap_or(1.0),
                roughness: pbr.and_then(|p| p.roughness_factor).unwrap_or(1.0),
                metallic_roughness_texture: slot(
                    "pbrMetallicRoughness/metallicRoughnessTexture",
                    pbr.and_then(|p| p.metallic_roughness_texture.as_ref()),
                    tally,
                ),
            })
        };
        let mut material = Material::new(def.name.clone(), shading);
        material.base_color = Vec4::from_array(pbr.and_then(|p| p.base_color_factor).unwrap_or([1.0; 4]));
        material.base_color_texture =
            slot("pbrMetallicRoughness/baseColorTexture", pbr.and_then(|p| p.base_color_texture.as_ref()), tally);
        material.emissive = Vec3::from_array(def.emissive_factor.unwrap_or([0.0; 3])) * strength;
        material.emissive_texture = slot("emissiveTexture", def.emissive_texture.as_ref(), tally);
        material.normal_texture = def.normal_texture.as_ref().and_then(|n| {
            self.map
                .tex_ref(n, &format!("{at}/normalTexture"), tally)
                .map(|texture| NormalTexture { texture, scale: n.scale.unwrap_or(1.0) })
        });
        material.alpha = match def.alpha_mode.as_deref() {
            None | Some("OPAQUE") => AlphaMode::Opaque,
            Some("BLEND") => AlphaMode::Blend,
            Some("MASK") => AlphaMode::Mask { cutoff: def.alpha_cutoff.unwrap_or(0.5) },
            Some(other) => {
                tally.warn(
                    "gltf.alpha_mode_unknown",
                    format!("alphaMode {other:?} is unknown; treated as OPAQUE"),
                    format!("{at}/alphaMode"),
                    None,
                );
                AlphaMode::Opaque
            }
        };
        material.double_sided = def.double_sided.unwrap_or(false);
        if def.occlusion_texture.is_some() {
            tally.info(
                "gltf.occlusion_dropped",
                "ambient occlusion textures aren't kept",
                format!("{at}/occlusionTexture"),
            );
        }
        material.passthrough = Passthrough {
            extensions: extensions::carried(def.extensions.others.clone(), Carrier::Material, &at, hooks, tally),
            extras: def.extras.clone(),
        };
        material
    }
}

fn wrap_of(wrap: Option<u32>) -> Wrap {
    match wrap {
        Some(code::CLAMP_TO_EDGE) => Wrap::ClampToEdge,
        Some(code::MIRRORED_REPEAT) => Wrap::MirroredRepeat,
        _ => Wrap::Repeat,
    }
}

/// Maps glTF's GL filter codes. The scene has one minification filter plus a mipmaps flag, so the four mipmap modes
/// reduce to the within-level filter (the first word) and `mipmaps = true`.
pub(crate) fn sampler_of(def: &gltf::Sampler) -> Sampler {
    let mag = if def.mag_filter == Some(code::NEAREST) { Filter::Nearest } else { Filter::Linear };
    let (min, mipmaps) = match def.min_filter {
        Some(code::NEAREST) => (Filter::Nearest, false),
        Some(code::LINEAR) => (Filter::Linear, false),
        Some(code::NEAREST_MIPMAP_NEAREST | code::NEAREST_MIPMAP_LINEAR) => (Filter::Nearest, true),
        _ => (Filter::Linear, true),
    };
    Sampler { wrap_u: wrap_of(def.wrap_s), wrap_v: wrap_of(def.wrap_t), mag, min, mipmaps }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_recognised_by_their_signature_not_their_label() {
        assert_eq!(sniff_image(&PNG_MAGIC), Some(ImageMime::Png));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(ImageMime::Jpeg));
        assert_eq!(sniff_image(b"RIFF....WEBP"), None);
        assert_eq!(sniff_image(&[]), None);
    }

    #[test]
    fn samplers_map_the_gl_codes() {
        let s = sampler_of(&gltf::Sampler {
            mag_filter: Some(9728),
            min_filter: Some(9984),
            wrap_s: Some(33071),
            wrap_t: Some(33648),
            ..gltf::Sampler::default()
        });
        assert_eq!(
            (s.mag, s.min, s.mipmaps, s.wrap_u, s.wrap_v),
            (Filter::Nearest, Filter::Nearest, true, Wrap::ClampToEdge, Wrap::MirroredRepeat)
        );
        assert_eq!(sampler_of(&gltf::Sampler::default()), Sampler::default());
        let plain = sampler_of(&gltf::Sampler { min_filter: Some(9729), ..gltf::Sampler::default() });
        assert_eq!((plain.min, plain.mipmaps), (Filter::Linear, false));
    }
}
