//! The container: hand-assembled INP1 and INP2 files, sections kept, textures decoded, truncations refused.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use common::{Inp2, ext_sect, inp1, inp2_file, minimal, minimal_file, pixels, png, tga};
use omuzaki_inochi2d::{
    Format, ImportError, Limits, Payload, TextureEncoding, detect, import, read_container, rule, write_inp1,
};

#[test]
fn a_minimal_inp1_imports() {
    let bytes = minimal_file();
    assert_eq!(detect(&bytes).unwrap(), Format::Inp1);
    let imported = import(&bytes, &Limits::default()).unwrap();
    let def = &imported.def;
    assert_eq!(def.nodes.len(), 2);
    assert_eq!(def.nodes[0].name, "Root");
    assert_eq!(def.nodes[1].parent.map(omuzaki_puppet_api::model::NodeIx::get), Some(0));
    assert_eq!(def.textures.len(), 1);
    assert_eq!(def.textures[0].file, "textures/0.png");
    assert_eq!(def.textures[0].size, [4, 4]);
    assert_eq!(imported.textures[0].rgba, pixels(4, 4), "pixels are kept as stored (already premultiplied)");
    assert_eq!(def.meta.source, "inochi2d");
    assert_eq!(imported.report.mapped.parts, 1);
    assert_eq!(imported.report.mapped.bindings, 1);
    assert!(imported.report.has(rule::CANVAS));
    assert_eq!(def.canvas, [10.0, 10.0]);
    assert!(def.check().is_ok());
}

#[test]
fn a_tga_texture_decodes() {
    let rgba = pixels(3, 5);
    let bytes = inp1(&minimal(), Some(&[(1, tga(3, 5, &rgba))]));
    let imported = import(&bytes, &Limits::default()).unwrap();
    assert_eq!(imported.textures[0].size, [3, 5]);
    assert_eq!(imported.textures[0].rgba, rgba);
}

#[test]
fn bc7_unknown_and_empty_slots_get_placeholders_and_findings() {
    let bytes = inp1(&minimal(), Some(&[(0, png(2, 2, &pixels(2, 2))), (2, vec![1, 2, 3]), (9, vec![4]), (0, vec![])]));
    let imported = import(&bytes, &Limits::default()).unwrap();
    assert_eq!(imported.textures.len(), 4);
    for i in 1..4 {
        assert_eq!(imported.textures[i].size, [1, 1]);
        assert_eq!(imported.textures[i].rgba, vec![0; 4]);
    }
    let r = &imported.report;
    assert_eq!(r.with_id(rule::TEXTURE_BC7).next().unwrap().path, "TEX_SECT/1");
    assert_eq!(r.with_id(rule::TEXTURE_ENCODING).next().unwrap().path, "TEX_SECT/2");
    assert_eq!(r.with_id(rule::TEXTURE_EMPTY).next().unwrap().path, "TEX_SECT/3");
}

#[test]
fn a_texture_that_does_not_decode_is_malformed() {
    let bytes = inp1(&minimal(), Some(&[(0, b"\x89PNG\r\n\x1a\nnot really".to_vec())]));
    let e = import(&bytes, &Limits::default()).unwrap_err();
    assert_eq!((e.rule(), e.path()), (rule::TEXTURE_DECODE, "TEX_SECT/0"));
}

#[test]
fn vendor_data_and_unknown_sections_are_kept() {
    let mut bytes = minimal_file();
    bytes.extend_from_slice(&ext_sect(&[("org.example.tool", b"\x00\x01\x02"), ("org.example.other", b"")]));
    let unknown_at = bytes.len();
    bytes.extend_from_slice(b"ZZZ_SECT");
    bytes.extend_from_slice(b"whatever follows");
    let c = read_container(&bytes, &Limits::default()).unwrap();
    assert_eq!(c.vendor.len(), 2);
    assert_eq!(c.vendor[0].name, "org.example.tool");
    assert_eq!(c.vendor[0].data, b"\x00\x01\x02");
    assert_eq!(c.unknown.len(), 1);
    assert_eq!(c.unknown[0].offset, unknown_at);
    assert_eq!(c.unknown[0].tag, Some(*b"ZZZ_SECT"));
    assert_eq!(c.unknown[0].data, &bytes[unknown_at..]);

    let imported = import(&bytes, &Limits::default()).unwrap();
    assert_eq!(imported.vendor.len(), 2);
    assert_eq!(imported.unknown.len(), 1);
    let found = imported.report.with_id(rule::UNKNOWN_SECTION).next().unwrap();
    assert_eq!(found.path, format!("@{unknown_at}"));
    assert_eq!(imported.report.with_id(rule::VENDOR_DATA).count(), 2);
}

