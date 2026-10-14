//! Textures between the character kit's pixels and the PNG `EncodedImage` of a file, lossless for RGBA8.
//!
//! A [`TextureData`] is RGBA8 plus a `TextureDesc`; a file holds a PNG and a glTF sampler. What crosses, field by field:
//!
//! | `TextureDesc` | In the file | Round trip |
//! |---|---|---|
//! | pixels | PNG, RGBA 8-bit | **bit-exact** |
//! | `size` | the PNG header | exact |
//! | `colour_space` | **not in the PNG**: glTF decides by the slot that uses the image ([`slot_colour_space`]) | exact when the caller passes the slot's space back |
//! | `sampler.wrap`, `sampler.filter`, `mips` | the glTF sampler ([`sampler_to_io`]) | exact (one wrap and one filter serve both axes) |
//! | `alpha` | PNG alpha is straight | `Premultiplied` is divided out on encode (lossy where alpha < 255) |
//! | `sampler.anisotropy` | none | lost, reported |
//! | `usage` | none | decoded textures are `Static` |
//!
//! The `image` crate cannot write the PNG `sRGB` or `gAMA` chunks, and glTF readers ignore them anyway ("colour space
//! comes from the slot"), so the flag travels with the material slot, not the file. Decoding accepts every PNG colour
//! type and reduces 16-bit samples to 8 bits, which is exact when every sample is `v * 257` (an 8-bit image stored in
//! 16 bits) and reported when it is not.

use std::io::Cursor;

use image::codecs::png::PngEncoder;
use image::{DynamicImage, ExtendedColorType, ImageEncoder, ImageFormat, ImageReader, Limits};
use omuzaki_char_api::TextureData;
use omuzaki_core::glam::UVec2;
use omuzaki_io_api::{self as io, Blob, EncodedImage, Finding, ImageMime, Report};
use omuzaki_render_api::{AlphaEncoding, ColourSpace, Filter, Sampler, TextureDesc, TextureUsage, Wrap};

/// The largest side a decoded image may have. The renderer guarantees 4096; this leaves room for the big textures
/// that real models ship, and the decoder's allocation cap (512 MiB) refuses anything that would not fit anyway.
const MAX_SIDE: u32 = 16_384;

/// Why a texture could not be converted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextureError {
    /// The pixels could not be turned into a PNG: the texture is inconsistent or the encoder failed.
    #[error("texture '{name}' can't be written as PNG: {reason}")]
    Write {
        /// The texture's name.
        name: String,
        /// What went wrong.
        reason: String,
    },
    /// The image could not be turned into pixels: it is not a PNG, is malformed or is too large.
    #[error("image '{name}' can't be read: {reason}")]
    Read {
        /// The image's name.
        name: String,
        /// What went wrong.
        reason: String,
    },
}

/// The colour space a glTF or `MToon` texture slot expects its image in, by the slot names of
/// `omuzaki_io_api::Material::texture_refs`. Colour slots (base colour, emissive, shade, matcap, rim) are sRGB; data
/// slots (normal, shading shift, masks, metallic-roughness) are linear.
pub fn slot_colour_space(slot: &str) -> ColourSpace {
    match slot {
        "baseColorTexture" | "emissiveTexture" | "shadeMultiplyTexture" | "matcapTexture" | "rimMultiplyTexture" => {
            ColourSpace::Srgb
        }
        _ => ColourSpace::Linear,
    }
}

/// The glTF sampler of a texture: its wrap on both axes, its filter for magnification and minification, and whether it
/// uses mip maps.
pub fn sampler_to_io(desc: &TextureDesc) -> io::Sampler {
    let wrap = match desc.sampler.wrap {
        Wrap::Clamp => io::Wrap::ClampToEdge,
        Wrap::Repeat => io::Wrap::Repeat,
    };
    let filter = match desc.sampler.filter {
        Filter::Linear => io::Filter::Linear,
        Filter::Nearest => io::Filter::Nearest,
    };
    io::Sampler { wrap_u: wrap, wrap_v: wrap, mag: filter, min: filter, mipmaps: desc.mips }
}

