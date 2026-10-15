//! The framing contact sheet (evidence for `frame`): the four templates (rows) at face, bust, full and hands (columns),
//! at 16:9 and 9:16, on the mock renderer. `crates/omuzaki-stage/evidence.toml` runs it for the Director's sign-off.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

#[path = "scene/templates.rs"]
mod templates;

use image::{Rgba, RgbaImage};
use omuzaki_core::glam::UVec2;
use omuzaki_render_api::Quality;
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::ops::render::still::{Shot, StillLight, StillSpec, render_stills};
use omuzaki_stage::recipe::LightRig;
use omuzaki_stage::scene::frame::frame;
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage_api::{CameraDef, Framing};

const PRESETS: [Framing; 4] = [Framing::Face, Framing::Bust, Framing::Full, Framing::Hands];
/// The colour of the 1 px lines between cells.
const GUTTER: Rgba<u8> = Rgba([128, 128, 128, 255]);

/// The sheet for cells of `cell` pixels: a row per template, a column per preset, 1 px gutters around every cell.
fn sheet(cell: UVec2) -> RgbaImage {
    let rig = LightRig::load("viewer-studio").expect("the light rig loads");
    let light = StillLight::new(rig.to_lights().expect("the rig's lights convert"), rig.background(), rig.post(false));
    let spec = StillSpec::new(light, cell, Quality::offline());
    let aspect = cell.x as f32 / cell.y as f32;
    let (columns, rows) = (PRESETS.len() as u32, templates::TEMPLATES.len() as u32);
    let mut out = RgbaImage::from_pixel(columns * (cell.x + 1) + 1, rows * (cell.y + 1) + 1, GUTTER);
    for (row, id) in templates::TEMPLATES.into_iter().enumerate() {
        let built = templates::build(id);
        let landmarks = Landmarks::measure(&built).expect("the template measures");
        let shots: Vec<Shot> = PRESETS
            .into_iter()
            .map(|framing| Shot::new(frame(framing, &landmarks, &CameraDef::preset(framing), aspect).expect("frames")))
            .collect();
        let stills = render_stills(&mut MockRenderer::new(), built, &spec, &shots).expect("the stills render");
        for (column, still) in stills.iter().enumerate() {
            let picture = RgbaImage::from_raw(cell.x, cell.y, still.image.rgba8.clone()).expect("the size matches");
            image::imageops::replace(
                &mut out,
                &picture,
                1 + column as i64 * i64::from(cell.x + 1),
                1 + row as i64 * i64::from(cell.y + 1),
            );
        }
    }
    out
}

#[test]
fn the_sheet_is_deterministic() {
    let cell = UVec2::new(64, 36);
    let first = sheet(cell);
    assert_eq!(first.dimensions(), (4 * 65 + 1, 4 * 37 + 1));
    assert_eq!(first.as_raw(), sheet(cell).as_raw());
}

/// Writes `framing-16x9.png` and `framing-9x16.png` to the folder `OMUZAKI_FRAMING_PNG` names, if it is set.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn write_the_framing_sheet_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_FRAMING_PNG") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    for (name, cell) in [("framing-16x9.png", UVec2::new(480, 270)), ("framing-9x16.png", UVec2::new(270, 480))] {
        sheet(cell).save(directory.join(name)).unwrap();
    }
}
