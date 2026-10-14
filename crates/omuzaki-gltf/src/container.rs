//! The GLB container (glTF 2.0, "Binary glTF Layout"): a 12-byte header, a JSON chunk, an optional BIN chunk,
//! little-endian, every chunk padded to 4 bytes (JSON with spaces, BIN with zeros).

use omuzaki_io_api::IoError;

const MAGIC: u32 = 0x4654_6C67; // "glTF"
const VERSION: u32 = 2;
const JSON: u32 = 0x4E4F_534A; // "JSON"
const BIN: u32 = 0x004E_4942; // "BIN\0"
const HEADER_LEN: usize = 12;
const CHUNK_HEADER_LEN: usize = 8;

/// The two parts of a GLB.
#[derive(Debug)]
pub struct Chunks<'a> {
    /// The JSON text, padding included.
    pub json: &'a [u8],
    /// The binary buffer, if the file has one. Buffer 0 of the document.
    pub bin: Option<&'a [u8]>,
}

/// True if `bytes` start with the GLB magic.
pub fn is_glb(bytes: &[u8]) -> bool {
    bytes.get(..4).is_some_and(|m| m == MAGIC.to_le_bytes())
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, IoError> {
    let raw = bytes.get(at..at + 4).and_then(|s| <[u8; 4]>::try_from(s).ok()).ok_or_else(|| truncated(at))?;
    Ok(u32::from_le_bytes(raw))
}

fn truncated(at: usize) -> IoError {
    IoError::Malformed {
        message: "the file ends in the middle of a GLB header or chunk".into(),
        at: Some(format!("byte {at}")),
    }
}

/// Splits a GLB into its JSON chunk and BIN chunk. Chunk lengths are checked against the file before any slicing, so a
/// lying header is an error and never a panic or a large allocation. Unknown chunk types are skipped, as the spec says.
/// Bytes after the declared total length are ignored.
///
/// # Errors
/// [`IoError::Malformed`] for a bad magic, a header or chunk running past the file, or no JSON chunk;
/// [`IoError::Unsupported`] for a container version other than 2.
pub fn split(bytes: &[u8]) -> Result<Chunks<'_>, IoError> {
    if !is_glb(bytes) {
        return Err(IoError::malformed("not a GLB file (bad magic)"));
    }
    let version = u32_at(bytes, 4)?;
    if version != VERSION {
        return Err(IoError::unsupported(format!("GLB container version {version}"), "only version 2 exists"));
    }
    let total = u32_at(bytes, 8)? as usize;
    if total > bytes.len() {
        return Err(IoError::Malformed {
            message: format!("the header says {total} bytes but the file has {}", bytes.len()),
            at: Some("byte 8".into()),
        });
    }
    let (mut json, mut bin) = (None, None);
    let mut at = HEADER_LEN;
    while at + CHUNK_HEADER_LEN <= total {
        let len = u32_at(bytes, at)? as usize;
        let kind = u32_at(bytes, at + 4)?;
        let start = at + CHUNK_HEADER_LEN;
        let end = start.checked_add(len).filter(|&end| end <= total).ok_or_else(|| truncated(at))?;
        let data = bytes.get(start..end).ok_or_else(|| truncated(at))?;
        match kind {
            JSON if json.is_none() => json = Some(data),
            BIN if bin.is_none() && json.is_some() => bin = Some(data),
            _ => {}
        }
        at = end.saturating_add((4 - len % 4) % 4);
    }
    let json = json.ok_or_else(|| IoError::malformed("the GLB has no JSON chunk"))?;
    Ok(Chunks { json, bin })
}

