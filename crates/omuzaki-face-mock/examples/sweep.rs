//! Paints the starter expressions with the mock painter and writes a contact sheet PNG: a quick look at the whole
//! pipeline (profile + expressions -> channels -> paint parameters -> draw list -> pixels) without a GPU.
//!
//!     cargo run -p omuzaki-face-mock --example sweep -- /path/to/sheet.png

use image::{Rgba, RgbaImage};
use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{
    ExpressionId, ExpressionSet, FaceDrawing, FaceInput, FaceLayerId, FaceMapping, FacePainter, FaceRasterizer,
    FaceView, Raster,
};
use omuzaki_face_mock::{MockPainter, MockRasterizer, mock_profile};

const PX_PER_M: f32 = 1000.0;
const COLUMNS: u32 = 4;
const SKIN: [u8; 3] = [249, 211, 197];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_path = std::env::args().nth(1).unwrap_or_else(|| "face-mock-sweep.png".to_owned());
    let profile = mock_profile();
    let set = ExpressionSet::starter();
    let mapping = FaceMapping::default();
    let id = |s: &str| ExpressionId::new(s);
    let mut painter = MockPainter::default();
    let spec = *painter.layers().iter().find(|l| l.id == FaceLayerId::Base).ok_or("no base layer")?;

    let blend = |name: &str, weight: f32| -> Result<ChannelBuf, Box<dyn std::error::Error>> {
        Ok(set.blend(&ChannelBuf::NEUTRAL, &[(&id(name)?, weight)])?)
    };
    let mut wink = ChannelBuf::NEUTRAL;
    wink.set(Channel::EYE_BLINK_LEFT, 1.0);
    wink.set(Channel::EYE_ARC_LEFT, 1.0);
    let mut smirk = ChannelBuf::NEUTRAL;
    smirk.set(Channel::MOUTH_SMILE_LEFT, 0.8);
    smirk.set(Channel::BROW_OUTER_UP_RIGHT, 0.8);
    smirk.set_gaze(0.6, 0.0);
    let cells: Vec<(&str, ChannelBuf)> = vec![
        ("neutral", ChannelBuf::NEUTRAL),
        ("happy", blend("happy", 1.0)?),
        ("surprised", blend("surprised", 0.8)?),
        ("angry", blend("angry", 1.0)?),
        ("sad", blend("sad", 1.0)?),
        ("aa viseme", blend("aa", 0.6)?),
        ("wink", wink),
        ("smirk + glance", smirk),
    ];

    let (cw, ch) = spec.rect.size_px(PX_PER_M);
    let rows = u32::try_from(cells.len())?.div_ceil(COLUMNS);
    let mut sheet = RgbaImage::from_pixel(cw * COLUMNS, ch * rows, Rgba([SKIN[0], SKIN[1], SKIN[2], 255]));
    for (i, (name, buf)) in cells.iter().enumerate() {
        let params = mapping.apply(buf);
        let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
        let mut drawing = FaceDrawing::new();
        painter.paint(&input, &mut drawing)?;
        let mut r = Raster::new(1, 1);
        MockRasterizer.rasterize(&spec, &drawing[FaceLayerId::Base], PX_PER_M, &mut r)?;
        let (ox, oy) = ((u32::try_from(i)? % COLUMNS) * cw, (u32::try_from(i)? / COLUMNS) * ch);
        over(&mut sheet, &r, ox, oy);
        println!(
            "{name}: {} commands, fingerprint {:016x}",
            drawing[FaceLayerId::Base].cmds().len(),
            drawing[FaceLayerId::Base].fingerprint()
        );
    }
    sheet.save(&out_path)?;
    println!("wrote {out_path} ({}x{})", sheet.width(), sheet.height());
    Ok(())
}

/// Composites a premultiplied raster over the sheet.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255 before the cast
fn over(sheet: &mut RgbaImage, r: &Raster, ox: u32, oy: u32) {
    for (i, px) in r.pixels.chunks(4).enumerate() {
        let i = u32::try_from(i).unwrap_or(0);
        let dst = sheet.get_pixel_mut(ox + i % r.width, oy + i / r.width);
        let inv = 1.0 - f32::from(px[3]) / 255.0;
        for (d, s) in dst.0.iter_mut().zip(&px[..3]) {
            *d = (f32::from(*s) + f32::from(*d) * inv).clamp(0.0, 255.0).round() as u8;
        }
    }
}
