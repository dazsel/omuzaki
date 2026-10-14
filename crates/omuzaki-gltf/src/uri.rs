//! `uri` fields: `data:` URIs this crate decodes itself, and relative paths it hands to the host's resolver.
//!
//! Nothing here fetches anything. A URI with any other scheme (`http:`, `file:`) is refused, because reading it
//! would make the importer a network client; a relative path goes to the [`Resolver`](omuzaki_io_api::Resolver)
//! percent-decoded, because that is what a file name is and the host has no business knowing glTF's URI rules.

use omuzaki_io_api::IoError;

/// What a `uri` points at.
#[derive(Debug, PartialEq, Eq)]
pub enum Target<'a> {
    /// A `data:` URI, decoded here.
    Data(Vec<u8>),
    /// A relative path (already percent-decoded) for the resolver.
    Path(String),
    /// Anything else, with the reason it can't be read.
    Refused(&'a str),
}

/// The last path segment of a relative URI, percent-decoded, for naming an image that has no `name`. `data:` URIs have
/// no file name.
pub fn file_name(uri: Option<&str>) -> String {
    let Some(uri) = uri.filter(|u| strip_scheme(u, "data").is_none()) else { return String::new() };
    let last = uri.rsplit('/').next().unwrap_or_default();
    percent_decode(last).unwrap_or_else(|_| last.to_owned())
}

/// Classifies `uri`. `max_bytes` bounds what a `data:` URI may decode to, checked before decoding allocates.
///
/// # Errors
/// [`IoError::Malformed`] for a `data:` URI that isn't valid, [`IoError::Limit`] when it decodes to more than
/// `max_bytes`.
pub fn classify(uri: &str, max_bytes: u64) -> Result<Target<'_>, IoError> {
    if let Some(rest) = strip_scheme(uri, "data") {
        return decode_data(rest, max_bytes).map(Target::Data);
    }
    if has_scheme(uri) || uri.starts_with("//") {
        return Ok(Target::Refused("only relative paths and data: URIs are read, never network or absolute URLs"));
    }
    percent_decode(uri).map(Target::Path)
}

fn has_scheme(uri: &str) -> bool {
    let Some((scheme, _)) = uri.split_once(':') else { return false };
    scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The part after `scheme:`, if the URI has that scheme (schemes are case-insensitive).
fn strip_scheme<'a>(uri: &'a str, scheme: &str) -> Option<&'a str> {
    let (head, rest) = uri.split_once(':')?;
    head.eq_ignore_ascii_case(scheme).then_some(rest)
}

fn decode_data(rest: &str, max_bytes: u64) -> Result<Vec<u8>, IoError> {
    let (header, payload) =
        rest.split_once(',').ok_or_else(|| IoError::malformed("a data: URI has no comma before its payload"))?;
    let base64 = header.rsplit(';').next().is_some_and(|p| p.eq_ignore_ascii_case("base64"));
    // Both encodings never decode to more than their text length; checking that first keeps the allocation bounded.
    if payload.len() as u64 > max_bytes.saturating_mul(2) {
        return Err(IoError::Limit { what: "data URI size in bytes", limit: max_bytes, got: payload.len() as u64 });
    }
    if base64 { base64_decode(payload) } else { percent_decode_bytes(payload) }
}

/// Decodes `%XX` escapes into text. A path that isn't valid UTF-8 after decoding is malformed.
fn percent_decode(text: &str) -> Result<String, IoError> {
    if !text.contains('%') {
        return Ok(text.to_owned());
    }
    String::from_utf8(percent_decode_bytes(text)?).map_err(|_| IoError::malformed("a URI decodes to invalid UTF-8"))
}

fn percent_decode_bytes(text: &str) -> Result<Vec<u8>, IoError> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
            let value = hex.and_then(|h| u8::from_str_radix(h, 16).ok());
            out.push(value.ok_or_else(|| IoError::malformed("a URI has a '%' not followed by two hex digits"))?);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    Ok(out)
}

/// Base64 (RFC 4648 sections 4 and 5: both alphabets). Padding is optional and whitespace is ignored, as exporters
/// that wrap lines write it; any other character is an error.
fn base64_decode(text: &str) -> Result<Vec<u8>, IoError> {
    let data: Vec<u8> = text.trim_end_matches('=').bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    if data.len() % 4 == 1 {
        return Err(IoError::malformed("base64 data has an impossible length"));
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 2);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in &data {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err(IoError::malformed("base64 data has a character outside the alphabet")),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xFF).unwrap_or(0));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_come_from_relative_uris_only() {
        assert_eq!(file_name(Some("textures/face.png")), "face.png");
        assert_eq!(file_name(Some("a%20b.png")), "a b.png");
        assert_eq!(file_name(Some("data:image/png;base64,AAAA")), "");
        assert_eq!(file_name(Some("bad%zz.png")), "bad%zz.png", "undecodable names are kept as written");
        assert_eq!(file_name(None), "");
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (text, bytes) in [
            ("", &b""[..]),
            ("Zg==", b"f"),
            ("Zm8=", b"fo"),
            ("Zm9v", b"foo"),
            ("Zm9vYg==", b"foob"),
            ("Zm9vYmE", b"fooba"),
        ] {
            assert_eq!(base64_decode(text).unwrap(), bytes, "{text}");
        }
        assert_eq!(base64_decode("+/8=").unwrap(), [0xFB, 0xFF]);
    }

    #[test]
    fn base64_accepts_the_url_alphabet_and_ignores_line_breaks() {
        assert_eq!(base64_decode("-_8=").unwrap(), base64_decode("+/8=").unwrap());
        assert_eq!(base64_decode("Zm9v\r\nYmE=").unwrap(), b"fooba");
        assert_eq!(base64_decode(" Zm9v Yg ").unwrap(), b"foob", "whitespace anywhere, padding optional");
    }

    #[test]
    fn bad_base64_is_an_error() {
        for bad in ["Zm9v!", "Zm9v.", "Z", "Zm9vY", "Zm9v\u{e9}"] {
            assert!(base64_decode(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn classify_sorts_uris_into_data_paths_and_refusals() {
        assert_eq!(classify("data:application/octet-stream;base64,Zm9v", 100).unwrap(), Target::Data(b"foo".to_vec()));
        assert_eq!(classify("data:,a%20b", 100).unwrap(), Target::Data(b"a b".to_vec()));
        assert_eq!(classify("textures/my%20skin.png", 100).unwrap(), Target::Path("textures/my skin.png".into()));
        assert!(matches!(classify("https://example.com/a.bin", 100).unwrap(), Target::Refused(_)));
        assert!(matches!(classify("file:///etc/passwd", 100).unwrap(), Target::Refused(_)));
        assert!(matches!(classify("//host/share/a.bin", 100).unwrap(), Target::Refused(_)));
        assert_eq!(
            classify("a:b.bin", 100).unwrap(),
            Target::Refused("only relative paths and data: URIs are read, never network or absolute URLs")
        );
        assert!(classify("data:nocomma", 100).is_err());
        assert!(classify("bad%zz.bin", 100).is_err());
        assert!(matches!(classify("data:;base64,AAAAAAAAAAAAAAAA", 4), Err(IoError::Limit { .. })));
    }
}
