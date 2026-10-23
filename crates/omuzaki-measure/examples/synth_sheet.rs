//! Draws the synthetic faces of the likeness design's Appendix A in both styles, measures each with its true
//! landmarks, and writes one PNG per face with the measured outline (green) over the analytic one (blue), plus a sheet
//! of all six: `cargo run -p omuzaki-measure --example synth_sheet -- <out-dir> [iod-px]`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::print_stdout, reason = "an example that writes evidence")]
#![allow(clippy::needless_range_loop, reason = "three arrays indexed by angle")]
#![allow(clippy::disallowed_methods, reason = "an example writes its images to the folder it is given")]

#[path = "../tests/support/mod.rs"]
mod support;

use ::image::ImageEncoder;
use ::image::codecs::png::PngEncoder;
use omuzaki_measure::face::draw::{Canvas, OK, REFERENCE};
use omuzaki_measure::face::outline::{ANGLES, angle};
use omuzaki_measure::image::Rgba8;
use support::synth::{FACE_A, FACE_B, FACE_C, Shot, Style, draw};

fn save(image: &Rgba8, path: &std::path::Path) {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&image.pixels, image.width, image.height, ::image::ExtendedColorType::Rgba8)
        .unwrap();
    std::fs::write(path, out).unwrap();
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("an output folder"));
    let iod: f64 = args.next().map_or(120.0, |s| s.parse().expect("an IOD in pixels"));
    std::fs::create_dir_all(&dir).unwrap();
    let mut tiles = Vec::new();
    for style in [Style::RenderLike, Style::Drawing] {
        for (name, face) in [("A", FACE_A), ("B", FACE_B), ("C", FACE_C)] {
            let drawn = draw(&face, style, Shot::new(iod));
            let m = support::measure(&drawn, support::true_landmarks(&drawn), false);
            let mut c = Canvas::new(&drawn.image);
            let truth = face.truth_profile();
            match &m {
                Ok(m) => {
                    let mut sq = 0.0;
                    let mut n = 0.0;
                    let mut worst: f64 = 0.0;
                    for k in 0..ANGLES {
                        c.dot(m.frame.along(angle(k), truth[k] * iod), 1.2, REFERENCE);
                        if let Some(r) = m.outline.ok(k) {
                            c.dot(m.frame.along(angle(k), r * iod), 1.0, OK);
                            sq += (r - truth[k]).powi(2);
                            n += 1.0;
                            worst = worst.max((r - truth[k]).abs());
                        }
                    }
                    let rms: f64 = (sq / n).sqrt();
                    println!(
                        "{name} {style:?} iod {iod}: rms {:.2}% max {:.2}% ok {n} warnings {:?}",
                        rms * 100.0,
                        worst * 100.0,
                        m.warnings.iter().map(|w| w.code).collect::<Vec<_>>()
                    );
                }
                Err(e) => println!("{name} {style:?} iod {iod}: refused {} {:?}", e.field, e.text),
            }
            let image = c.finish();
            save(&image, &dir.join(format!("face_{name}_{style:?}.png")));
            tiles.push(image);
        }
    }
    // The sheet: three across, two down.
    let (tw, th) = (tiles[0].width as usize, tiles[0].height as usize);
    let mut sheet = vec![255_u8; tw * 3 * th * 2 * 4];
    for (i, t) in tiles.iter().enumerate() {
        let (ox, oy) = ((i % 3) * tw, (i / 3) * th);
        for y in 0..th {
            let row = &t.pixels[y * tw * 4..(y + 1) * tw * 4];
            let at = ((oy + y) * tw * 3 + ox) * 4;
            sheet[at..at + tw * 4].copy_from_slice(row);
        }
    }
    let sheet = Rgba8 {
        width: u32::try_from(tw * 3).unwrap(),
        height: u32::try_from(th * 2).unwrap(),
        pixels: sheet,
        hash: tiles[0].hash,
    };
    save(&sheet, &dir.join("sheet.png"));
}