/// Encodes `texture` as a PNG named `name`. `at` locates findings (for example `images/2`).
///
/// RGBA8 straight-alpha pixels come back byte for byte from [`decode_png`]. Premultiplied pixels are divided out first
/// because PNG stores straight alpha (reported: the division rounds, and a fully transparent pixel loses its colour).
/// Encoding is deterministic: the same texture gives the same bytes.
///
/// # Errors
/// [`TextureError::Write`] if the texture is inconsistent (`TextureData::validate`) or the encoder fails.
pub fn encode_png(
    name: &str,
    texture: &TextureData,
    at: &str,
    report: &mut Report,
) -> Result<EncodedImage, TextureError> {
    let write = |reason: String| TextureError::Write { name: name.to_owned(), reason };
    texture.validate().map_err(write)?;
    let desc = &texture.desc;
    if desc.alpha == AlphaEncoding::Premultiplied {
        let msg = format!("texture '{name}' has premultiplied alpha; PNG stores straight alpha, so it was divided out");
        report.push(
            Finding::warning("texture.unpremultiplied", msg).at(at).hint("paint with straight alpha to export exactly"),
        );
    }
    if desc.sampler.anisotropy > 1 {
        let msg = format!(
            "texture '{name}' asks for {}x anisotropic filtering; glTF samplers have no such setting",
            desc.sampler.anisotropy
        );
        report.push(Finding::info("texture.anisotropy_dropped", msg).at(format!("{at}/sampler/anisotropy")));
    }
    let straight;
    let pixels = if desc.alpha == AlphaEncoding::Premultiplied {
        straight = unpremultiply(&texture.rgba8);
        &straight
    } else {
        &texture.rgba8
    };
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(pixels, desc.size.x, desc.size.y, ExtendedColorType::Rgba8)
        .map_err(|e| write(e.to_string()))?;
    Ok(EncodedImage::new(name, ImageMime::Png, Blob::new(bytes)))
}

/// Decodes `image` into a static, straight-alpha RGBA8 texture whose colour space is `colour_space` (the slot's, see
/// [`slot_colour_space`]) and whose sampling comes from `sampler`. `at` locates findings.
///
/// Every PNG colour type is accepted; grey, palette and RGB images are expanded to RGBA with opaque alpha, which is
/// exact. A 16-bit PNG is reduced to 8 bits and reported unless the reduction changed nothing. The renderer has one
/// wrap and one filter per texture, so a sampler that differs per axis (or between magnification and minification)
/// is approximated and reported, as is mirrored repeat.
///
/// # Errors
/// [`TextureError::Read`] if the image is not a PNG, is malformed or exceeds the size limit.
pub fn decode_png(
    image: &EncodedImage,
    colour_space: ColourSpace,
    sampler: &io::Sampler,
    at: &str,
    report: &mut Report,
) -> Result<TextureData, TextureError> {
    let read = |reason: String| TextureError::Read { name: image.name.clone(), reason };
    if image.mime != ImageMime::Png {
        return Err(read(format!("{} isn't supported; only PNG is", image.mime.media_type())));
    }
    let mut reader = ImageReader::with_format(Cursor::new(image.bytes.as_slice()), ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|e| read(e.to_string()))?;
    if reduces_depth(&decoded) {
        let msg = format!("image '{}' has 16 bits per channel; it was reduced to 8", image.name);
        report.push(Finding::info("texture.depth_reduced", msg).at(at));
    }
    let rgba = decoded.into_rgba8();
    let size = UVec2::new(rgba.width(), rgba.height());
    let desc = TextureDesc {
        size,
        colour_space,
        alpha: AlphaEncoding::Straight,
        mips: sampler.mipmaps,
        usage: TextureUsage::Static,
        sampler: sampler_from_io(*sampler, &image.name, at, report),
    };
    Ok(TextureData { desc, rgba8: rgba.into_raw() })
}

/// True if `image` has 16-bit samples that are not all `v * 257`, the only ones an 8-bit image can hold exactly.
fn reduces_depth(image: &DynamicImage) -> bool {
    match image {
        DynamicImage::ImageLuma16(_)
        | DynamicImage::ImageLumaA16(_)
        | DynamicImage::ImageRgb16(_)
        | DynamicImage::ImageRgba16(_) => image.to_rgba16().as_raw().iter().any(|v| v % 257 != 0),
        _ => false,
    }
}