#[test]
fn a_short_tail_is_kept_as_an_untagged_section() {
    let mut bytes = minimal_file();
    bytes.extend_from_slice(b"TAIL");
    let c = read_container(&bytes, &Limits::default()).unwrap();
    assert_eq!(c.unknown.len(), 1);
    assert_eq!(c.unknown[0].tag, None);
    assert_eq!(c.unknown[0].data, b"TAIL");
}

#[test]
fn a_repeated_section_is_malformed() {
    let mut bytes = minimal_file();
    bytes.extend_from_slice(b"TEX_SECT\0\0\0\0");
    let e = read_container(&bytes, &Limits::default()).unwrap_err();
    assert_eq!(e.rule(), rule::SECTION);
}

#[test]
fn no_magic_is_malformed() {
    for bytes in [&b""[..], b"TRNSRTS", b"TRNSRTS3........", b"PNG....."] {
        assert_eq!(detect(bytes).unwrap_err().rule(), rule::MAGIC);
        assert_eq!(import(bytes, &Limits::default()).unwrap_err().rule(), rule::MAGIC);
    }
}

/// Every strict prefix of a file is refused as malformed or over a limit, and none panics.
#[test]
fn every_truncation_is_refused() {
    let bytes = minimal_file();
    for n in 0..bytes.len() {
        match import(&bytes[..n], &Limits::default()) {
            Err(ImportError::Malformed { .. } | ImportError::Limit { .. }) => {}
            other => panic!("prefix {n} of {}: {other:?}", bytes.len()),
        }
    }
    let bytes = inp2_file();
    for n in 0..bytes.len() {
        match read_container(&bytes[..n], &Limits::default()) {
            Err(ImportError::Malformed { .. } | ImportError::Limit { .. }) => {}
            other => panic!("INP2 prefix {n} of {}: {other:?}", bytes.len()),
        }
    }
}

/// With vendor data after the textures, a cut inside it is either refused or kept as an unknown section: never lost
/// silently, never a panic.
#[test]
fn a_truncated_tail_is_refused_or_kept() {
    let mut bytes = minimal_file();
    let tail = bytes.len();
    bytes.extend_from_slice(&ext_sect(&[("org.example.tool", b"0123456789")]));
    for n in tail + 1..bytes.len() {
        match import(&bytes[..n], &Limits::default()) {
            Err(ImportError::Malformed { .. } | ImportError::Limit { .. }) => {}
            Ok(imported) => assert!(!imported.unknown.is_empty(), "prefix {n}"),
            Err(other) => panic!("prefix {n}: {other:?}"),
        }
    }
}

fn with_be(bytes: &[u8], at: usize, v: u32) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out[at..at + 4].copy_from_slice(&v.to_be_bytes());
    out
}

/// `u32::MAX` in any length field is refused before anything is allocated for it (the allocation itself is measured in
/// `tests/allocation.rs`).
#[test]
fn a_max_length_is_a_limit() {
    let bytes = minimal_file();
    let e = import(&with_be(&bytes, 8, u32::MAX), &Limits::default()).unwrap_err();
    assert!(matches!(e, ImportError::Limit { limit: "max_payload_bytes", .. }), "{e:?}");

    let payload_len = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let tex_count_at = 12 + payload_len + 8;
    let e = import(&with_be(&bytes, tex_count_at, u32::MAX), &Limits::default()).unwrap_err();
    assert!(matches!(e, ImportError::Limit { limit: "max_textures", .. }), "{e:?}");

    let e = import(&with_be(&bytes, tex_count_at + 4, u32::MAX), &Limits::default()).unwrap_err();
    assert!(matches!(e, ImportError::Limit { limit: "max_texture_bytes", .. }), "{e:?}");

    // With the limits lifted, the same lengths are refused against the bytes that are there.
    let open = Limits {
        max_payload_bytes: u64::MAX,
        max_textures: usize::MAX,
        max_texture_bytes: u64::MAX,
        ..Limits::default()
    };
    for at in [8, tex_count_at, tex_count_at + 4] {
        let e = import(&with_be(&bytes, at, u32::MAX), &open).unwrap_err();
        assert_eq!(e.rule(), rule::TRUNCATED, "length at {at}");
    }
}

