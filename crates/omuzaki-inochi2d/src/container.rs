//! The container: magic, payload, texture blobs and vendor data, for INP1 and INP2.
//!
//! INP1 (`TRNSRTS\0`, `Inochi2D` 0.8's `.inp` and Inochi Creator's `.inx`) is big-endian: the magic, a `u32` payload
//! length and the JSON payload, then `TEX_SECT` (a `u32` count, then per texture a `u32` length, a `u8` encoding and the
//! bytes) and optionally `EXT_SECT` (a `u32` count, then per entry a `u32` name length, the name, a `u32` data length and
//! the data). INP2 (`TRNSRTS2`) is one tagged little-endian tree (see [`crate::inp2`]) whose root object holds
//! `INP_SECT`, `TEX_SECT` and `EXT_SECT`.
//!
//! Everything borrows the input: nothing here copies a texture or the payload.

use crate::error::{ImportError, Limits, limit, malformed};
use crate::inp2::{self, DataNode};
use crate::report::rule;

/// INP1's magic bytes.
pub const INP1_MAGIC: [u8; 8] = *b"TRNSRTS\0";
/// INP2's magic bytes.
pub const INP2_MAGIC: [u8; 8] = *b"TRNSRTS2";
/// The texture section's tag.
pub const TEX_SECT: [u8; 8] = *b"TEX_SECT";
/// The vendor data section's tag.
pub const EXT_SECT: [u8; 8] = *b"EXT_SECT";
/// INP2's payload key.
pub const INP_SECT: &str = "INP_SECT";

/// Which container a file is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// `TRNSRTS\0`: the JSON payload of `Inochi2D` 0.8.
    Inp1,
    /// `TRNSRTS2`: the tagged tree of `Inochi2D` 0.9.
    Inp2,
}

/// A texture slot's declared encoding.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TextureEncoding {
    /// 0: PNG.
    Png,
    /// 1: Truevision TGA.
    Tga,
    /// 2: BC7 (BPTC) blocks.
    Bc7,
    /// Anything else.
    Other(u32),
}

impl TextureEncoding {
    /// The encoding for a container's code.
    #[must_use]
    pub const fn from_code(code: u32) -> Self {
        match code {
            0 => Self::Png,
            1 => Self::Tga,
            2 => Self::Bc7,
            other => Self::Other(other),
        }
    }

    /// The container's code for it.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::Png => 0,
            Self::Tga => 1,
            Self::Bc7 => 2,
            Self::Other(code) => code,
        }
    }
}

/// One texture slot, still encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawTexture<'a> {
    /// What the file says it is.
    pub encoding: TextureEncoding,
    /// The encoded bytes (empty for a slot `Inochi2D` 0.8 wrote without data).
    pub data: &'a [u8],
}

/// One vendor data entry (`EXT_SECT`), kept as bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VendorData<'a> {
    /// The vendor's key (reverse domain notation by convention).
    pub name: &'a str,
    /// The bytes, uninterpreted.
    pub data: &'a [u8],
}

/// Bytes after the known sections, kept as they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownSection<'a> {
    /// The byte offset where they start.
    pub offset: usize,
    /// The section's 8-byte tag, if at least 8 bytes are left.
    pub tag: Option<[u8; 8]>,
    /// Everything from the tag to the end of the file (a section of unknown layout has no length to stop at).
    pub data: &'a [u8],
}

/// The puppet payload, still unparsed (INP1) or as a tree (INP2).
#[derive(Clone, Debug, PartialEq)]
pub enum Payload<'a> {
    /// INP1's JSON text.
    Json(&'a [u8]),
    /// INP2's `INP_SECT` object.
    Tree(DataNode<'a>),
}

/// A container, read but not interpreted.
#[derive(Clone, Debug, PartialEq)]
pub struct Container<'a> {
    /// Which container.
    pub format: Format,
    /// The puppet.
    pub payload: Payload<'a>,
    /// The texture slots, in file order (parts name them by index).
    pub textures: Vec<RawTexture<'a>>,
    /// Whether the file had a texture section at all.
    pub has_texture_section: bool,
    /// Vendor data, in file order.
    pub vendor: Vec<VendorData<'a>>,
    /// Sections this reader doesn't know.
    pub unknown: Vec<UnknownSection<'a>>,
}

