//! Underlay tracing (CB-25): pixel placement, alignment, opacity, composition under the Base layer, and the request type.
//! Synthetic sheets only: nothing here reads an asset.

use std::f64::consts::FRAC_PI_2;

use omuzaki_char_api::{CharId, FaceOverlay, face_texture_desc};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_face::underlay::{
    Geometry, Registration, Sheet, Underlay, UnderlayAlign, UnderlayLayer, UnderlaySet, UnderlayView,
};
use omuzaki_face_api::{FaceLayerId, FaceLayerSpec, FaceProjection, FaceRect};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Instance, Material, PixelRect, Projection, Quality, Renderer, RendererExt,
    Scene, Tonemap,
};
use omuzaki_render_mock::{MockRenderer, shapes};
use omuzaki_store_api::ContentHash;
use proptest::prelude::*;
use serde_json::json;

/// A 0.4 m face window. At 20 px/m the Base layer is 8 x 8 px.
fn projection() -> FaceProjection {
    FaceProjection { rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.2, y1: 0.2 }, front_fade: [-1.0, -0.5] }
}

const PX_PER_M: f32 = 20.0;

/// The 8 x 8 Base layer of the 0.4 m window at 20 px/m.
fn geometry() -> Geometry {
    Geometry::new(&projection(), Vec3::ZERO, PX_PER_M).unwrap()
}
const LAYER: u32 = 8;
const IDENTITY: UnderlayAlign = UnderlayAlign::IDENTITY;

fn face(x: u32, y: u32, size: u32) -> PixelRect {
    PixelRect { x, y, width: size, height: size }
}

fn low_byte(n: u32) -> u8 {
    u8::try_from(n % 256).unwrap_or(0)
}

/// A sheet whose every pixel has its own colour, so a misplaced pixel shows up in the comparison.
fn coded_sheet(size: u32) -> Vec<u8> {
    let mut rgba = Vec::new();
    for y in 0..size {
        for x in 0..size {
            rgba.extend_from_slice(&[low_byte(x * 16), low_byte(y * 16), low_byte(x + y), 255]);
        }
    }
    rgba
}

/// The RGBA pixel at `(x, y)` of an image `width` pixels wide.
fn texel_at(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * width + x) * 4) as usize;
    [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
}

fn layer_pixel(layer: &UnderlayLayer, x: u32, y: u32) -> [u8; 4] {
    texel_at(layer.rgba(), LAYER, x, y)
}

#[test]
fn identity_at_the_sheets_scale_copies_the_face_rect_pixel_for_pixel() {
    let pixels = coded_sheet(16);
    let sheet = Sheet::new(UVec2::splat(16), &pixels).unwrap();
    let layer = Underlay::new(&sheet, face(4, 4, LAYER), &geometry(), &IDENTITY).unwrap();
    assert_eq!(layer.texture_desc().size, UVec2::splat(LAYER));
    // The corners are the first and last pixels of the loop; both must land on the face rectangle's corners.
    for y in 0..LAYER {
        for x in 0..LAYER {
            assert_eq!(layer_pixel(&layer, x, y), texel_at(&pixels, 16, 4 + x, 4 + y), "layer pixel ({x}, {y})");
        }
    }
}

#[test]
fn a_sheet_twice_the_layer_size_gives_each_block_its_own_colour() {
    // Every 2 x 2 block of the sheet is one colour. A layer pixel covers exactly one block, so it must take that colour
    // with no bleed from the neighbouring blocks.
    let mut pixels = Vec::new();
    for y in 0..16_u32 {
        for x in 0..16_u32 {
            let (bx, by) = (x / 2, y / 2);
            pixels.extend_from_slice(&[low_byte(bx * 30), low_byte(by * 30), 40, 255]);
        }
    }
    let sheet = Sheet::new(UVec2::splat(16), &pixels).unwrap();
    let whole = PixelRect { x: 0, y: 0, width: 16, height: 16 };
    let layer = Underlay::new(&sheet, whole, &geometry(), &IDENTITY).unwrap();
    for y in 0..LAYER {
        for x in 0..LAYER {
            assert_eq!(layer_pixel(&layer, x, y), [low_byte(x * 30), low_byte(y * 30), 40, 255]);
        }
    }
}

#[test]
fn a_whole_pixel_offset_moves_the_picture_right() {
    // `+x` moves the picture right: the sheet column at 4 + x shows one layer column further right, at x + 1.
    let pixels = coded_sheet(16);
    let sheet = Sheet::new(UVec2::splat(16), &pixels).unwrap();
    let align = UnderlayAlign { offset: [1.0, 0.0], ..IDENTITY };
    let layer = Underlay::new(&sheet, face(4, 4, LAYER), &geometry(), &align).unwrap();
    for y in 0..LAYER {
        for x in 0..LAYER - 1 {
            assert_eq!(layer_pixel(&layer, x + 1, y), texel_at(&pixels, 16, 4 + x, 4 + y), "column {x} moved right");
        }
    }
}

