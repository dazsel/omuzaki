//! Conversion accuracy against an f64 reference, layout handling, error cases, a never-panics property test and MJPG
//! against Pillow's decode.

// Tests index small known sizes, build byte patterns by truncating casts and name Y/U/V/W/H as the maths does.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::many_single_char_names, clippy::too_many_lines)]

use omuzaki_pixfmt::{
    ConvertError, MjpgDecoder, Plane, RgbOut, Yuv, YuvMatrix, YuvRange, bgra_to_rgb, nv12_to_rgb, rgb_to_rgb,
    yuy2_to_rgb,
};
use proptest::prelude::*;
use proptest::test_runner::RngSeed;

/// The textbook formulas with the published decimal constants (BT.601-7, BT.709-6), in f64.
fn reference(yuv: Yuv, y: u8, u: u8, v: u8) -> [f64; 3] {
    let (yf, uf, vf) = match yuv.range {
        YuvRange::Limited => (
            (f64::from(y) - 16.0) * 255.0 / 219.0,
            (f64::from(u) - 128.0) * 255.0 / 224.0,
            (f64::from(v) - 128.0) * 255.0 / 224.0,
        ),
        YuvRange::Full => (f64::from(y), f64::from(u) - 128.0, f64::from(v) - 128.0),
    };
    let (rv, gu, gv, bu) = match yuv.matrix {
        YuvMatrix::Bt601 => (1.402, 0.344_136, 0.714_136, 1.772),
        YuvMatrix::Bt709 => (1.5748, 0.187_324, 0.468_124, 1.8556),
    };
    [yf + rv * vf, yf - gu * uf - gv * vf, yf + bu * uf].map(|c| c.clamp(0.0, 255.0))
}

const ALL: [Yuv; 4] = [
    Yuv { matrix: YuvMatrix::Bt601, range: YuvRange::Limited },
    Yuv { matrix: YuvMatrix::Bt601, range: YuvRange::Full },
    Yuv { matrix: YuvMatrix::Bt709, range: YuvRange::Limited },
    Yuv { matrix: YuvMatrix::Bt709, range: YuvRange::Full },
];

fn tight(data: &[u8], width: usize) -> Plane<'_> {
    Plane { data, stride: width }
}

fn convert_nv12(yuv: Yuv, y: &[u8], uv: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut buf = vec![0; (w * h * 3) as usize];
    let mut out = RgbOut { data: &mut buf, width: w, height: h };
    nv12_to_rgb(tight(y, w as usize), tight(uv, w as usize), yuv, &mut out).unwrap();
    buf
}

#[test]
fn every_matrix_and_range_is_within_one_level_of_f64() {
    let steps: Vec<u8> = (0..=255).step_by(5).collect();
    for yuv in ALL {
        let mut worst = 0.0f64;
        for &y in &steps {
            for &u in &steps {
                for &v in &steps {
                    // One 2x2 NV12 block, so luma and both chroma values go through the real path.
                    let rgb = convert_nv12(yuv, &[y; 4], &[u, v], 2, 2);
                    let want = reference(yuv, y, u, v);
                    for px in rgb.as_chunks::<3>().0 {
                        for (got, want) in px.iter().zip(want) {
                            worst = worst.max((f64::from(*got) - want).abs());
                        }
                    }
                }
            }
        }
        assert!(worst <= 1.0, "{yuv:?}: worst error {worst}");
    }
}

