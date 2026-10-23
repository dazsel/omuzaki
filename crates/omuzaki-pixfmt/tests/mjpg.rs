//! The allocation-free MJPG path on synthetic frames: every sampling, odd sizes, restart intervals, frames with and
//! without their own Huffman tables, against zune-jpeg and the source picture; corrupt frames never panic; and what it
//! doesn't cover still decodes through zune-jpeg.

#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

#[path = "support/jpeg_encoder.rs"]
mod jpeg_encoder;

use jpeg_encoder::{Options, Sampling, encode, picture};
use omuzaki_pixfmt::{ConvertError, MjpgDecoder, RgbOut};
use proptest::prelude::*;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

fn ours(jpeg: &[u8], w: usize, h: usize) -> (Vec<u8>, u64) {
    let mut dec = MjpgDecoder::new();
    let mut out = vec![0; w * h * 3];
    dec.decode(jpeg, &mut RgbOut { data: &mut out, width: w as u32, height: h as u32 }).unwrap();
    (out, dec.fallbacks())
}

/// zune-jpeg's decode of the same frame. zune-jpeg can't decode a frame without its Huffman tables (what most webcams
/// send), so it gets the frame encoded with them: the same coefficients, so the same picture.
fn zune(rgb: &[u8], w: usize, h: usize, o: Options) -> Vec<u8> {
    let jpeg = encode(rgb, w, h, Options { dht: true, ..o });
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    JpegDecoder::new_with_options(ZCursor::new(&jpeg), options).decode().unwrap()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2)).sum::<f64>() / a.len() as f64;
    10.0 * (255.0 * 255.0 / mse.max(1e-9)).log10()
}

fn mean_abs(a: &[u8], b: &[u8]) -> f64 {
    a.iter().zip(b).map(|(&x, &y)| f64::from(x.abs_diff(y))).sum::<f64>() / a.len() as f64
}

#[test]
fn every_sampling_and_odd_size_decodes_like_zune_jpeg() {
    let sizes = [(1, 1), (7, 5), (17, 9), (33, 17), (64, 48), (161, 91), (320, 240)];
    let samplings = [Sampling::S444, Sampling::S422, Sampling::S420];
    for (w, h) in sizes {
        let rgb = picture(w, h, (w * 31 + h) as u32);
        for sampling in samplings {
            for (restart, dht) in [(0, true), (0, false), (3, false), (1, true)] {
                let o = Options { quality: 85, sampling, restart, dht };
                let jpeg = encode(&rgb, w, h, o);
                let (got, fallbacks) = ours(&jpeg, w, h);
                assert_eq!(fallbacks, 0, "{w}x{h} {o:?} left the fast path");
                let reference = zune(&rgb, w, h, o);
                // zune-jpeg upsamples chroma its own way, so compare the averages; the source bounds the error.
                let diff = mean_abs(&got, &reference);
                assert!(diff < 1.5, "{w}x{h} {o:?}: mean abs diff from zune-jpeg {diff:.3}");
                let quality = psnr(&got, &rgb);
                assert!(quality > 25.0, "{w}x{h} {o:?}: {quality:.1} dB against the source");
            }
        }
    }
}

#[test]
fn a_720p_and_a_1080p_frame_decode_on_the_fast_path() {
    for (w, h) in [(1280, 720), (1920, 1080)] {
        let rgb = picture(w, h, 7);
        for sampling in [Sampling::S422, Sampling::S420] {
            let o = Options { sampling, restart: 8, ..Options::default() };
            let jpeg = encode(&rgb, w, h, o);
            let (got, fallbacks) = ours(&jpeg, w, h);
            assert_eq!(fallbacks, 0);
            assert!(mean_abs(&got, &zune(&rgb, w, h, o)) < 1.0);
            assert!(psnr(&got, &rgb) > 30.0, "{w}x{h} {sampling:?}: {:.1} dB", psnr(&got, &rgb));
        }
    }
}

#[test]
fn a_frame_without_huffman_tables_decodes_where_zune_jpeg_alone_could_not() {
    let (w, h) = (64, 32);
    let rgb = picture(w, h, 5);
    let jpeg = encode(&rgb, w, h, Options::default());
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    assert!(JpegDecoder::new_with_options(ZCursor::new(&jpeg), options).decode().is_err());
    let (got, fallbacks) = ours(&jpeg, w, h);
    assert_eq!(fallbacks, 0);
    assert!(psnr(&got, &rgb) > 30.0);
}

#[test]
fn a_wrong_size_is_refused_before_decoding() {
    let jpeg = encode(&picture(32, 16, 1), 32, 16, Options::default());
    let mut out = vec![0; 16 * 32 * 3];
    let r = MjpgDecoder::new().decode(&jpeg, &mut RgbOut { data: &mut out, width: 16, height: 32 });
    assert_eq!(r, Err(ConvertError::JpegSize { expected: (16, 32), got: (32, 16) }));
}

#[test]
fn a_progressive_jpeg_falls_back_to_zune_jpeg() {
    // Made by Pillow from the same picture as the other fixtures (`fixtures/make_fixtures.py`).
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let jpeg = std::fs::read(format!("{dir}/64x48_progressive.jpg")).unwrap();
    let want = std::fs::read(format!("{dir}/64x48_progressive.rgb")).unwrap();
    let (got, fallbacks) = ours(&jpeg, 64, 48);
    assert_eq!(fallbacks, 1);
    let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(worst <= 3, "max abs diff {worst} from Pillow");
}

#[test]
fn a_frame_cut_short_at_a_restart_marker_is_an_error() {
    let (w, h) = (64, 32);
    let jpeg = encode(&picture(w, h, 3), w, h, Options { restart: 2, ..Options::default() });
    // Cut just after the second restart marker: every MCU after it is missing.
    let second = jpeg.windows(2).enumerate().filter(|(_, p)| p[0] == 0xFF && (0xD0..=0xD7).contains(&p[1])).nth(1);
    let cut = second.unwrap().0 + 2;
    let mut out = vec![0; w * h * 3];
    let mut dec = MjpgDecoder::new();
    let r = dec.decode(&jpeg[..cut], &mut RgbOut { data: &mut out, width: w as u32, height: h as u32 });
    assert!(matches!(r, Err(ConvertError::Jpeg(_))), "{r:?}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Damaged frames are an error or some picture, never a panic, and the decoder still works after them.
    #[test]
    fn corrupt_frames_never_panic(
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
        cut in any::<prop::sample::Index>(),
        sampling in prop::sample::select(vec![Sampling::S444, Sampling::S422, Sampling::S420]),
        restart in 0_u16..3,
    ) {
        let (w, h) = (40, 24);
        let rgb = picture(w, h, 9);
        let good = encode(&rgb, w, h, Options { sampling, restart, ..Options::default() });
        let mut bad = good.clone();
        for (at, byte) in edits {
            let i = at.index(bad.len());
            bad[i] = byte;
        }
        bad.truncate(cut.index(bad.len()) + 1);
        let mut dec = MjpgDecoder::new();
        let mut out = vec![0; w * h * 3];
        let _ = dec.decode(&bad, &mut RgbOut { data: &mut out, width: w as u32, height: h as u32 });
        dec.decode(&good, &mut RgbOut { data: &mut out, width: w as u32, height: h as u32 }).unwrap();
        prop_assert!(psnr(&out, &rgb) > 25.0);
    }
}
