//! What the visual evidence sees of the face tools: the channel sweep's contact sheet (every face channel pushed to its
//! far end on the default face) and the built-in expression library at emote and stream sizes, as the range test paints
//! and flags it. Each case of `evidence/face-tools.toml` runs one test here with `OMUZAKI_FACE_EVIDENCE_DIR` set; in
//! ordinary test runs they return at once and write nothing.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use image::imageops::{FilterType, resize};
use omuzaki_char_api::HeadParams;
use omuzaki_core::colour::Colour;
use omuzaki_face::preview::metrics::downsample;
use omuzaki_face::preview::range::{RangeOptions, compare, paint_all};
use omuzaki_face::preview::sheet::{Cell, compose};
use omuzaki_face::preview::sweep::{ChannelStatus, SweepOptions, face_channels, sweep};
use omuzaki_face::preview::{FaceSubject, sweep::default_min_changed_px};
use omuzaki_face_api::{ExpressionSet, FaceProfile, Palette};

/// The default face with `expressions`, on the default head and the starting skin.
fn subject(expressions: ExpressionSet) -> FaceSubject {
    let profile = FaceProfile { expressions, ..FaceProfile::default() };
    FaceSubject {
        profile: profile.resolve_colours(&Palette::default()).unwrap(),
        projection: HeadParams::default().face_projection(),
        skin: Colour::rgb(0xf9, 0xd3, 0xc5),
    }
}

fn evidence_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("OMUZAKI_FACE_EVIDENCE_DIR").map(std::path::PathBuf::from)
}

#[test]
fn render_the_channel_sweep_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    let width_px = 387;
    let options = SweepOptions {
        channels: face_channels(),
        width_px,
        min_changed_px: default_min_changed_px(width_px),
        keep_pictures: true,
    };
    let report = sweep(&subject(ExpressionSet::default()), &options, &|| false).unwrap().unwrap();
    let mut cells =
        vec![Cell { picture: downsample(report.rest.as_ref().unwrap(), 160), label: "rest".into(), flagged: false }];
    for row in &report.rows {
        let picture = downsample(row.picture.as_ref().unwrap(), 160);
        cells.push(Cell { picture, label: row.channel.to_owned(), flagged: row.status == ChannelStatus::Dead });
    }
    compose(&cells, 10).save(dir.join("channel-sweep.png")).unwrap();
}

#[test]
fn render_the_expression_library_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    let face = subject(ExpressionSet::library().unwrap());
    let options = RangeOptions { sizes: vec![28, 56, 112], ..RangeOptions::default() };
    let painted = paint_all(&face, &options, &|| false).unwrap().unwrap();
    let report = compare(&painted, &options, &face);
    for (s, &size) in options.sizes.iter().enumerate() {
        let flagged = |name: &str| report.similar.iter().any(|p| p.size == size && (p.a == name || p.b == name));
        let cells: Vec<Cell> = painted
            .iter()
            .map(|p| {
                let crop = &p.crops[s];
                // Small sizes are blown up with whole pixels, so the sheet shows what a viewer gets.
                let factor = 160_u32.div_ceil(crop.width()).max(1);
                let picture = resize(crop, crop.width() * factor, crop.height() * factor, FilterType::Nearest);
                Cell { picture, label: p.name.clone(), flagged: flagged(&p.name) }
            })
            .collect();
        compose(&cells, 6).save(dir.join(format!("library-{size}px.png"))).unwrap();
    }
}