#[test]
fn the_file_size_limit_holds() {
    let bytes = minimal_file();
    let tight = Limits { max_bytes: 16, ..Limits::default() };
    assert!(matches!(import(&bytes, &tight), Err(ImportError::Limit { limit: "max_bytes", .. })));
}

#[test]
fn texture_dimension_and_decoded_size_limits_hold() {
    let bytes = inp1(&minimal(), Some(&[(0, png(64, 2, &pixels(64, 2)))]));
    let small = Limits { max_texture_dim: 32, ..Limits::default() };
    assert!(matches!(import(&bytes, &small), Err(ImportError::Limit { limit: "max_texture_dim", .. })));
    let small = Limits { max_decoded_bytes: 64 * 2 * 4 - 1, ..Limits::default() };
    assert!(matches!(import(&bytes, &small), Err(ImportError::Limit { limit: "max_decoded_bytes", .. })));
}

#[test]
fn an_inp2_container_reads_and_its_payload_is_unsupported() {
    let bytes = inp2_file();
    assert_eq!(detect(&bytes).unwrap(), Format::Inp2);
    let c = read_container(&bytes, &Limits::default()).unwrap();
    assert_eq!(c.format, Format::Inp2);
    let Payload::Tree(tree) = &c.payload else { panic!("an INP2 payload is a tree") };
    assert_eq!(tree.get("name"), Some(&omuzaki_inochi2d::DataNode::Str("Synthetic")));
    assert_eq!(c.textures.len(), 1);
    assert_eq!(c.textures[0].encoding, TextureEncoding::Png);
    assert_eq!(c.vendor.len(), 1);
    assert_eq!(c.vendor[0].name, "org.omuzaki.test");
    assert_eq!(c.vendor[0].data, b"vendor bytes");

    let e = import(&bytes, &Limits::default()).unwrap_err();
    assert!(matches!(e, ImportError::Unsupported { rule: rule::INP2_PAYLOAD, .. }), "{e:?}");
}

#[test]
fn an_inp2_blob_with_a_bad_checksum_is_malformed() {
    let mut w = Inp2::new();
    w.object(1).key("INP_SECT").object(1).key("b").blob(b"abcd").end_object().end_object();
    let mut bytes = w.buf;
    assert!(read_container(&bytes, &Limits::default()).is_ok());
    // The checksum is the word after the blob's bytes; flip one of its bits.
    let at = bytes.windows(4).position(|win| win == b"abcd").unwrap() + 4;
    bytes[at] ^= 1;
    assert_eq!(read_container(&bytes, &Limits::default()).unwrap_err().rule(), rule::INP2);
}

#[test]
fn deep_inp2_nesting_is_a_limit() {
    let mut w = Inp2::new();
    w.object(1).key("INP_SECT");
    for _ in 0..100 {
        w.array(1);
    }
    let e = read_container(&w.buf, &Limits::default()).unwrap_err();
    assert!(matches!(e, ImportError::Limit { limit: "max_depth", .. }), "{e:?}");
}

#[test]
fn an_inp2_count_past_the_data_is_malformed_without_allocating_for_it() {
    let mut w = Inp2::new();
    w.object(0x00FF_FFFF);
    let e = read_container(&w.buf, &Limits::default()).unwrap_err();
    assert_eq!(e.rule(), rule::TRUNCATED);
}

#[test]
fn written_files_read_back_through_the_container() {
    let original = minimal_file();
    let imported = import(&original, &Limits::default()).unwrap();
    let bytes = write_inp1(&imported.def, &imported.textures).unwrap();
    let c = read_container(&bytes, &Limits::default()).unwrap();
    assert!(c.has_texture_section);
    assert_eq!(c.textures.len(), 1);
    assert_eq!(c.textures[0].encoding, TextureEncoding::Png);
    assert_eq!(c.unknown, [] as [omuzaki_inochi2d::UnknownSection<'_>; 0]);
}