/// A bounds-checked reader over the input.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8], pos: usize) -> Self {
        Self { bytes, pos }
    }

    pub(crate) const fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    /// The next `n` bytes; a length past the end is malformed, and nothing is allocated for it.
    pub(crate) fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8], ImportError> {
        if n > self.remaining() {
            return Err(malformed(
                rule::TRUNCATED,
                format!("@{}", self.pos),
                format!("{what}: {n} bytes declared, {} left", self.remaining()),
            ));
        }
        let start = self.pos;
        self.pos += n;
        Ok(&self.bytes[start..self.pos])
    }

    pub(crate) fn array<const N: usize>(&mut self, what: &str) -> Result<[u8; N], ImportError> {
        let bytes = self.take(N, what)?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    pub(crate) fn u8(&mut self, what: &str) -> Result<u8, ImportError> {
        Ok(self.array::<1>(what)?[0])
    }

    pub(crate) fn u32_be(&mut self, what: &str) -> Result<u32, ImportError> {
        self.array::<4>(what).map(u32::from_be_bytes)
    }

    pub(crate) fn u32_le(&mut self, what: &str) -> Result<u32, ImportError> {
        self.array::<4>(what).map(u32::from_le_bytes)
    }

    pub(crate) fn peek_u32_le(&self) -> Option<u32> {
        let end = self.pos.checked_add(4)?;
        let bytes = self.bytes.get(self.pos..end)?;
        let mut out = [0u8; 4];
        out.copy_from_slice(bytes);
        Some(u32::from_le_bytes(out))
    }

    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let start = self.pos.min(self.bytes.len());
        self.pos = self.bytes.len();
        &self.bytes[start..]
    }
}

fn utf8<'a>(bytes: &'a [u8], path: &str) -> Result<&'a str, ImportError> {
    std::str::from_utf8(bytes).map_err(|e| malformed(rule::UTF8, path, e.to_string()))
}

/// Which container `bytes` is, from its magic.
///
/// # Errors
/// [`ImportError::Malformed`] (`inochi2d.magic`) when it is neither.
pub fn detect(bytes: &[u8]) -> Result<Format, ImportError> {
    match bytes.get(..8) {
        Some(m) if m == INP1_MAGIC => Ok(Format::Inp1),
        Some(m) if m == INP2_MAGIC => Ok(Format::Inp2),
        _ => Err(malformed(rule::MAGIC, "@0", "not an Inochi2D puppet (no TRNSRTS magic)")),
    }
}

/// Reads a container without interpreting its payload or decoding its textures.
///
/// # Errors
/// [`ImportError::Limit`] when a declared size passes `limits`, [`ImportError::Malformed`] when the bytes are out of
/// shape. Both are found before anything is allocated for the declared size.
pub fn read_container<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Container<'a>, ImportError> {
    if bytes.len() as u64 > limits.max_bytes {
        return Err(limit("max_bytes", "@0", format!("a {} byte file", bytes.len())));
    }
    match detect(bytes)? {
        Format::Inp1 => read_inp1(bytes, limits),
        Format::Inp2 => read_inp2(bytes, limits),
    }
}

fn read_inp1<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Container<'a>, ImportError> {
    let mut r = Reader::new(bytes, 8);
    let len = r.u32_be("the payload length")?;
    if u64::from(len) > limits.max_payload_bytes {
        return Err(limit("max_payload_bytes", "@8", format!("a {len} byte payload")));
    }
    let payload = r.take(len as usize, "the JSON payload")?;
    let mut container = Container {
        format: Format::Inp1,
        payload: Payload::Json(payload),
        textures: Vec::new(),
        has_texture_section: false,
        vendor: Vec::new(),
        unknown: Vec::new(),
    };
    let mut seen_ext = false;
    while r.remaining() > 0 {
        let offset = r.pos();
        if r.remaining() < 8 {
            container.unknown.push(UnknownSection { offset, tag: None, data: r.rest() });
            break;
        }
        let tag = r.array::<8>("a section tag")?;
        match tag {
            TEX_SECT if !container.has_texture_section => {
                container.has_texture_section = true;
                read_textures_inp1(&mut r, limits, &mut container.textures)?;
            }
            EXT_SECT if !seen_ext => {
                seen_ext = true;
                read_vendor_inp1(&mut r, &mut container.vendor)?;
            }
            TEX_SECT | EXT_SECT => {
                return Err(malformed(rule::SECTION, format!("@{offset}"), "a section appears twice"));
            }
            _ => {
                let data = &bytes[offset..];
                r.rest();
                container.unknown.push(UnknownSection { offset, tag: Some(tag), data });
            }
        }
    }
    Ok(container)
}

/// The smallest a texture entry can be: its length and its encoding byte.
const TEXTURE_ENTRY_MIN: usize = 5;
/// The smallest a vendor entry can be: its two lengths.
const VENDOR_ENTRY_MIN: usize = 8;

