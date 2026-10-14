//! The GLB container (glTF 2.0, "GLB File Format Specification"): a 12-byte header, a JSON chunk, an optional BIN
//! chunk, little-endian, every chunk padded to 4 bytes (JSON with spaces, BIN with zeros).

use omuzaki_io_api::IoError;

const MAGIC: u32 = 0x4654_6C67; // "glTF"
const JSON: u32 = 0x4E4F_534A; // "JSON"
const BIN: u32 = 0x004E_4942; // "BIN\0"

/// True if `bytes` start with the GLB magic.
pub(crate) fn is_glb(bytes: &[u8]) -> bool {
    bytes.get(..4).is_some_and(|m| m == MAGIC.to_le_bytes())
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, IoError> {
    let raw = bytes.get(at..at + 4).ok_or_else(|| truncated(at))?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn truncated(at: usize) -> IoError {
    IoError::Malformed {
        message: "the file ends in the middle of a GLB header or chunk".into(),
        at: Some(format!("byte {at}")),
    }
}

/// Splits a GLB into its JSON chunk and BIN chunk (empty if absent). Chunk lengths are checked against the file
/// before any slicing, so a lying header is an error and never a panic or a huge allocation.
pub(crate) fn split(bytes: &[u8]) -> Result<(&[u8], &[u8]), IoError> {
    if !is_glb(bytes) {
        return Err(IoError::malformed("not a GLB file (bad magic)"));
    }
    let version = u32_at(bytes, 4)?;
    if version != 2 {
        return Err(IoError::unsupported(format!("GLB container version {version}"), "only version 2 exists"));
    }
    let total = u32_at(bytes, 8)? as usize;
    if total > bytes.len() {
        return Err(IoError::Malformed {
            message: format!("header says {total} bytes, file has {}", bytes.len()),
            at: Some("byte 8".into()),
        });
    }
    let (mut json, mut bin) = (None, None);
    let mut at = 12;
    while at + 8 <= total {
        let len = u32_at(bytes, at)? as usize;
        let kind = u32_at(bytes, at + 4)?;
        let end = (at + 8).checked_add(len).filter(|&end| end <= total).ok_or_else(|| truncated(at))?;
        let data = bytes.get(at + 8..end).ok_or_else(|| truncated(at))?;
        match kind {
            JSON if json.is_none() => json = Some(data),
            BIN if bin.is_none() && json.is_some() => bin = Some(data),
            _ => {} // The spec says to ignore unknown chunk types.
        }
        at = end + (4 - len % 4) % 4;
    }
    Ok((json.ok_or_else(|| IoError::malformed("the GLB has no JSON chunk"))?, bin.unwrap_or(&[])))
}

/// Builds a GLB from JSON text and a binary buffer, padding both chunks.
pub(crate) fn join(json: &[u8], bin: &[u8]) -> Vec<u8> {
    let mut json = json.to_vec();
    json.resize(json.len().next_multiple_of(4), b' ');
    let mut bin = bin.to_vec();
    bin.resize(bin.len().next_multiple_of(4), 0);
    let chunk = |len: usize| u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes();
    let total = 12 + 8 + json.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&chunk(total));
    out.extend_from_slice(&chunk(json.len()));
    out.extend_from_slice(&JSON.to_le_bytes());
    out.extend_from_slice(&json);
    if !bin.is_empty() {
        out.extend_from_slice(&chunk(bin.len()));
        out.extend_from_slice(&BIN.to_le_bytes());
        out.extend_from_slice(&bin);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_then_split_round_trips_with_padding() {
        let glb = join(b"{}", &[1, 2, 3]);
        assert_eq!(glb.len() % 4, 0);
        let (json, bin) = split(&glb).unwrap();
        assert_eq!(json, b"{}  ", "JSON is padded with spaces");
        assert_eq!(bin, &[1, 2, 3, 0], "BIN is padded with zeros");
    }

    #[test]
    fn a_missing_bin_chunk_is_fine_and_lies_are_errors() {
        let glb = join(b"{}", &[]);
        assert_eq!(split(&glb).unwrap().1, &[] as &[u8]);
        assert!(split(b"nope").is_err());
        let mut lying = join(b"{}", &[0; 8]);
        lying[12] = 0xFF; // JSON chunk length now runs past the file
        assert!(matches!(split(&lying), Err(IoError::Malformed { .. })));
        let mut short_total = join(b"{}", &[]);
        short_total[8] = 0xFF;
        assert!(split(&short_total).is_err(), "total length past the end of the file");
        let mut v3 = join(b"{}", &[]);
        v3[4] = 3;
        assert!(matches!(split(&v3), Err(IoError::Unsupported { .. })));
    }
}
