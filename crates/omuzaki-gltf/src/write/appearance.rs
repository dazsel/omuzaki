//! Images, samplers, textures and materials.

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    AlphaMode, Filter, IoScene, Layout, Material, OutputFile, Sampler, Severity, Shading, TexRef, Wrap,
};

use super::Writer;
use crate::hooks::ExportHooks;
use crate::schema::{
    self as gltf, EmissiveStrength, Empty, MaterialExtensions, Pbr as GltfPbr, TextureInfo, TextureInfoExtensions,
    TextureTransform, code,
};
use crate::tally::Tally;

impl Writer<'_> {
    /// Images go last in the buffer, after the geometry, so the vertex data stays at the front of the file.
    pub(super) fn images(&mut self) {
        for (i, image) in self.scene.images.iter().enumerate() {
            let media = image.mime.media_type();
            let mut def =
                gltf::Image { name: image.name.clone(), mime_type: Some(media.into()), ..gltf::Image::default() };
            match self.out.layout {
                Layout::Binary => def.buffer_view = Some(self.out.pack.view(image.bytes.as_slice(), None)),
                Layout::Separate => {
                    let ext = match image.mime {
                        omuzaki_io_api::ImageMime::Png => "png",
                        omuzaki_io_api::ImageMime::Jpeg => "jpg",
                    };
                    let path = format!("image_{i}.{ext}");
                    self.out.files.push(OutputFile {
                        path: path.clone(),
                        media_type: media.into(),
                        bytes: image.bytes.as_slice().to_vec(),
                    });
                    def.uri = Some(path);
                }
            }
            self.out.document.images.push(def);
        }
    }

    /// Textures, with identical samplers shared (first occurrence first).
    pub(super) fn textures(&mut self) {
        for texture in &self.scene.textures {
            let def = sampler_def(texture.sampler);
            let doc = &mut self.out.document;
            let sampler = doc.samplers.iter().position(|s| *s == def).unwrap_or_else(|| {
                doc.samplers.push(def);
                doc.samplers.len() - 1
            });
            doc.textures.push(gltf::Texture {
                sampler: Some(sampler),
                source: Some(texture.image.index()),
                ..gltf::Texture::default()
            });
        }
    }

    pub(super) fn materials(&mut self) {
        for (i, material) in self.scene.materials.iter().enumerate() {
            let mut def = self.material(i, material);
            self.hooks.material(i, material, &mut def, &mut self.out.tally);
            self.out.document.materials.push(def);
        }
    }

    fn material(&mut self, index: usize, m: &Material) -> gltf::Material {
        let mut extensions = MaterialExtensions::default();
        let (metallic, roughness, mr_texture) = match &m.shading {
            Shading::Pbr(p) => (p.metallic, p.roughness, p.metallic_roughness_texture),
            Shading::Unlit => {
                extensions.unlit = Some(Empty::default());
                (0.0, 1.0, None)
            }
            // MToon has no glTF core form: a matte PBR stand-in keeps the lit colour and texture (reported by `check`).
            Shading::Toon(_) => (0.0, 1.0, None),
        };
        let (emissive, strength) = split_emissive(m.emissive);
        if let Some(strength) = strength {
            extensions.emissive_strength = Some(EmissiveStrength { emissive_strength: Some(strength) });
        }
        let (alpha_mode, alpha_cutoff) = match m.alpha {
            AlphaMode::Opaque => ("OPAQUE", None),
            AlphaMode::Mask { cutoff } => ("MASK", Some(cutoff)),
            AlphaMode::Blend => ("BLEND", None),
        };
        self.carry(&m.passthrough, &mut extensions.others, &format!("/materials/{index}"));
        gltf::Material {
            name: m.name.clone(),
            pbr_metallic_roughness: Some(GltfPbr {
                base_color_factor: Some(m.base_color.to_array()),
                base_color_texture: m.base_color_texture.as_ref().map(|t| texture_info(t, None)),
                metallic_factor: Some(metallic),
                roughness_factor: Some(roughness),
                metallic_roughness_texture: mr_texture.as_ref().map(|t| texture_info(t, None)),
                ..GltfPbr::default()
            }),
            normal_texture: m.normal_texture.as_ref().map(|n| texture_info(&n.texture, Some(n.scale))),
            emissive_texture: m.emissive_texture.as_ref().map(|t| texture_info(t, None)),
            emissive_factor: (emissive != Vec3::ZERO).then(|| emissive.to_array()),
            alpha_mode: Some(alpha_mode.into()),
            alpha_cutoff,
            double_sided: Some(m.double_sided),
            extensions,
            extras: m.passthrough.extras.clone(),
            ..gltf::Material::default()
        }
    }
}

