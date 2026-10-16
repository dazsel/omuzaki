//! `image::load`: PNG and JPEG decoding, the store and file sources, the file fence, and the refusals of an oversized or
//! unreadable image. Every image is made here, from code (solid fills and gradients), never from a real photograph.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::fs;
use std::sync::Arc;

use ::image::codecs::jpeg::JpegEncoder;
use ::image::codecs::png::PngEncoder;
use ::image::{ExtendedColorType, ImageEncoder};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_measure::image::{ImageRef, Rgba8, load};
use omuzaki_ops_api::{Actor, ErrorCode, Grants, OpCtx, OpError, ProjectDir, Scope, Services, ServicesBuilder};
use omuzaki_store_api::{Commercial, ContentHash, Store};
use omuzaki_store_mock::MemStore;
use omuzaki_store_mock::conformance::sample_provenance;
use serde_json::json;
use tempfile::TempDir;

/// The field name the tests pass to `load`, as an op would name its argument.
const FIELD: &str = "reference.image";

/// Runs `f` in a context built the way the executor builds one, with the given services, grants and project folder.
fn run<T>(services: &Services, grants: &Grants, project: Option<&ProjectDir>, f: impl FnOnce(&OpCtx<'_>) -> T) -> T {
    let (doc, actor, clock) =
        (MemDoc::default(), Actor::Agent { id: "a".into(), model: None }, FakeClock::new(Seconds(0.0)));
    let ctx = OpCtx::new(&doc, &actor, &clock).with_services(services).with_grants(grants);
    match project {
        Some(dir) => f(&ctx.with_project_dir(dir)),
        None => f(&ctx),
    }
}

/// A store with the image put into it, and the services that carry it.
fn stored(bytes: &[u8]) -> (Arc<MemStore>, ContentHash, Services) {
    let store = Arc::new(MemStore::new());
    let hash = store.put(bytes, sample_provenance(Commercial::Ok)).unwrap();
    let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
    (store, hash, services)
}

/// The pixels of a decoded image as `[r, g, b, a]` quads, for comparison.
fn quads(image: &Rgba8) -> Vec<[u8; 4]> {
    image.pixels.as_chunks::<4>().0.to_vec()
}

fn load_ok(bytes: &[u8]) -> Rgba8 {
    let (_, hash, services) = stored(bytes);
    run(&services, &Grants::of([]), None, |ctx| load(ctx, &ImageRef::Asset { asset: hash }, FIELD)).unwrap()
}

fn load_err(services: &Services, grants: &Grants, project: Option<&ProjectDir>, image: &ImageRef) -> OpError {
    run(services, grants, project, |ctx| load(ctx, image, FIELD)).unwrap_err()
}

fn png(width: u32, height: u32, colour: ExtendedColorType, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out).write_image(data, width, height, colour).unwrap();
    out
}