/// Builds a GLB from JSON text and a binary buffer, padding both chunks. An empty `bin` writes no BIN chunk.
///
/// # Errors
/// [`IoError::Unsupported`] when the result would not fit the container's 32-bit lengths (4 GiB).
pub fn join(json: &[u8], bin: &[u8]) -> Result<Vec<u8>, IoError> {
    let json_len = json.len().next_multiple_of(4);
    let bin_len = bin.len().next_multiple_of(4);
    let total = HEADER_LEN + CHUNK_HEADER_LEN + json_len + if bin.is_empty() { 0 } else { CHUNK_HEADER_LEN + bin_len };
    let too_big = || {
        IoError::unsupported(
            "a model over 4 GiB as one GLB",
            "the GLB container has 32-bit lengths; use the separate layout",
        )
    };
    let word = |n: usize| u32::try_from(n).map(u32::to_le_bytes).map_err(|_| too_big());
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&word(total)?);
    out.extend_from_slice(&word(json_len)?);
    out.extend_from_slice(&JSON.to_le_bytes());
    out.extend_from_slice(json);
    out.resize(HEADER_LEN + CHUNK_HEADER_LEN + json_len, b' ');
    if !bin.is_empty() {
        out.extend_from_slice(&word(bin_len)?);
        out.extend_from_slice(&BIN.to_le_bytes());
        out.extend_from_slice(bin);
        out.resize(total, 0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_then_split_round_trips_with_padding() {
        let glb = join(b"{}", &[1, 2, 3]).unwrap();
        assert_eq!(glb.len() % 4, 0);
        let parts = split(&glb).unwrap();
        assert_eq!(parts.json, b"{}  ", "JSON is padded with spaces");
        assert_eq!(parts.bin, Some(&[1, 2, 3, 0][..]), "BIN is padded with zeros");
    }

    #[test]
    fn a_missing_bin_chunk_is_fine_and_unknown_chunks_are_skipped() {
        let glb = join(b"{}", &[]).unwrap();
        assert_eq!(split(&glb).unwrap().bin, None);
        // Splice an unknown chunk (type 0x1234, 4 bytes) in front of a real BIN chunk.
        let mut with_unknown = join(b"{}", &[9, 9, 9, 9]).unwrap();
        let bin_at = HEADER_LEN + CHUNK_HEADER_LEN + 4;
        let mut unknown = 4u32.to_le_bytes().to_vec();
        unknown.extend_from_slice(&0x1234u32.to_le_bytes());
        unknown.extend_from_slice(&[7; 4]);
        with_unknown.splice(bin_at..bin_at, unknown);
        let total = u32::try_from(with_unknown.len()).unwrap();
        with_unknown[8..12].copy_from_slice(&total.to_le_bytes());
        assert_eq!(split(&with_unknown).unwrap().bin, Some(&[9, 9, 9, 9][..]));
    }

    #[test]
    fn lying_headers_are_errors_and_never_panics() {
        assert!(split(b"nope").is_err());
        assert!(split(b"glTF").is_err(), "magic only");
        let mut lying = join(b"{}", &[0; 8]).unwrap();
        lying[12] = 0xFF; // the JSON chunk length now runs past the file
        assert!(matches!(split(&lying), Err(IoError::Malformed { .. })));
        let mut short_total = join(b"{}", &[]).unwrap();
        short_total[8] = 0xFF;
        assert!(split(&short_total).is_err(), "total length past the end of the file");
        let mut v3 = join(b"{}", &[]).unwrap();
        v3[4] = 3;
        assert!(matches!(split(&v3), Err(IoError::Unsupported { .. })));
        let glb = join(b"{}", &[]).unwrap();
        for cut in 0..glb.len() {
            let _ = split(&glb[..cut]);
        }
    }

    #[test]
    fn bytes_after_the_declared_total_are_ignored() {
        let mut glb = join(b"{}", &[1, 2, 3, 4]).unwrap();
        glb.extend_from_slice(b"trailing bytes some tools append");
        let parts = split(&glb).unwrap();
        assert_eq!((parts.json, parts.bin), (&b"{}  "[..], Some(&[1, 2, 3, 4][..])));
        let mut bin_first = join(b"{}", &[1, 2, 3, 4]).unwrap();
        // Swap the chunk order by hand: a BIN chunk before the JSON chunk is not the binary buffer.
        let (json_chunk, bin_chunk) = (bin_first[12..24].to_vec(), bin_first[24..].to_vec());
        bin_first.truncate(12);
        bin_first.extend(bin_chunk);
        bin_first.extend(json_chunk);
        let swapped = split(&bin_first).unwrap();
        assert_eq!(swapped.bin, None, "the BIN chunk is the one after the JSON chunk");
    }

    #[test]
    fn a_glb_without_json_is_malformed() {
        let mut glb = join(b"{}", &[1, 2, 3, 4]).unwrap();
        glb[HEADER_LEN + 4..HEADER_LEN + 8].copy_from_slice(&0x1234u32.to_le_bytes()); // the JSON chunk becomes unknown
        assert!(matches!(split(&glb), Err(IoError::Malformed { .. })));
    }
}