/// The glTF texture reference for a scene texture reference: the texture's index, `scale` (a normal texture's), and a
/// `KHR_texture_transform` when the reference has a UV transform.
///
/// The writer builds every texture slot of a material with this, and a format that writes texture references inside
/// its own extension (`MToon`'s `shadeMultiplyTexture`, `matcapTexture`, ...) should too, so a transform is written the
/// same way everywhere. Turn it into the extension body's JSON with [`to_json`](crate::to_json), which keeps `f32`
/// numbers short. The texture index is the scene's: the writer writes every texture in scene order, so it is also
/// the file's.
pub fn texture_info(t: &TexRef, scale: Option<f32>) -> TextureInfo {
    // Only UV set 0 exists in the scene's meshes, so that is the only set a texture can read.
    let transform = t.transform.map(|uv| TextureTransform {
        offset: Some(uv.offset.to_array()),
        scale: Some(uv.scale.to_array()),
        ..TextureTransform::default()
    });
    TextureInfo {
        index: t.texture.index(),
        scale,
        extensions: TextureInfoExtensions { transform, ..TextureInfoExtensions::default() },
        ..TextureInfo::default()
    }
}

/// The glTF sampler for a scene sampler, every field written so no reader has to guess a default. Mipmapped
/// minification maps to the `LINEAR`/`NEAREST` within-level filters the reader reads back (`NEAREST_MIPMAP_NEAREST`,
/// `LINEAR_MIPMAP_LINEAR`).
fn sampler_def(s: Sampler) -> gltf::Sampler {
    let wrap = |w: Wrap| match w {
        Wrap::Repeat => code::REPEAT,
        Wrap::MirroredRepeat => code::MIRRORED_REPEAT,
        Wrap::ClampToEdge => code::CLAMP_TO_EDGE,
    };
    let mag = match s.mag {
        Filter::Nearest => code::NEAREST,
        Filter::Linear => code::LINEAR,
    };
    let min = match (s.min, s.mipmaps) {
        (Filter::Nearest, false) => code::NEAREST,
        (Filter::Linear, false) => code::LINEAR,
        (Filter::Nearest, true) => code::NEAREST_MIPMAP_NEAREST,
        (Filter::Linear, true) => code::LINEAR_MIPMAP_LINEAR,
    };
    gltf::Sampler {
        mag_filter: Some(mag),
        min_filter: Some(min),
        wrap_s: Some(wrap(s.wrap_u)),
        wrap_t: Some(wrap(s.wrap_v)),
        ..gltf::Sampler::default()
    }
}

/// Splits an emissive colour into glTF's two parts. `emissiveFactor` must stay within 0..1, so a colour with a channel
/// above 1 is written as a factor times `KHR_materials_emissive_strength`, and the strength is the colour's peak (its
/// brightest channel): the factor's brightest channel is exactly 1, and the strength reads as the intensity an author
/// would give it (a colour of `(3, 1, 0)` is strength 3, factor `(1, 1/3, 0)`).
///
/// The reader multiplies factor by strength in `f32`, and that product rounds, so a factor is not always the quotient.
/// Each channel's factor is the smallest one whose product reaches the channel, which makes two things hold:
///
/// - a colour the reader produced from a file whose strength is its own peak reads back bit for bit;
/// - the split is *stable*: splitting what the reader read back from a split gives the same factor and strength,
///   so `export(import(export(x))) == export(x)`.
///
/// A colour that no factor times its peak can reach reads back as the next colour above it that one can: at most a couple of
/// ulps, which no render can show.
pub fn split_emissive(emissive: Vec3) -> (Vec3, Option<f32>) {
    let peak = emissive.max_element();
    if !peak.is_finite() || peak <= 1.0 {
        return (emissive, None);
    }
    (Vec3::from_array(emissive.to_array().map(|channel| factor_for(channel, peak))), Some(peak))
}

/// The factor to write for one channel of a colour whose peak is `strength` (see [`split_emissive`]): the smallest `f`
/// in 0..=1 with `f * strength >= channel` in `f32`. The quotient is within an ulp of it, so each search takes a step
/// or two.
fn factor_for(channel: f32, strength: f32) -> f32 {
    if channel <= 0.0 {
        // Black, or a negative channel (nothing to write a factor for; kept as the sign it had).
        return channel / strength;
    }
    let reaches = |f: f32| f * strength >= channel;
    let mut f = (channel / strength).min(1.0);
    while f < 1.0 && !reaches(f) {
        f = f.next_up();
    }
    while f > 0.0 && reaches(f.next_down()) {
        f = f.next_down();
    }
    // A channel that rounds up to the peak is the peak: write exactly 1, so the factor's brightest channel always is.
    if f * strength >= strength { 1.0 } else { f }
}