/// Widths of 32 and up go through the 32-pixel block kernel (34 adds a pair tail); the small-size tests above only reach
/// the tail path. Varying luma and chroma, every matrix and range, NV12 and YUY2, within 1 level of the f64 reference.
#[test]
fn block_kernel_widths_match_the_f64_reference() {
    for (w, h) in [(32usize, 2usize), (34, 2), (64, 4), (66, 2)] {
        let y = pattern(w * h);
        let uv = pattern(w * h / 2);
        // YUY2 carrying exactly the NV12 samples (a pair's chroma repeated on both rows).
        let mut yuy2 = Vec::new();
        for (row, y_row) in y.chunks(w).enumerate() {
            let uv_row = &uv[(row / 2) * w..][..w];
            for i in 0..w / 2 {
                yuy2.extend_from_slice(&[y_row[2 * i], uv_row[2 * i], y_row[2 * i + 1], uv_row[2 * i + 1]]);
            }
        }
        for yuv in ALL {
            let nv12 = convert_nv12(yuv, &y, &uv, w as u32, h as u32);
            let mut got = vec![0; w * h * 3];
            let mut out = RgbOut { data: &mut got, width: w as u32, height: h as u32 };
            yuy2_to_rgb(tight(&yuy2, w * 2), yuv, &mut out).unwrap();
            assert_eq!(got, nv12, "{yuv:?} {w}x{h}: yuy2 and nv12 disagree");
            for (i, px) in nv12.as_chunks::<3>().0.iter().enumerate() {
                let (row, col) = (i / w, i % w);
                let uv_at = (row / 2) * w + (col / 2) * 2;
                let want = reference(yuv, y[i], uv[uv_at], uv[uv_at + 1]);
                for (g, w_) in px.iter().zip(want) {
                    assert!((f64::from(*g) - w_).abs() <= 1.0, "{yuv:?} {w}x{h} pixel {i}: {px:?} vs {want:?}");
                }
            }
        }
    }
}

#[test]
fn limited_bt601_anchors() {
    let yuv = Yuv::default();
    assert_eq!(convert_nv12(yuv, &[16; 4], &[128, 128], 2, 2)[..3], [0, 0, 0]);
    assert_eq!(convert_nv12(yuv, &[235; 4], &[128, 128], 2, 2)[..3], [255, 255, 255]);
    let red = convert_nv12(yuv, &[81; 4], &[90, 240], 2, 2);
    for (got, want) in red[..3].iter().zip([255i32, 0, 0]) {
        assert!((i32::from(*got) - want).abs() <= 2, "red came out as {:?}", &red[..3]);
    }
}

#[test]
fn yuy2_matches_nv12_for_the_same_samples() {
    // 4x2: per pair a distinct U/V; NV12 shares a pair's chroma across both rows, so give both YUY2 rows the same U/V.
    let y = [10u8, 60, 110, 160, 200, 30, 90, 250];
    let uv = [100u8, 150, 40, 220];
    let mut yuy2 = Vec::new();
    for row in y.chunks(4) {
        yuy2.extend_from_slice(&[row[0], uv[0], row[1], uv[1], row[2], uv[2], row[3], uv[3]]);
    }
    let want = convert_nv12(Yuv::default(), &y, &uv, 4, 2);
    let mut got = vec![0; 4 * 2 * 3];
    yuy2_to_rgb(tight(&yuy2, 8), Yuv::default(), &mut RgbOut { data: &mut got, width: 4, height: 2 }).unwrap();
    assert_eq!(got, want);
}

/// A deterministic byte pattern.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i.wrapping_mul(37) ^ (i >> 3)) as u8).collect()
}

/// Copies `rows` of `row_bytes` into a plane with `pad` junk bytes after each row.
fn padded(tight: &[u8], row_bytes: usize, pad: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for row in tight.chunks(row_bytes) {
        out.extend_from_slice(row);
        out.extend(std::iter::repeat_n(0xEE, pad));
    }
    out
}

#[test]
fn a_padded_stride_gives_the_same_image_as_a_tight_one() {
    let (w, h) = (6usize, 4usize);
    let y = pattern(w * h);
    let uv = pattern(w * h / 2);
    let yuy2 = pattern(w * 2 * h);
    let bgra = pattern(w * 4 * h);
    let rgb = pattern(w * 3 * h);
    let mut a = vec![0; w * h * 3];
    let mut b = vec![0; w * h * 3];
    let (wu, hu) = (w as u32, h as u32);
    macro_rules! out {
        ($v:expr) => {
            RgbOut { data: &mut $v, width: wu, height: hu }
        };
    }
    let yuv = Yuv::default();

    nv12_to_rgb(tight(&y, w), tight(&uv, w), yuv, &mut out!(a)).unwrap();
    let (yp, uvp) = (padded(&y, w, 5), padded(&uv, w, 3));
    nv12_to_rgb(Plane { data: &yp, stride: w + 5 }, Plane { data: &uvp, stride: w + 3 }, yuv, &mut out!(b)).unwrap();
    assert_eq!(a, b, "nv12");

    yuy2_to_rgb(tight(&yuy2, w * 2), yuv, &mut out!(a)).unwrap();
    let p = padded(&yuy2, w * 2, 6);
    yuy2_to_rgb(Plane { data: &p, stride: w * 2 + 6 }, yuv, &mut out!(b)).unwrap();
    assert_eq!(a, b, "yuy2");

    bgra_to_rgb(tight(&bgra, w * 4), false, &mut out!(a)).unwrap();
    let p = padded(&bgra, w * 4, 4);
    bgra_to_rgb(Plane { data: &p, stride: w * 4 + 4 }, false, &mut out!(b)).unwrap();
    assert_eq!(a, b, "bgra");

    rgb_to_rgb(tight(&rgb, w * 3), &mut out!(a)).unwrap();
    assert_eq!(a, rgb, "rgb tight is a copy");
    let p = padded(&rgb, w * 3, 7);
    rgb_to_rgb(Plane { data: &p, stride: w * 3 + 7 }, &mut out!(b)).unwrap();
    assert_eq!(a, b, "rgb");
}

