//! INP2's tagged tree (`Inochi2D` 0.9, `tech-docs/inp2.md`, `modules/inp/source/inp/format/inp2/`).
//!
//! Every value starts with a little-endian `u32` tag: the low byte is the type, the upper 24 bits its metadata (a
//! length or a count). Everything is 4-byte aligned, counted from the start of the file. Strings and blobs longer
//! than `0xFF_FFFE` bytes put `0xFF_FFFF` in the metadata and their length in the next word; a blob is followed by
//! its CRC-32. Arrays and objects carry their element count and end with a sentinel tag.
//!
//! The reference reader leaves the end sentinels unread and skips booleans' value words, so a file its own writer
//! made reads back wrong when an array or object is nested; this reader follows the writer, which always writes
//! the value word and the sentinel, and requires both.

use crate::container::Reader;
use crate::error::{ImportError, Limits, limit, malformed};
use crate::report::rule;

const TAG_NIL: u32 = 0x00;
const TAG_BOOL: u32 = 0x01;
const TAG_INT: u32 = 0x02;
const TAG_UINT: u32 = 0x03;
const TAG_FLOAT: u32 = 0x04;
const TAG_STRING: u32 = 0x05;
const TAG_BLOB: u32 = 0x06;
const TAG_ARRAY_BEGIN: u32 = 0x10;
const TAG_ARRAY_END: u32 = 0x11;
const TAG_OBJECT_BEGIN: u32 = 0x12;
const TAG_OBJECT_END: u32 = 0x13;
const TAG_KEY: u32 = 0xF0;
/// The metadata that says a string's or blob's length is in the next word.
const LONG_LENGTH: u32 = 0x00FF_FFFF;

/// One INP2 value. Strings and blobs borrow the input.
#[derive(Clone, Debug, PartialEq)]
pub enum DataNode<'a> {
    /// Nothing.
    Nil,
    /// A boolean.
    Bool(bool),
    /// A signed integer.
    Int(i32),
    /// An unsigned integer.
    UInt(u32),
    /// A float.
    Float(f32),
    /// UTF-8 text.
    Str(&'a str),
    /// Bytes (their CRC-32 checked).
    Blob(&'a [u8]),
    /// An array.
    Array(Vec<DataNode<'a>>),
    /// An object, its keys in file order.
    Object(Vec<(&'a str, DataNode<'a>)>),
}

impl<'a> DataNode<'a> {
    /// The value at `key`, if this is an object that has it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&DataNode<'a>> {
        match self {
            Self::Object(entries) => entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value as an unsigned integer, if it is a non-negative integer.
    #[must_use]
    pub fn as_u32(&self) -> Option<u32> {
        match *self {
            Self::UInt(v) => Some(v),
            Self::Int(v) => u32::try_from(v).ok(),
            _ => None,
        }
    }
}

const fn align4(pos: usize) -> usize {
    (pos + 3) & !3
}

fn skip_padding(r: &mut Reader<'_>) -> Result<(), ImportError> {
    let pad = align4(r.pos()) - r.pos();
    r.take(pad, "alignment padding").map(|_| ())
}

fn length(r: &mut Reader<'_>, tag: u32, what: &str) -> Result<usize, ImportError> {
    let meta = tag >> 8;
    let len = if meta == LONG_LENGTH { r.u32_le(what)? } else { meta };
    Ok(len as usize)
}

/// Parses one value at the reader's position (just after the magic, for a file).
pub(crate) fn parse<'a>(r: &mut Reader<'a>, limits: &Limits) -> Result<DataNode<'a>, ImportError> {
    value(r, limits, 0)
}

fn value<'a>(r: &mut Reader<'a>, limits: &Limits, depth: usize) -> Result<DataNode<'a>, ImportError> {
    let at = r.pos();
    let path = || format!("@{at}");
    let tag = r.u32_le("a tag")?;
    match tag & 0xFF {
        TAG_NIL => Ok(DataNode::Nil),
        TAG_BOOL => Ok(DataNode::Bool(r.u32_le("a boolean")? != 0)),
        TAG_INT => Ok(DataNode::Int(i32::from_le_bytes(r.array::<4>("an integer")?))),
        TAG_UINT => Ok(DataNode::UInt(r.u32_le("an integer")?)),
        TAG_FLOAT => Ok(DataNode::Float(f32::from_le_bytes(r.array::<4>("a float")?))),
        TAG_STRING => {
            let len = length(r, tag, "a string length")?;
            let bytes = r.take(len, "a string")?;
            let text = std::str::from_utf8(bytes).map_err(|e| malformed(rule::UTF8, path(), e.to_string()))?;
            skip_padding(r)?;
            Ok(DataNode::Str(text))
        }
        TAG_BLOB => {
            let len = length(r, tag, "a blob length")?;
            if len as u64 > limits.max_payload_bytes {
                return Err(limit("max_payload_bytes", path(), format!("a {len} byte blob")));
            }
            let bytes = r.take(len, "a blob")?;
            skip_padding(r)?;
            let crc = r.u32_le("a blob checksum")?;
            if crc32fast::hash(bytes) != crc {
                return Err(malformed(rule::INP2, path(), "a blob's CRC-32 doesn't match"));
            }
            Ok(DataNode::Blob(bytes))
        }
        TAG_ARRAY_BEGIN | TAG_OBJECT_BEGIN => {
            if depth >= limits.max_depth {
                return Err(limit("max_depth", path(), format!("nesting deeper than {}", limits.max_depth)));
            }
            let object = tag & 0xFF == TAG_OBJECT_BEGIN;
            let end = if object { TAG_OBJECT_END } else { TAG_ARRAY_END };
            let count = tag >> 8;
            // No capacity from the count: each element takes at least one word, so the input bounds the memory.
            let mut items = Vec::new();
            let mut entries = Vec::new();
            for _ in 0..count {
                if r.peek_u32_le().is_some_and(|t| t & 0xFF == end) {
                    break;
                }
                if object {
                    let key = key(r)?;
                    entries.push((key, value(r, limits, depth + 1)?));
                } else {
                    items.push(value(r, limits, depth + 1)?);
                }
            }
            let close = r.u32_le("an end sentinel")?;
            if close & 0xFF != end {
                return Err(malformed(rule::INP2, path(), "an array or object doesn't end where its count says"));
            }
            Ok(if object { DataNode::Object(entries) } else { DataNode::Array(items) })
        }
        other => Err(malformed(rule::INP2, path(), format!("unexpected tag 0x{other:02X}"))),
    }
}

fn key<'a>(r: &mut Reader<'a>) -> Result<&'a str, ImportError> {
    let at = r.pos();
    let tag = r.u32_le("a key tag")?;
    if tag & 0xFF != TAG_KEY {
        return Err(malformed(rule::INP2, format!("@{at}"), "an object entry without a key"));
    }
    let bytes = r.take((tag >> 8) as usize, "a key")?;
    let key = std::str::from_utf8(bytes).map_err(|e| malformed(rule::UTF8, format!("@{at}"), e.to_string()))?;
    skip_padding(r)?;
    Ok(key)
}