/// The CRC-32 of a PNG chunk (ISO 3309), for a chunk the test writes by hand.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A PNG that stops after its header: the size and the colour, and no pixels. A decoder reads the size from here and
/// must refuse it before it allocates for the pixels.
fn png_header(width: u32, height: u32, depth: u8, colour: u8) -> Vec<u8> {
    let mut chunk = b"IHDR".to_vec();
    chunk.extend(width.to_be_bytes());
    chunk.extend(height.to_be_bytes());
    chunk.extend([depth, colour, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend(13_u32.to_be_bytes());
    out.extend(&chunk);
    out.extend(crc32(&chunk).to_be_bytes());
    // An empty IDAT: the decoder reads the header up to the first image data chunk, so the header alone is not enough
    // to read the size. The chunk holds no bytes, so nothing is decoded.
    out.extend(0_u32.to_be_bytes());
    out.extend(b"IDAT");
    out.extend(crc32(b"IDAT").to_be_bytes());
    out
}

/// A folder laid out like a project next to a folder the project does not own, with one image in each.
struct Site {
    _dir: TempDir,
    project: ProjectDir,
    png: Vec<u8>,
}

impl Site {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let (project, outside) = (root.join("project"), root.join("outside"));
        fs::create_dir_all(project.join("refs")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let png = png(2, 2, ExtendedColorType::Rgb8, &[9, 8, 7, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        fs::write(project.join("refs/front.png"), &png).unwrap();
        fs::write(outside.join("secret.png"), &png).unwrap();
        let project = ProjectDir::open(&project).unwrap();
        Self { _dir: dir, project, png }
    }
}

fn read_scope() -> Grants {
    Grants::of([Scope::Read])
}

#[test]
fn grey_rgb_and_rgba_pngs_decode_to_rgba() {
    let grey = load_ok(&png(2, 1, ExtendedColorType::L8, &[10, 200]));
    assert_eq!((grey.width, grey.height), (2, 1));
    assert_eq!(quads(&grey), vec![[10, 10, 10, 255], [200, 200, 200, 255]]);

    let rgb = load_ok(&png(1, 2, ExtendedColorType::Rgb8, &[1, 2, 3, 250, 251, 252]));
    assert_eq!(quads(&rgb), vec![[1, 2, 3, 255], [250, 251, 252, 255]]);

    let rgba = load_ok(&png(1, 1, ExtendedColorType::Rgba8, &[5, 6, 7, 128]));
    assert_eq!(quads(&rgba), vec![[5, 6, 7, 128]]);
}

#[test]
fn a_transparent_pixel_keeps_its_colour_and_alpha_straight() {
    // Straight, not premultiplied: the colour under a zero alpha is the one the file gives, not black.
    let image = load_ok(&png(2, 1, ExtendedColorType::Rgba8, &[10, 20, 30, 0, 40, 50, 60, 255]));
    assert_eq!(quads(&image), vec![[10, 20, 30, 0], [40, 50, 60, 255]]);
}

#[test]
fn sixteen_bit_pngs_decode_to_their_eight_bit_values() {
    // Each channel is a multiple of 257 (0x0101), so the eight-bit value is exact whichever way the rounding goes.
    let values: [u16; 6] = [0, 257, 128 * 257, 255 * 257, 3 * 257, 77 * 257];
    let data: Vec<u8> = values.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let image = load_ok(&png(1, 2, ExtendedColorType::Rgb16, &data));
    assert_eq!(quads(&image), vec![[0, 1, 128, 255], [255, 3, 77, 255]]);
}

#[test]
fn pixels_run_row_by_row_from_the_top() {
    // A 7 by 5 gradient where every pixel is distinct, so any transposition or row shift shows.
    let mut data = Vec::new();
    for y in 0..5_u8 {
        for x in 0..7_u8 {
            data.extend([x * 30, y * 40, 50]);
        }
    }
    let image = load_ok(&png(7, 5, ExtendedColorType::Rgb8, &data));
    assert_eq!((image.width, image.height), (7, 5));
    for y in 0..5_u8 {
        for x in 0..7_u8 {
            let at = usize::from(y) * 7 + usize::from(x);
            assert_eq!(quads(&image)[at], [x * 30, y * 40, 50, 255], "pixel ({x}, {y})");
        }
    }
}

#[test]
fn a_flat_jpeg_decodes_to_its_colour() {
    // A solid fill is the one case where a JPEG is predictable: the lossy error is a count or two, not a pattern.
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 100)
        .write_image(&[200, 120, 60].repeat(16 * 16), 16, 16, ExtendedColorType::Rgb8)
        .unwrap();
    let image = load_ok(&jpeg);
    assert_eq!((image.width, image.height), (16, 16));
    for (at, pixel) in quads(&image).iter().enumerate() {
        for (channel, (got, want)) in pixel.iter().zip([200_i32, 120, 60, 255]).enumerate() {
            let diff = (i32::from(*got) - want).abs();
            assert!(diff <= 3, "pixel {at} channel {channel}: {got} vs {want}");
        }
    }
}

#[test]
fn an_asset_loads_to_the_bytes_the_store_holds() {
    let bytes = png(1, 1, ExtendedColorType::Rgb8, &[11, 22, 33]);
    let (_, hash, services) = stored(&bytes);
    let image = run(&services, &Grants::of([]), None, |ctx| load(ctx, &ImageRef::Asset { asset: hash }, FIELD));
    let image = image.unwrap();
    assert_eq!(image.hash, hash);
    assert_eq!(quads(&image), vec![[11, 22, 33, 255]]);
}

#[test]
fn a_file_loads_and_is_named_by_the_hash_of_its_bytes() {
    let site = Site::new();
    let services = Services::none();
    let image = run(&services, &read_scope(), Some(&site.project), |ctx| {
        load(ctx, &ImageRef::File { path: "refs/front.png".into() }, FIELD)
    })
    .unwrap();
    assert_eq!(image.hash, ContentHash::of(&site.png));
    assert_eq!(quads(&image), vec![[9, 8, 7, 255], [1, 2, 3, 255], [4, 5, 6, 255], [7, 8, 9, 255]]);
}

#[test]
fn a_missing_asset_is_not_found_and_names_the_argument() {
    let store = Arc::new(MemStore::new());
    let services = ServicesBuilder::new().insert::<dyn Store>(store).build();
    let missing = ContentHash::of(b"never stored");
    let error = load_err(&services, &Grants::of([]), None, &ImageRef::Asset { asset: missing });
    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(error.field.as_deref(), Some(FIELD));
}

#[test]
fn a_missing_file_is_not_found() {
    let site = Site::new();
    let error = load_err(
        &Services::none(),
        &read_scope(),
        Some(&site.project),
        &ImageRef::File { path: "refs/absent.png".into() },
    );
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[test]
fn an_asset_without_a_store_is_unavailable() {
    let error =
        load_err(&Services::none(), &Grants::of([]), None, &ImageRef::Asset { asset: ContentHash::of(b"anything") });
    assert_eq!(error.code, ErrorCode::Unavailable);
}

#[test]
fn an_asset_the_store_cannot_verify_is_internal() {
    let (store, hash, services) = stored(&png(1, 1, ExtendedColorType::Rgb8, &[1, 2, 3]));
    assert!(store.flip_byte(&hash), "the mock corrupts the stored bytes");
    let error = load_err(&services, &Grants::of([]), None, &ImageRef::Asset { asset: hash });
    assert_eq!(error.code, ErrorCode::Internal);
}

#[test]
fn a_path_outside_the_project_is_denied() {
    let site = Site::new();
    let error = load_err(
        &Services::none(),
        &read_scope(),
        Some(&site.project),
        &ImageRef::File { path: "../outside/secret.png".into() },
    );
    assert_eq!(error.code, ErrorCode::Denied);
    assert_eq!(error.field.as_deref(), Some(FIELD));
}

#[test]
fn a_file_without_the_read_scope_is_denied() {
    let site = Site::new();
    let error = load_err(
        &Services::none(),
        &Grants::of([Scope::Render]),
        Some(&site.project),
        &ImageRef::File { path: "refs/front.png".into() },
    );
    assert_eq!(error.code, ErrorCode::Denied);
}

#[test]
fn a_header_larger_than_the_side_limit_is_refused_before_any_pixels() {
    // 10000 px is over the 8192 cap. Only the header exists, so nothing can be decoded: the size alone decides.
    let (_, hash, services) = stored(&png_header(10_000, 10_000, 8, 2));
    let error = load_err(&services, &Grants::of([]), None, &ImageRef::Asset { asset: hash });
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(error.field.as_deref(), Some(FIELD));
    assert!(error.message.contains("10,000 × 10,000"), "{}", error.message);
}

#[test]
fn an_image_over_the_decoded_size_limit_is_refused_from_its_header() {
    // 5000 by 5000 is under the 8192 side limit, but as RGBA8 it is 100 MB, over the 64 MiB the measure keeps.
    let (_, hash, services) = stored(&png_header(5_000, 5_000, 8, 6));
    let error = load_err(&services, &Grants::of([]), None, &ImageRef::Asset { asset: hash });
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.message.contains("5,000 × 5,000"), "{}", error.message);
}

#[test]
fn bytes_that_are_not_an_image_are_unreadable() {
    let (_, hash, services) = stored(b"not an image at all");
    let error = load_err(&services, &Grants::of([]), None, &ImageRef::Asset { asset: hash });
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.message.starts_with("The image could not be read"), "{}", error.message);
}

#[test]
fn an_image_ref_reads_the_shapes_the_contract_names() {
    let hash = ContentHash::of(b"x");
    let asset: ImageRef =
        serde_json::from_value(serde_json::to_value(ImageRef::Asset { asset: hash }).unwrap()).unwrap();
    assert_eq!(asset, ImageRef::Asset { asset: hash });
    let file: ImageRef = serde_json::from_value(json!({"path": "refs/front.png"})).unwrap();
    assert_eq!(file, ImageRef::File { path: "refs/front.png".into() });
}