/// A sheet whose red channel is `8 * column` and whose alpha is opaque, so a sampled red reads back the sheet column it
/// came from, exactly: bilinear interpolation of a linear ramp is exact.
fn ramp_sheet(size: u32) -> Vec<u8> {
    let mut rgba = Vec::new();
    for _ in 0..size {
        for x in 0..size {
            rgba.extend_from_slice(&[low_byte(8 * x), 0, 0, 255]);
        }
    }
    rgba
}

#[test]
fn scale_two_doubles_the_picture_about_the_face_centre() {
    // The face rect (12, 12, 16) has its centre at sheet (20, 20); the layer is 8 px, so each layer pixel spans 2 sheet
    // px. Scale 2 doubles the picture about that centre: the sheet point at distance D from it shows at distance 2D. So
    // layer column i, at distance 2 (i - 3.5) from the centre, shows the sheet point at distance i - 3.5, which is
    // sheet column 16 + i. Against the inverted sign this reads 4i + 6 instead, which is the bug this test catches.
    let pixels = ramp_sheet(32);
    let sheet = Sheet::new(UVec2::splat(32), &pixels).unwrap();
    let align = UnderlayAlign { scale: 2.0, ..IDENTITY };
    let layer = Underlay::new(&sheet, face(12, 12, 16), &geometry(), &align).unwrap();
    for y in 0..LAYER {
        for x in 0..LAYER {
            assert_eq!(layer_pixel(&layer, x, y), [low_byte(8 * (16 + x)), 0, 0, 255], "pixel ({x}, {y})");
        }
    }
}

#[test]
fn a_quarter_turn_moves_the_picture_from_plus_x_to_plus_y() {
    // A picture point on +x from the centre ends on +y, so the sheet's x ramp runs down the layer instead of across it:
    // layer row j shows sheet column 13 + 2j (red 100 + 16 j), whatever the column.
    let pixels = ramp_sheet(32);
    let sheet = Sheet::new(UVec2::splat(32), &pixels).unwrap();
    let align = UnderlayAlign { rotation: FRAC_PI_2, ..IDENTITY };
    let layer = Underlay::new(&sheet, face(12, 12, 16), &geometry(), &align).unwrap();
    for y in 0..LAYER {
        for x in 0..LAYER {
            assert_eq!(layer_pixel(&layer, x, y), [low_byte(100 + 16 * y), 0, 0, 255], "pixel ({x}, {y})");
        }
    }
}

#[test]
fn bilinear_sampling_is_premultiplied_so_an_edge_does_not_darken() {
    // Column 0 is opaque white and column 1 is clear. Offset 0.5 samples layer column 1 exactly halfway between them: the
    // white gives half its colour and half its alpha, the clear column nothing, so the pixel is (128, 128, 128, 128).
    // Averaging straight colour instead would give a 50% grey that darkens once premultiplied.
    let mut pixels = Vec::new();
    for _ in 0..8 {
        for x in 0..8 {
            let texel: &[u8] = if x == 0 { &[255, 255, 255, 255] } else { &[0, 0, 0, 0] };
            pixels.extend_from_slice(texel);
        }
    }
    let sheet = Sheet::new(UVec2::splat(8), &pixels).unwrap();
    let align = UnderlayAlign { offset: [0.5, 0.0], ..IDENTITY };
    let layer = Underlay::new(&sheet, face(0, 0, 8), &geometry(), &align).unwrap();
    for y in 0..LAYER {
        assert_eq!(layer_pixel(&layer, 1, y), [128, 128, 128, 128], "row {y}");
    }
}

#[test]
fn px_per_m_is_checked_before_the_layer_is_sized_or_allocated() {
    let pixels = coded_sheet(16);
    let sheet = Sheet::new(UVec2::splat(16), &pixels).unwrap();
    let build = |px_per_m: f32| {
        let geometry = Geometry::new(&projection(), Vec3::ZERO, px_per_m)?;
        Underlay::new(&sheet, face(4, 4, LAYER), &geometry, &IDENTITY)
    };
    // NaN, zero and negative would each round to a 1x1 layer without complaint, so each is refused by name.
    for px_per_m in [f32::NAN, f32::INFINITY, 0.0, -20.0] {
        let err = build(px_per_m).unwrap_err();
        assert_eq!(err.field, "px_per_m", "{px_per_m}: {err}");
        assert!(err.to_string().contains("px_per_m"), "{px_per_m}: {err}");
    }
    // 1e6 px/m is a 400 000 px side: refused against the renderer's limit, before any buffer exists.
    let err = build(1.0e6).unwrap_err();
    assert_eq!(err.field, "px_per_m");
    assert!(err.message.contains("px_per_m") && err.message.contains("4096"), "{err}");
}

