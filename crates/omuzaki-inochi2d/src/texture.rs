//! Texture slots decoded to RGBA8.
//!
//! `Inochi2D`'s textures are stored with their alpha already premultiplied: Inochi Creator premultiplies the art when it
//! imports it (v0.8.6 `io/psd.d`, `io/package.d`), `Inochi2D` 0.8 uploads the pixels as they are and blends them with
//! `GL_ONE, GL_ONE_MINUS_SRC_ALPHA` (`core/texture.d`, `core/nodes/common.d`), and 0.9 loads them with its
//! "no premultiply" flag. So the pixels are kept as stored, which is already what [`omuzaki_puppet_api::TextureRef`]
//! asks for; premultiplying again would darken every soft edge.

use std::io::Cursor;

use image::{ImageFormat, ImageReader};

use crate::container::{RawTexture, TextureEncoding};
use crate::error::{ImportError, Limits, limit, malformed};
use crate::report::{FindingKind, ImportReport, rule};

/// PNG's signature.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// One decoded texture: RGBA8, sRGB colour, premultiplied alpha, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texture {
    /// Width and height, pixels.
    pub size: [u32; 2],
    /// `size[0] * size[1] * 4` bytes.
    pub rgba: Vec<u8>,
}

impl Texture {
    /// A 1x1 transparent texture, which stands in for a slot that can't be decoded.
    #[must_use]
    pub fn placeholder() -> Self {
        Self { size: [1, 1], rgba: vec![0; 4] }
    }
}

/// Decodes every slot. A slot that can't be read in principle (empty, BC7, an unknown encoding) gets a placeholder
/// and a finding; bytes that claim a format and don't decode as it are malformed.
pub(crate) fn decode_all(
    raw: &[RawTexture<'_>],
    limits: &Limits,
    report: &mut ImportReport,
) -> Result<Vec<Texture>, ImportError> {
    let mut out = Vec::with_capacity(raw.len());
    let mut budget = limits.max_decoded_bytes;
    for (i, tex) in raw.iter().enumerate() {
        let path = format!("TEX_SECT/{i}");
        let format = if tex.data.is_empty() {
            report.add(
                rule::TEXTURE_EMPTY,
                FindingKind::Synthesised,
                &path,
                "an empty slot: a transparent 1x1 texture",
            );
            None
        } else if tex.data.starts_with(&PNG_SIGNATURE) {
            Some(ImageFormat::Png)
        } else {
            match tex.encoding {
                // TGA has no signature; Inochi2D decodes by content, so a PNG-labelled slot that isn't PNG is tried as
                // TGA too.
                TextureEncoding::Tga | TextureEncoding::Png => Some(ImageFormat::Tga),
                TextureEncoding::Bc7 => {
                    report.add(
                        rule::TEXTURE_BC7,
                        FindingKind::Unsupported,
                        &path,
                        "BC7 blocks: no permissive decoder in the workspace yet, so a transparent 1x1 texture stands in",
                    );
                    None
                }
                TextureEncoding::Other(code) => {
                    report.add(
                        rule::TEXTURE_ENCODING,
                        FindingKind::Unsupported,
                        &path,
                        format!("encoding {code}: a transparent 1x1 texture stands in"),
                    );
                    None
                }
            }
        };
        let texture = match format {
            Some(format) => decode(tex.data, format, &path, limits, &mut budget)?,
            None => Texture::placeholder(),
        };
        out.push(texture);
    }
    Ok(out)
}

fn decode(
    data: &[u8],
    format: ImageFormat,
    path: &str,
    limits: &Limits,
    budget: &mut u64,
) -> Result<Texture, ImportError> {
    let fail = |e: image::ImageError| malformed(rule::TEXTURE_DECODE, path, format!("{format:?}: {e}"));
    let (w, h) = ImageReader::with_format(Cursor::new(data), format).into_dimensions().map_err(fail)?;
    if w == 0 || h == 0 {
        return Err(malformed(rule::TEXTURE_DECODE, path, format!("a {w} x {h} image")));
    }
    if w > limits.max_texture_dim || h > limits.max_texture_dim {
        return Err(limit("max_texture_dim", path, format!("a {w} x {h} texture")));
    }
    let bytes = u64::from(w) * u64::from(h) * 4;
    if bytes > *budget {
        return Err(limit("max_decoded_bytes", path, format!("{bytes} more decoded bytes, {budget} left")));
    }
    *budget -= bytes;
    let mut reader = ImageReader::with_format(Cursor::new(data), format);
    let mut bounds = image::Limits::default();
    bounds.max_image_width = Some(w);
    bounds.max_image_height = Some(h);
    // Room for a 16-bit RGBA intermediate, which some PNGs decode through.
    bounds.max_alloc = Some(bytes.saturating_mul(2).max(1 << 20));
    reader.limits(bounds);
    let image = reader.decode().map_err(fail)?.into_rgba8();
    if image.dimensions() != (w, h) {
        return Err(malformed(rule::TEXTURE_DECODE, path, "the decoded size differs from the header's"));
    }
    Ok(Texture { size: [w, h], rgba: image.into_raw() })
}