/// What a glTF material can't say, and numbers that would corrupt the JSON.
pub(super) fn check(scene: &IoScene, hooks: &mut dyn ExportHooks, tally: &mut Tally) {
    for (i, m) in scene.materials.iter().enumerate() {
        let at = format!("materials/{i}");
        hooks.check_material(i, m, tally);
        let finite = m.base_color.is_finite()
            && m.emissive.is_finite()
            && match m.shading {
                Shading::Pbr(p) => p.metallic.is_finite() && p.roughness.is_finite(),
                Shading::Unlit | Shading::Toon(_) => true,
            }
            && m.normal_texture.is_none_or(|n| n.scale.is_finite())
            && !matches!(m.alpha, AlphaMode::Mask { cutoff } if !cutoff.is_finite());
        let transforms_finite = m
            .texture_refs()
            .iter()
            .all(|(_, t)| t.transform.is_none_or(|uv| uv.offset.is_finite() && uv.scale.is_finite()));
        if !(finite && transforms_finite) {
            tally.note(
                Severity::Error,
                "gltf.non_finite",
                "a material contains NaN or infinity, which JSON can't hold",
                &at,
                None,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::rng::Rng;

    use super::*;

    /// What the reader makes of a split: factor times strength, in `f32`.
    fn read_back((factor, strength): (Vec3, Option<f32>)) -> Vec3 {
        factor * strength.unwrap_or(1.0)
    }

    #[test]
    fn the_strength_is_the_peak_channel() {
        let (factor, strength) = split_emissive(Vec3::new(3.0, 1.0, 0.0));
        assert_eq!(strength, Some(3.0), "the peak, not the next power of two");
        assert_eq!(factor, Vec3::new(1.0, 1.0 / 3.0, 0.0));
        assert_eq!(read_back((factor, strength)), Vec3::new(3.0, 1.0, 0.0));
        // Colours within the factor's range are written as they are, with no extension.
        for e in [Vec3::new(1.0, 0.5, 0.0), Vec3::ZERO, Vec3::splat(1.0)] {
            assert_eq!(split_emissive(e), (e, None));
        }
        assert_eq!(split_emissive(Vec3::splat(4.0)), (Vec3::ONE, Some(4.0)));
        assert_eq!(split_emissive(Vec3::new(1.000_000_1, 0.0, 0.0)), (Vec3::new(1.0, 0.0, 0.0), Some(1.000_000_1)));
        let (huge, strength) = split_emissive(Vec3::splat(f32::MAX));
        assert_eq!((huge, strength), (Vec3::ONE, Some(f32::MAX)));
        assert_eq!(split_emissive(Vec3::new(f32::NAN, 0.0, 0.0)).1, None, "nothing sensible to split");
    }

    #[test]
    fn a_split_colour_reads_back_and_splits_the_same_again() {
        let mut rng = Rng::new(11);
        let (mut exact, mut total) = (0, 0);
        for i in 0..50_000 {
            // Peaks from just over 1 to a million, one channel at the peak and the others anywhere below it.
            let peak = 1.001 + rng.next_f32().powi(3) * if i % 2 == 0 { 8.0 } else { 1e6 };
            let mut e = Vec3::new(rng.next_f32(), rng.next_f32(), rng.next_f32()) * peak;
            // A product of two floats is a colour some factor reaches; a nudge of an ulp or two makes it one that may not be.
            e = e
                .to_array()
                .map(|c| f32::from_bits(c.to_bits() + u32::try_from(rng.next_u64() % 4).unwrap()).min(peak))
                .into();
            e[usize::try_from(rng.next_u64() % 3).unwrap()] = peak;
            let (factor, strength) = split_emissive(e);
            let strength = strength.expect("a channel above 1");
            assert_eq!(strength, peak, "{e:?}");
            assert_eq!(factor.max_element(), 1.0, "the brightest factor is exactly 1: {e:?}");
            assert!(factor.min_element() >= 0.0 && factor.max_element() <= 1.0, "{factor:?}");
            let back = read_back((factor, Some(strength)));
            for (b, w) in back.to_array().into_iter().zip(e.to_array()) {
                assert!((b - w).abs() <= 2.0 * f32::EPSILON * w, "{e:?} read back as {back:?}");
                total += 1;
                exact += usize::from(b.to_bits() == w.to_bits());
            }
            // Stable: what the reader reads back from this file splits into the same file.
            assert_eq!(split_emissive(back), (factor, Some(strength)), "{e:?} -> {back:?}");
        }
        assert!(exact * 10 > total * 9, "only {exact} of {total} channels read back bit for bit");
    }

    #[test]
    fn a_colour_read_from_a_file_whose_strength_is_its_peak_reads_back_bit_for_bit() {
        let mut rng = Rng::new(12);
        for _ in 0..50_000 {
            // A file as another tool writes it: some factor with a channel at 1, times a strength.
            let strength = 1.0 + rng.next_f32() * 20.0;
            let mut factor = Vec3::new(rng.next_f32(), rng.next_f32(), rng.next_f32());
            factor[usize::try_from(rng.next_u64() % 3).unwrap()] = 1.0;
            let colour = factor * strength;
            assert_eq!(read_back(split_emissive(colour)), colour, "factor {factor:?} strength {strength}");
        }
    }

    #[test]
    fn samplers_round_trip_through_the_codes_the_importer_reads() {
        for sampler in [
            Sampler::default(),
            Sampler {
                wrap_u: Wrap::ClampToEdge,
                wrap_v: Wrap::MirroredRepeat,
                mag: Filter::Nearest,
                min: Filter::Nearest,
                mipmaps: false,
            },
            Sampler { min: Filter::Nearest, mipmaps: true, ..Sampler::default() },
        ] {
            let def = sampler_def(sampler);
            let back = crate::read::sampler_of(&def);
            assert_eq!(back, sampler);
        }
    }
}