#[test]
fn partly_transparent_pixels_are_premultiplied_and_the_outside_is_clear() -> Result<(), Box<dyn std::error::Error>> {
    // Straight (200, 100, 50) at alpha 128 becomes premultiplied (100, 50, 25) at alpha 128.
    let pixels = [200_u8, 100, 50, 128].repeat(16 * 16);
    let sheet = Sheet::new(UVec2::splat(16), &pixels)?;
    let whole = PixelRect { x: 0, y: 0, width: 16, height: 16 };
    let layer = Underlay::new(&sheet, whole, &geometry(), &IDENTITY)?;
    assert_eq!(layer_pixel(&layer, 3, 3), [100, 50, 25, 128]);

    // Sampled a whole sheet away, every texel is outside the sheet: the layer is transparent, not a repeated edge.
    let far = UnderlayAlign { offset: [400.0, 0.0], ..IDENTITY };
    let clear = Underlay::new(&sheet, whole, &geometry(), &far)?;
    assert!(clear.rgba().iter().all(|&b| b == 0));
    Ok(())
}

proptest! {
    /// Registration maps are exact inverses over the whole useful range, in both directions, well inside 1e-4 px.
    #[test]
    fn registration_round_trips_within_a_thousandth_of_a_pixel(
        offset_x in -50.0..50.0_f64,
        offset_y in -50.0..50.0_f64,
        scale in 0.25..4.0_f64,
        rotation in -3.2..3.2_f64,
        layer_x in 0.0..1548.0_f64,
        layer_y in 0.0..1548.0_f64,
        sheet_x in 0.0..2048.0_f64,
        sheet_y in 0.0..2048.0_f64,
    ) {
        let align = UnderlayAlign { offset: [offset_x, offset_y], scale, rotation };
        let map = Registration::new(
            PixelRect { x: 300, y: 400, width: 1536, height: 1400 },
            UVec2::new(1548, 1548),
            align,
        );
        let [lx, ly] = map.to_layer(map.to_sheet([layer_x, layer_y]));
        prop_assert!((lx - layer_x).abs() < 1e-4 && (ly - layer_y).abs() < 1e-4, "layer {lx}, {ly}");
        let [sx, sy] = map.to_sheet(map.to_layer([sheet_x, sheet_y]));
        prop_assert!((sx - sheet_x).abs() < 1e-4 && (sy - sheet_y).abs() < 1e-4, "sheet {sx}, {sy}");
    }
}

#[test]
fn opacity_is_the_overlay_strength_and_changes_no_pixels() -> Result<(), Box<dyn std::error::Error>> {
    let pixels = coded_sheet(16);
    let sheet = Sheet::new(UVec2::splat(16), &pixels)?;
    let mut layer = Underlay::new(&sheet, face(4, 4, LAYER), &geometry(), &IDENTITY)?;
    let before = layer.rgba().to_vec();
    let mut renderer = MockRenderer::new();
    let texture = renderer.create_texture(layer.texture_desc(), Some(layer.rgba()))?;

    layer.set_opacity(0.0)?;
    assert_eq!(layer.overlay(texture).strength, 0.0);
    assert_eq!(layer.rgba(), before.as_slice(), "opacity must not touch the pixels");
    layer.set_opacity(0.25)?;
    assert_eq!(layer.overlay(texture).strength, 0.25);
    assert!(layer.set_opacity(1.5).is_err());
    assert!(layer.set_opacity(f64::NAN).is_err());
    assert_eq!(layer.opacity(), 0.25, "a rejected opacity leaves the last good one");
    assert_eq!(UnderlayView::of(&layer).opacity, 0.25);
    Ok(())
}

fn camera() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    )
}

/// Renders a quad carrying the underlay, then the Base layer (`base` pixels) over it, the order the viewport uses. Returns
/// two pixels: the centre, inside the face window, and one on the quad but past the window.
fn composite(layer: &UnderlayLayer, base: &[u8]) -> Result<([u8; 4], [u8; 4]), Box<dyn std::error::Error>> {
    let proj = projection();
    let mut renderer = MockRenderer::new();
    let base_desc = face_texture_desc(&FaceLayerSpec::for_layer(FaceLayerId::Base, &proj, PX_PER_M));
    let under = renderer.create_texture(layer.texture_desc(), Some(layer.rgba()))?;
    let base_texture = renderer.create_texture(&base_desc, Some(base))?;
    let mut skin = Material::unlit(Colour::WHITE);
    skin.overlays =
        vec![layer.overlay(under), FaceOverlay::of(&proj, Vec3::ZERO, FaceLayerId::Base).to_overlay(base_texture)];
    let mesh = renderer.create_mesh(&shapes::quad(Vec2::splat(0.5)))?;
    let material = renderer.create_material(&skin)?;
    let mut scene = Scene::new(camera());
    scene.background = Background::Solid { colour: Colour::BLACK };
    scene.post.tonemap = Tonemap::None;
    scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
    let quality = Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() };
    let image = renderer.render_image(UVec2::splat(64), AlphaEncoding::Straight, &scene, &quality)?;
    let at = |x, y| image.pixel(x, y).ok_or("pixel outside the image");
    Ok((at(32, 32)?, at(43, 32)?))
}