/// The renderer's sampler for a glTF one, reporting what has no counterpart.
fn sampler_from_io(sampler: io::Sampler, name: &str, at: &str, report: &mut Report) -> Sampler {
    let mut note = |field: &str, message: String| {
        report.push(Finding::info("texture.sampler_approximated", message).at(format!("{at}/sampler/{field}")));
    };
    if sampler.wrap_u != sampler.wrap_v {
        note("wrap", format!("image '{name}' wraps differently along U and V; the renderer has one wrap, U was used"));
    }
    let wrap = match sampler.wrap_u {
        io::Wrap::ClampToEdge => Wrap::Clamp,
        io::Wrap::Repeat => Wrap::Repeat,
        io::Wrap::MirroredRepeat => {
            note("wrap", format!("image '{name}' mirrors its repeat; the renderer can only repeat plainly"));
            Wrap::Repeat
        }
    };
    if sampler.mag != sampler.min {
        note(
            "filter",
            format!("image '{name}' filters magnification and minification differently; magnification was used"),
        );
    }
    let filter = match sampler.mag {
        io::Filter::Linear => Filter::Linear,
        io::Filter::Nearest => Filter::Nearest,
    };
    Sampler { filter, wrap, anisotropy: 1 }
}

/// Straight RGBA from premultiplied RGBA, rounding to the nearest byte. A transparent pixel has no colour left.
fn unpremultiply(rgba8: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba8.len());
    for px in rgba8.as_chunks::<4>().0 {
        let alpha = u32::from(px[3]);
        for &c in &px[..3] {
            let straight = (u32::from(c) * 255 + alpha / 2).checked_div(alpha).unwrap_or(0);
            // Premultiplied colour never exceeds alpha, but a hand-made texture might; clamp rather than wrap.
            out.push(u8::try_from(straight.min(255)).unwrap_or(u8::MAX));
        }
        out.push(px[3]);
    }
    out
}

#[cfg(test)]
mod tests {
    use image::{ImageBuffer, Rgba};

    use super::*;

    fn png16(pixels: &[[u16; 4]]) -> EncodedImage {
        let raw: Vec<u16> = pixels.iter().flatten().copied().collect();
        let buffer: ImageBuffer<Rgba<u16>, Vec<u16>> =
            ImageBuffer::from_raw(u32::try_from(pixels.len()).unwrap(), 1, raw).unwrap();
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba16(buffer).write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
        EncodedImage::new("deep", ImageMime::Png, Blob::new(bytes))
    }

    #[test]
    fn unpremultiplying_inverts_premultiplying_for_opaque_and_clamps_nonsense() {
        assert_eq!(unpremultiply(&[10, 20, 30, 255]), [10, 20, 30, 255]);
        assert_eq!(unpremultiply(&[50, 0, 100, 100]), [128, 0, 255, 100], "50 * 255 / 100 rounds to 128");
        assert_eq!(unpremultiply(&[9, 9, 9, 0]), [0, 0, 0, 0], "a transparent pixel has no colour");
        assert_eq!(unpremultiply(&[200, 0, 0, 100]), [255, 0, 0, 100], "colour above alpha is clamped");
    }

    #[test]
    fn a_16_bit_image_of_8_bit_values_decodes_without_a_finding() {
        let mut report = Report::new();
        let image = png16(&[[0, 257, 255 * 257, 128 * 257], [257, 2 * 257, 3 * 257, 4 * 257]]);
        let data = decode_png(&image, ColourSpace::Srgb, &io::Sampler::default(), "images/0", &mut report).unwrap();
        assert_eq!(data.rgba8, [0, 1, 255, 128, 1, 2, 3, 4]);
        assert_eq!(report, Report::new());
    }

    #[test]
    fn a_16_bit_image_with_finer_values_is_reduced_and_reported_once() {
        let mut report = Report::new();
        let image = png16(&[[256, 0, 0, 65535], [65534, 129, 128, 65535]]);
        let data = decode_png(&image, ColourSpace::Linear, &io::Sampler::default(), "images/1", &mut report).unwrap();
        assert_eq!(data.rgba8.len(), 8);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].rule, "texture.depth_reduced");
        assert_eq!(report.findings[0].location.as_deref(), Some("images/1"));
    }
}
