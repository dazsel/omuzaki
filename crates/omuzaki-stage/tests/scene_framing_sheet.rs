//! The framing contact sheet: the four templates (soft-sd, mini, standard, tall) in the four framing presets (face, bust,
//! full, hands), at 16:9, 9:16 and 1:1, on the mock renderer. It is visual evidence for the Director's sign-off on the
//! framing (`tools/ci/evidence.py`, `evidence.toml`). The sheets are written only when `OMUZAKI_FRAMING_PNG` names a
//! folder; otherwise the one test that writes them returns at once.
//!
//! Rows are templates, columns are presets, and cells are separated by one-pixel grey gutters.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "scene/templates.rs"]
mod templates;

use std::path::Path;

use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Quality};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::ops::render::still::{Shot, Still, StillLight, StillSpec, render_stills};
use omuzaki_stage::recipe::LightRig;
use omuzaki_stage::scene::frame::frame;
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage_api::{CameraDef, Framing};

/// The presets, in column order.
const PRESETS: [Framing; 4] = [Framing::Face, Framing::Bust, Framing::Full, Framing::Hands];
/// The sheets written for the evidence case: file name, pixels per picture, and the picture's aspect (width / height).
const SHEETS: [(&str, UVec2, f32); 3] = [
    ("framing-16x9.png", UVec2::new(480, 270), 16.0 / 9.0),
    ("framing-9x16.png", UVec2::new(270, 480), 9.0 / 16.0),
    ("framing-1x1.png", UVec2::new(320, 320), 1.0),
];
/// The grey between cells, pixels.
const GUTTER: u32 = 1;
const GUTTER_RGBA: [u8; 4] = [128, 128, 128, 255];
/// Columns (presets) and rows (templates) of the sheet.
const COLUMNS: u32 = 4;
const ROWS: u32 = 4;

/// The viewer studio light, as the stills take it.
fn light() -> StillLight {
    let rig = LightRig::load("viewer-studio").expect("the light rig loads");
    let lights = rig.to_lights().expect("the rig's lights convert");
    StillLight::new(lights, rig.background(), rig.post(false))
}

/// One template's four presets at `cell` pixels and `aspect`, in [`PRESETS`] order.
fn row(template: &str, cell: UVec2, aspect: f32) -> Vec<Still> {
    let built = templates::build(template);
    let landmarks = Landmarks::measure(&built).expect("the template measures");
    let shots: Vec<Shot> = PRESETS
        .iter()
        .map(|&framing| {
            let camera = frame(framing, &landmarks, &CameraDef::preset(framing), aspect).expect("the preset frames");
            Shot::new(camera)
        })
        .collect();
    let spec = StillSpec::new(light(), cell, Quality::offline());
    render_stills(&mut MockRenderer::new(), built, &spec, &shots).expect("the row renders")
}

/// The sheet of every template and preset at `cell` pixels per picture, PNG-encoded: templates as rows, presets as
/// columns, one grey gutter between cells.
fn sheet_png(cell: UVec2, aspect: f32) -> Vec<u8> {
    let width = COLUMNS * cell.x + (COLUMNS - 1) * GUTTER;
    let height = ROWS * cell.y + (ROWS - 1) * GUTTER;
    let row_bytes = cell.x as usize * 4;
    let mut pixels: Vec<u8> = GUTTER_RGBA.iter().copied().cycle().take(width as usize * height as usize * 4).collect();
    for (r, template) in (0..ROWS).zip(templates::TEMPLATES) {
        for (c, still) in (0..COLUMNS).zip(row(template, cell, aspect)) {
            assert_eq!(still.image.size, cell, "every still is the cell size");
            assert_eq!(still.image.alpha, AlphaEncoding::Straight, "the target is straight alpha");
            let left = c * (cell.x + GUTTER);
            for y in 0..cell.y {
                let from = y as usize * row_bytes;
                let to = ((r * (cell.y + GUTTER) + y) * width + left) as usize * 4;
                pixels[to..to + row_bytes].copy_from_slice(&still.image.rgba8[from..from + row_bytes]);
            }
        }
    }
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&pixels, width, height, ExtendedColorType::Rgba8)
        .expect("the sheet encodes");
    bytes
}

#[test]
fn the_sheet_is_deterministic() {
    let small = UVec2::new(64, 36);
    assert_eq!(sheet_png(small, 16.0 / 9.0), sheet_png(small, 16.0 / 9.0), "two renders give the same bytes");
}

#[test]
fn the_sheet_has_a_gutter_between_every_cell() {
    let small = UVec2::new(64, 36);
    let decoded = image::load_from_memory(&sheet_png(small, 16.0 / 9.0)).expect("the sheet decodes").to_rgba8();
    let (width, height) = (COLUMNS * small.x + (COLUMNS - 1) * GUTTER, ROWS * small.y + (ROWS - 1) * GUTTER);
    assert_eq!(decoded.dimensions(), (width, height), "four cells and three gutters across, the same down");
    assert_eq!(decoded.get_pixel(small.x, 10).0, GUTTER_RGBA, "the gutter between two columns is grey");
    assert_eq!(decoded.get_pixel(10, small.y).0, GUTTER_RGBA, "the gutter between two rows is grey");
}

#[test]
// The evidence case writes its own scratch folder, which no op reaches; a test fixture may write by path.
#[allow(clippy::disallowed_methods)]
fn write_the_framing_sheet_when_asked() {
    let Some(dir) = std::env::var_os("OMUZAKI_FRAMING_PNG") else { return };
    for (name, cell, aspect) in SHEETS {
        std::fs::write(Path::new(&dir).join(name), sheet_png(cell, aspect)).expect("the sheet is written");
    }
}