fn assert_close(got: [u8; 4], want: [u8; 4]) {
    for (g, w) in got.iter().zip(want) {
        assert!(g.abs_diff(w) <= 1, "pixel {got:?}, expected {want:?}");
    }
}

#[test]
fn the_underlay_sits_under_the_base_and_hides_at_zero_opacity() -> Result<(), Box<dyn std::error::Error>> {
    let blue = [0_u8, 0, 255, 255].repeat(16 * 16);
    let sheet = Sheet::new(UVec2::splat(16), &blue)?;
    let whole = PixelRect { x: 0, y: 0, width: 16, height: 16 };
    let mut layer = Underlay::new(&sheet, whole, &geometry(), &IDENTITY)?;
    let clear = vec![0_u8; (LAYER * LAYER * 4) as usize];
    let red = [255_u8, 0, 0, 255].repeat((LAYER * LAYER) as usize);
    let (white, blue_px) = ([255_u8, 255, 255, 255], [0_u8, 0, 255, 255]);

    // A clear Base lets the underlay through; the face window is where it shows, the edge past it stays the skin.
    let (centre, edge) = composite(&layer, &clear)?;
    assert_close(centre, blue_px);
    assert_close(edge, white);

    // An opaque Base covers the underlay, so the underlay really is under the Base.
    assert_close(composite(&layer, &red)?.0, [255_u8, 0, 0, 255]);

    layer.set_opacity(0.0)?;
    assert_close(composite(&layer, &clear)?.0, white);
    Ok(())
}

#[test]
fn errors_name_the_field_at_fault() -> Result<(), Box<dyn std::error::Error>> {
    let pixels = coded_sheet(16);
    let short = Sheet::new(UVec2::splat(16), &pixels[..10]).unwrap_err();
    assert_eq!(short.field, "sheet_rgba");
    assert!(short.to_string().contains("sheet_rgba"), "{short}");

    let sheet = Sheet::new(UVec2::splat(16), &pixels)?;
    let outside = Underlay::new(
        &sheet,
        PixelRect { x: 12, y: 0, width: 8, height: 8 },
        &geometry(),
        &IDENTITY,
    );
    let outside = outside.unwrap_err();
    assert_eq!(outside.field, "sheet_face");
    assert!(outside.to_string().contains("sheet_face"));

    let zero_scale = UnderlayAlign { scale: 0.0, ..IDENTITY };
    let bad_scale = Underlay::new(&sheet, face(0, 0, LAYER), &geometry(), &zero_scale);
    let bad_scale = bad_scale.unwrap_err();
    assert_eq!(bad_scale.field, "align.scale");
    assert!(bad_scale.to_string().contains("align.scale"));

    let request = |opacity: f64, align: UnderlayAlign| UnderlaySet {
        character: CharId::new("char-a").unwrap(),
        asset: ContentHash([0; 32]),
        opacity,
        align,
        sheet_face: None,
    };
    assert_eq!(request(1.5, IDENTITY).validate().unwrap_err().field.as_deref(), Some("opacity"));
    let nan_offset = UnderlayAlign { offset: [f64::NAN, 0.0], ..IDENTITY };
    assert_eq!(request(0.5, nan_offset).validate().unwrap_err().field.as_deref(), Some("align.offset"));
    Ok(())
}

#[test]
fn the_request_reads_its_json_and_rejects_unknown_fields() -> Result<(), Box<dyn std::error::Error>> {
    let json = json!({
        "character": "char-a",
        "asset": "0".repeat(64),
        "opacity": 0.5,
        "align": { "offset": [1.0, -2.0], "scale": 1.0, "rotation": 0.0 }
    });
    let request: UnderlaySet = serde_json::from_value(json.clone())?;
    assert_eq!(request.opacity, 0.5);
    request.validate()?;
    assert_eq!(serde_json::to_value(&request)?, json);

    let mut extra = json;
    extra["gain"] = json!(2);
    assert!(serde_json::from_value::<UnderlaySet>(extra).is_err(), "an unknown field must be refused");
    Ok(())
}