fn read_textures_inp1<'a>(
    r: &mut Reader<'a>,
    limits: &Limits,
    out: &mut Vec<RawTexture<'a>>,
) -> Result<(), ImportError> {
    let at = r.pos();
    let count = r.u32_be("the texture count")? as usize;
    if count > limits.max_textures {
        return Err(limit("max_textures", format!("TEX_SECT@{at}"), format!("{count} textures")));
    }
    if count.saturating_mul(TEXTURE_ENTRY_MIN) > r.remaining() {
        return Err(malformed(
            rule::TRUNCATED,
            format!("TEX_SECT@{at}"),
            format!("{count} textures can't fit in the {} bytes left", r.remaining()),
        ));
    }
    out.reserve(count);
    for i in 0..count {
        let len = r.u32_be("a texture length")?;
        let encoding = TextureEncoding::from_code(u32::from(r.u8("a texture encoding")?));
        if u64::from(len) > limits.max_texture_bytes {
            return Err(limit("max_texture_bytes", format!("TEX_SECT/{i}"), format!("a {len} byte texture")));
        }
        let data = r.take(len as usize, "a texture")?;
        out.push(RawTexture { encoding, data });
    }
    Ok(())
}

fn read_vendor_inp1<'a>(r: &mut Reader<'a>, out: &mut Vec<VendorData<'a>>) -> Result<(), ImportError> {
    let at = r.pos();
    let count = r.u32_be("the vendor entry count")? as usize;
    if count.saturating_mul(VENDOR_ENTRY_MIN) > r.remaining() {
        return Err(malformed(
            rule::TRUNCATED,
            format!("EXT_SECT@{at}"),
            format!("{count} vendor entries can't fit in the {} bytes left", r.remaining()),
        ));
    }
    out.reserve(count);
    for i in 0..count {
        let path = format!("EXT_SECT/{i}");
        let name_len = r.u32_be("a vendor name length")? as usize;
        let name = utf8(r.take(name_len, "a vendor name")?, &path)?;
        let data_len = r.u32_be("a vendor data length")? as usize;
        let data = r.take(data_len, "vendor data")?;
        out.push(VendorData { name, data });
    }
    Ok(())
}

fn read_inp2<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Container<'a>, ImportError> {
    let mut r = Reader::new(bytes, 8);
    let DataNode::Object(entries) = inp2::parse(&mut r, limits)? else {
        return Err(malformed(rule::INP2, "@8", "the root is not an object"));
    };
    let mut payload = None;
    let mut textures = Vec::new();
    let mut has_texture_section = false;
    let mut vendor = Vec::new();
    for (key, value) in entries {
        match key {
            INP_SECT => {
                if !matches!(value, DataNode::Object(_)) {
                    return Err(malformed(rule::INP2, INP_SECT, "INP_SECT is not an object"));
                }
                payload = Some(value);
            }
            "TEX_SECT" => {
                has_texture_section = true;
                read_textures_inp2(&value, limits, &mut textures)?;
            }
            "EXT_SECT" => {
                let DataNode::Object(items) = value else {
                    return Err(malformed(rule::INP2, "EXT_SECT", "EXT_SECT is not an object"));
                };
                for (name, data) in items {
                    let DataNode::Blob(data) = data else {
                        return Err(malformed(rule::INP2, format!("EXT_SECT/{name}"), "vendor data is not a blob"));
                    };
                    vendor.push(VendorData { name, data });
                }
            }
            _ => {}
        }
    }
    let Some(payload) = payload else {
        return Err(malformed(rule::INP2, "INP_SECT", "no INP_SECT payload"));
    };
    let mut unknown = Vec::new();
    if r.remaining() > 0 {
        let offset = r.pos();
        unknown.push(UnknownSection { offset, tag: None, data: r.rest() });
    }
    Ok(Container {
        format: Format::Inp2,
        payload: Payload::Tree(payload),
        textures,
        has_texture_section,
        vendor,
        unknown,
    })
}

fn read_textures_inp2<'a>(
    value: &DataNode<'a>,
    limits: &Limits,
    out: &mut Vec<RawTexture<'a>>,
) -> Result<(), ImportError> {
    let DataNode::Array(items) = value else {
        return Err(malformed(rule::INP2, "TEX_SECT", "TEX_SECT is not an array"));
    };
    if items.len() > limits.max_textures {
        return Err(limit("max_textures", "TEX_SECT", format!("{} textures", items.len())));
    }
    for (i, item) in items.iter().enumerate() {
        // The spec names the code `format`; Inochi2D 0.9's loader reads `encoding`. Either is accepted.
        let code = item.get("encoding").or_else(|| item.get("format")).and_then(DataNode::as_u32);
        let data = match item.get("data") {
            Some(DataNode::Blob(data)) => data,
            None | Some(DataNode::Nil) => &[][..],
            Some(_) => return Err(malformed(rule::INP2, format!("TEX_SECT/{i}/data"), "texture data is not a blob")),
        };
        if data.len() as u64 > limits.max_texture_bytes {
            return Err(limit("max_texture_bytes", format!("TEX_SECT/{i}"), format!("a {} byte texture", data.len())));
        }
        let encoding = code.map_or(TextureEncoding::Other(u32::MAX), TextureEncoding::from_code);
        out.push(RawTexture { encoding, data });
    }
    Ok(())
}