#[test]
fn bgra_swaps_channels_drops_alpha_and_bottom_up_flips_rows() {
    // Two rows of one pixel: (B,G,R,A).
    let src = [1u8, 2, 3, 255, 4, 5, 6, 255];
    let mut out = [0u8; 6];
    bgra_to_rgb(tight(&src, 4), false, &mut RgbOut { data: &mut out, width: 1, height: 2 }).unwrap();
    assert_eq!(out, [3, 2, 1, 6, 5, 4]);
    bgra_to_rgb(tight(&src, 4), true, &mut RgbOut { data: &mut out, width: 1, height: 2 }).unwrap();
    assert_eq!(out, [6, 5, 4, 3, 2, 1]);
}

#[test]
fn every_error_variant_has_a_case() {
    let yuv = Yuv::default();
    let mut buf = vec![0u8; 64];
    let y = [0u8; 16];
    let uv = [0u8; 8];

    // BadSize: odd width, odd height (NV12 only), zero.
    let mut o = RgbOut { data: &mut buf, width: 3, height: 2 };
    assert_eq!(
        nv12_to_rgb(tight(&y, 4), tight(&uv, 4), yuv, &mut o),
        Err(ConvertError::BadSize { width: 3, height: 2 })
    );
    assert_eq!(yuy2_to_rgb(tight(&y, 4), yuv, &mut o), Err(ConvertError::BadSize { width: 3, height: 2 }));
    let mut o = RgbOut { data: &mut buf, width: 4, height: 3 };
    assert_eq!(
        nv12_to_rgb(tight(&y, 4), tight(&uv, 4), yuv, &mut o),
        Err(ConvertError::BadSize { width: 4, height: 3 })
    );
    assert!(yuy2_to_rgb(tight(&[0; 24], 8), yuv, &mut o).is_ok(), "yuy2 allows an odd height");
    let mut o = RgbOut { data: &mut buf, width: 0, height: 4 };
    assert_eq!(bgra_to_rgb(tight(&y, 4), false, &mut o), Err(ConvertError::BadSize { width: 0, height: 4 }));

    // ShortPlane: data too short, and a stride narrower than a row.
    let mut o = RgbOut { data: &mut buf, width: 4, height: 4 };
    assert_eq!(
        nv12_to_rgb(tight(&y[..15], 4), tight(&uv, 4), yuv, &mut o),
        Err(ConvertError::ShortPlane { plane: "y", need: 16, got: 15 })
    );
    assert_eq!(
        nv12_to_rgb(tight(&y, 4), tight(&uv[..7], 4), yuv, &mut o),
        Err(ConvertError::ShortPlane { plane: "uv", need: 8, got: 7 })
    );
    assert_eq!(
        rgb_to_rgb(Plane { data: &[0; 48], stride: 11 }, &mut o),
        Err(ConvertError::ShortPlane { plane: "src", need: 12, got: 11 })
    );

    // ShortOutput.
    let mut small = [0u8; 47];
    let mut o = RgbOut { data: &mut small, width: 4, height: 4 };
    assert_eq!(
        nv12_to_rgb(tight(&y, 4), tight(&uv, 4), yuv, &mut o),
        Err(ConvertError::ShortOutput { need: 48, got: 47 })
    );

    // Jpeg and JpegSize.
    let mut o = RgbOut { data: &mut buf, width: 4, height: 4 };
    assert!(matches!(MjpgDecoder::new().decode(&[1, 2, 3], &mut o), Err(ConvertError::Jpeg(_))));
    let jpeg = fixture("64x48_444.jpg");
    assert_eq!(
        MjpgDecoder::new().decode(&jpeg, &mut o),
        Err(ConvertError::JpegSize { expected: (4, 4), got: (64, 48) })
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, rng_seed: RngSeed::Fixed(0x5049_5846), ..ProptestConfig::default() })]

    /// Any size, stride and bytes: no panic, and `Ok` exactly when the sizes are valid.
    #[test]
    fn never_panics_and_is_ok_exactly_for_valid_sizes(
        w in 0u32..=64, h in 0u32..=64,
        y_stride in 0usize..=200, uv_stride in 0usize..=200,
        y_len in 0usize..=4200, uv_len in 0usize..=2200, out_len in 0usize..=12500,
        seed in any::<u8>(),
    ) {
        let fill = |n: usize| (0..n).map(|i| (i as u8).wrapping_mul(31) ^ seed).collect::<Vec<u8>>();
        let (y, uv) = (fill(y_len), fill(uv_len));
        let mut out_buf = vec![0u8; out_len];
        let (wu, hu) = (w as usize, h as usize);
        let fits = |len: usize, stride: usize, row: usize, rows: usize| {
            rows > 0 && stride >= row && len >= stride * (rows - 1) + row
        };
        let yuv = Yuv::default();
        let out_ok = out_len >= wu * hu * 3;

        let r = nv12_to_rgb(Plane { data: &y, stride: y_stride }, Plane { data: &uv, stride: uv_stride }, yuv,
            &mut RgbOut { data: &mut out_buf, width: w, height: h });
        let nv12_ok = w > 0 && h > 0 && w % 2 == 0 && h % 2 == 0 && out_ok
            && fits(y_len, y_stride, wu, hu) && fits(uv_len, uv_stride, wu, hu / 2);
        prop_assert_eq!(r.is_ok(), nv12_ok);

        let r = yuy2_to_rgb(Plane { data: &y, stride: y_stride }, yuv, &mut RgbOut { data: &mut out_buf, width: w, height: h });
        prop_assert_eq!(r.is_ok(), w > 0 && h > 0 && w % 2 == 0 && out_ok && fits(y_len, y_stride, wu * 2, hu));

        for bottom_up in [false, true] {
            let r = bgra_to_rgb(Plane { data: &y, stride: y_stride }, bottom_up, &mut RgbOut { data: &mut out_buf, width: w, height: h });
            prop_assert_eq!(r.is_ok(), w > 0 && h > 0 && out_ok && fits(y_len, y_stride, wu * 4, hu));
        }

        let r = rgb_to_rgb(Plane { data: &y, stride: y_stride }, &mut RgbOut { data: &mut out_buf, width: w, height: h });
        prop_assert_eq!(r.is_ok(), w > 0 && h > 0 && out_ok && fits(y_len, y_stride, wu * 3, hu));
    }
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn mjpg_fixtures_match_pillow_within_three_levels() {
    for name in ["444", "422", "420"] {
        let jpeg = fixture(&format!("64x48_{name}.jpg"));
        let want = fixture(&format!("64x48_{name}.rgb"));
        let mut got = vec![0u8; 64 * 48 * 3];
        MjpgDecoder::new().decode(&jpeg, &mut RgbOut { data: &mut got, width: 64, height: 48 }).unwrap();
        let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(worst <= 3, "{name}: max abs diff {worst}");
    }
}

#[test]
fn a_truncated_jpeg_is_a_jpeg_error() {
    let jpeg = fixture("64x48_420.jpg");
    let mut got = vec![0u8; 64 * 48 * 3];
    // `default()` must behave like `new()`: both reject damaged frames instead of returning partial pixels.
    for mut dec in [MjpgDecoder::new(), MjpgDecoder::default()] {
        for keep in [jpeg.len() / 2, jpeg.len() - 20] {
            let r = dec.decode(&jpeg[..keep], &mut RgbOut { data: &mut got, width: 64, height: 48 });
            assert!(matches!(r, Err(ConvertError::Jpeg(_))), "kept {keep} of {}: {r:?}", jpeg.len());
        }
    }
}
