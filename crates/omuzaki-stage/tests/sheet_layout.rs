//! The character-sheet layout engine (EX-10), end to end on a synthetic sheet: four flat silhouettes as the
//! turnaround, twelve labelled expression tiles and eight palette swatches, plus the title block.
//!
//! Nothing here is a real character (`docs/licensing/policy.md`): the figures are flat shapes drawn below.
//!
//! The 150 dpi A4 sheet is compared with `tests/golden/sheet_a4_150.png`; `OMUZAKI_BLESS=1 cargo test` rewrites it after
//! an intended change (look at the picture first). Set `OMUZAKI_SHEET_PNG=/some/dir` to write the A4 150 / 300 dpi sheets
//! and the 16:9 board there as PNGs.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
// Drawing helpers: coordinates are small pixel values, colours are written as plain 0xrrggbb, and the shape code reads
// better with the usual x, y, c, p names.
#![allow(clippy::unreadable_literal, clippy::many_single_char_names, clippy::similar_names)]
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]

use std::path::{Path, PathBuf};

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{Rgba, RgbaImage};
use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{JointFit, JointRef};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_stage::sheet::layout::{
    Board, Cell, Dpi, LayoutError, Orientation, Page, Paper, Proportions, Rect, Ruler, RulerSpan, SheetDate,
    SheetLayout, Swatch, TextStyle, Theme, TitleBlock, WcagGrade, Weight, contrast_ratio,
};

/// 2026-10-12T00:00:00Z: the shot clock of every sheet below.
const SHOT_CLOCK: Seconds = Seconds(1_791_763_200.0);
/// The silhouettes put the crown at 4% and the soles at 96% of their picture, 7.5 heads apart.
const CROWN: f32 = 0.04;
const SOLES: f32 = 0.96;

// ---------------------------------------------------------------------------------------------------------------
// Synthetic art: flat shapes from signed-distance functions, anti-aliased by coverage.
// ---------------------------------------------------------------------------------------------------------------

struct Painter {
    w: u32,
    h: u32,
    /// Premultiplied RGBA in 0..1.
    px: Vec<[f32; 4]>,
}

impl Painter {
    fn new(w: u32, h: u32) -> Self {
        Self { w, h, px: vec![[0.0; 4]; (w * h) as usize] }
    }

    /// Paints `colour` where `sdf` (negative inside, in pixels) says so, within `bounds` = (x0, y0, x1, y1).
    fn shape(&mut self, colour: u32, alpha: f32, bounds: (f32, f32, f32, f32), sdf: impl Fn(f32, f32) -> f32) {
        let c =
            [(colour >> 16 & 255) as f32 / 255.0, (colour >> 8 & 255) as f32 / 255.0, (colour & 255) as f32 / 255.0];
        let (x0, y0) = (bounds.0.floor().max(0.0) as u32, bounds.1.floor().max(0.0) as u32);
        let (x1, y1) = ((bounds.2.ceil() as u32).min(self.w), (bounds.3.ceil() as u32).min(self.h));
        for y in y0..y1 {
            for x in x0..x1 {
                let cover = (0.5 - sdf(x as f32 + 0.5, y as f32 + 0.5)).clamp(0.0, 1.0) * alpha;
                if cover > 0.0 {
                    let p = &mut self.px[(y * self.w + x) as usize];
                    for i in 0..3 {
                        p[i] = c[i] * cover + p[i] * (1.0 - cover);
                    }
                    p[3] = cover + p[3] * (1.0 - cover);
                }
            }
        }
    }

    fn ellipse(&mut self, colour: u32, (cx, cy): (f32, f32), (rx, ry): (f32, f32)) {
        let k = rx.min(ry);
        self.shape(colour, 1.0, (cx - rx - 1.0, cy - ry - 1.0, cx + rx + 1.0, cy + ry + 1.0), move |x, y| {
            ((((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2)).sqrt() - 1.0) * k
        });
    }

    fn rounded(&mut self, colour: u32, (cx, cy): (f32, f32), (hx, hy): (f32, f32), r: f32) {
        self.shape(colour, 1.0, (cx - hx - 1.0, cy - hy - 1.0, cx + hx + 1.0, cy + hy + 1.0), move |x, y| {
            let (qx, qy) = ((x - cx).abs() - (hx - r), (y - cy).abs() - (hy - r));
            (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
        });
    }

    fn capsule(&mut self, colour: u32, alpha: f32, (ax, ay): (f32, f32), (bx, by): (f32, f32), r: f32) {
        let bounds = (ax.min(bx) - r - 1.0, ay.min(by) - r - 1.0, ax.max(bx) + r + 1.0, ay.max(by) + r + 1.0);
        self.shape(colour, alpha, bounds, move |x, y| {
            let (dx, dy) = (bx - ax, by - ay);
            let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
            ((x - ax - t * dx).powi(2) + (y - ay - t * dy).powi(2)).sqrt() - r
        });
    }

    fn polyline(&mut self, colour: u32, points: &[(f32, f32)], r: f32) {
        for pair in points.windows(2) {
            self.capsule(colour, 1.0, pair[0], pair[1], r);
        }
    }

    fn finish(self) -> RgbaImage {
        let mut img = RgbaImage::new(self.w, self.h);
        for (out, p) in img.pixels_mut().zip(&self.px) {
            let a = p[3];
            let un = |c: f32| if a > 0.0 { (c / a * 255.0 + 0.5).clamp(0.0, 255.0) as u8 } else { 0 };
            *out = Rgba([un(p[0]), un(p[1]), un(p[2]), (a * 255.0 + 0.5) as u8]);
        }
        img
    }
}

#[derive(Copy, Clone)]
enum View {
    Front,
    ThreeQuarter,
    Side,
    Back,
}

const SKIN: u32 = 0xe8c4b0;
const HAIR: u32 = 0x3d3550;
const OUTFIT: u32 = 0x6c63a8;
const OUTFIT_DARK: u32 = 0x4a4466;

/// A flat 7.5-head figure on a transparent 0.38:1 picture, crown at 4% and soles at 96% of the height.
fn silhouette(view: View) -> RgbaImage {
    let (w, h) = (380_u32, 1000_u32);
    let mut p = Painter::new(w, h);
    let head = h as f32 * (SOLES - CROWN) / 7.5; // one head unit in pixels
    let (c, top) = (w as f32 / 2.0, h as f32 * CROWN);
    let y = |units: f32| top + units * head;
    // How wide the figure reads from this side, and which way it turns.
    let (sx, shift) = match view {
        View::Front | View::Back => (1.0, 0.0),
        View::ThreeQuarter => (0.82, 0.07 * head),
        View::Side => (0.5, 0.0),
    };
    let x = |units: f32| c + shift + units * head * sx;

    // Back hair first, so the body stands in front of it.
    let long_hair = matches!(view, View::Back | View::Side | View::ThreeQuarter);
    if long_hair {
        let back = if matches!(view, View::Side) { 0.45 } else { 0.0 };
        p.rounded(HAIR, (x(-back), y(2.3)), (0.5 * head * sx.max(0.7), 1.35 * head), 0.4 * head);
    }
    // Legs and arms.
    let legs = if matches!(view, View::Side) { [0.0, 0.0] } else { [-0.26, 0.26] };
    for lx in legs {
        p.capsule(OUTFIT_DARK, 1.0, (x(lx), y(3.7)), (x(lx), y(7.2)), 0.23 * head);
        p.rounded(HAIR, (x(lx), y(7.33)), (0.27 * head, 0.17 * head), 0.12 * head);
    }
    let arms: &[f32] = if matches!(view, View::Side) { &[0.0] } else { &[-1.0, 1.0] };
    for &ax in arms {
        p.capsule(SKIN, 1.0, (x(ax * 0.82), y(1.55)), (x(ax * 1.02), y(3.6)), 0.15 * head);
        p.capsule(OUTFIT, 1.0, (x(ax * 0.82), y(1.5)), (x(ax * 0.9), y(2.5)), 0.19 * head);
    }
    // Torso: chest, waist and hips.
    p.rounded(OUTFIT, (x(0.0), y(2.0)), (0.74 * head * sx, 0.8 * head), 0.34 * head);
    p.rounded(OUTFIT, (x(0.0), y(3.1)), (0.6 * head * sx, 0.7 * head), 0.28 * head);
    p.rounded(OUTFIT_DARK, (x(0.0), y(3.75)), (0.66 * head * sx, 0.18 * head), 0.1 * head);
    // Neck and head: the hair cap's crown is the top of the figure, the chin is one head unit below it.
    p.rounded(SKIN, (x(0.0), y(1.12)), (0.12 * head, 0.2 * head), 0.05 * head);
    let face_w = 0.44 * head * if matches!(view, View::Side) { 0.86 } else { 1.0 };
    p.ellipse(HAIR, (x(0.0), y(0.5)), (face_w, 0.5 * head));
    if !matches!(view, View::Back) {
        p.ellipse(SKIN, (x(0.0), y(0.66)), (face_w * 0.86, 0.34 * head));
    }
    p.finish()
}

/// A flat face: eye openness, brow tilt, mouth curve (smile positive), mouth openness, blush.
struct Face {
    name: &'static str,
    eyes: f32,
    brow: f32,
    smile: f32,
    open: f32,
    blush: bool,
}

const FACES: [Face; 12] = [
    Face { name: "Neutral", eyes: 1.0, brow: 0.0, smile: 0.0, open: 0.0, blush: false },
    Face { name: "Happy", eyes: 0.18, brow: -0.1, smile: 1.0, open: 0.0, blush: false },
    Face { name: "Joy", eyes: 0.18, brow: -0.2, smile: 1.0, open: 0.8, blush: true },
    Face { name: "Sad", eyes: 0.8, brow: 0.5, smile: -0.8, open: 0.0, blush: false },
    Face { name: "Angry", eyes: 0.8, brow: -0.6, smile: -0.5, open: 0.2, blush: false },
    Face { name: "Surprised", eyes: 1.3, brow: -0.3, smile: 0.0, open: 1.0, blush: false },
    Face { name: "Smug", eyes: 0.55, brow: -0.2, smile: 0.6, open: 0.0, blush: false },
    Face { name: "Wink", eyes: 0.5, brow: -0.1, smile: 0.8, open: 0.0, blush: false },
    Face { name: "Shy", eyes: 0.9, brow: 0.3, smile: 0.3, open: 0.0, blush: true },
    Face { name: "Sleepy", eyes: 0.2, brow: 0.2, smile: 0.0, open: 0.4, blush: false },
    Face { name: "Pout", eyes: 0.9, brow: -0.2, smile: -0.6, open: 0.0, blush: true },
    Face { name: "Confused", eyes: 0.9, brow: 0.45, smile: -0.2, open: 0.3, blush: false },
];

fn face_tile(face: &Face) -> RgbaImage {
    let s = 240_u32;
    let mut p = Painter::new(s, s);
    let c = s as f32 / 2.0;
    p.ellipse(HAIR, (c, c * 0.92), (c * 0.98, c * 0.94));
    p.ellipse(SKIN, (c, c * 1.04), (c * 0.8, c * 0.84));
    p.ellipse(HAIR, (c, c * 0.52), (c * 0.86, c * 0.34));
    for side in [-1.0_f32, 1.0] {
        let ex = c + side * c * 0.34;
        let closed = face.name == "Wink" && side > 0.0;
        let open = if closed { 0.12 } else { face.eyes };
        p.ellipse(0x2b2438, (ex, c * 1.08), (c * 0.115, c * 0.2 * open.max(0.12)));
        p.polyline(
            0x2b2438,
            &[
                (ex - c * 0.16, c * (0.9 - side * face.brow * 0.12)),
                (ex + c * 0.16, c * (0.9 + side * face.brow * 0.12)),
            ],
            c * 0.022,
        );
        if face.blush {
            p.capsule(0xe88aa0, 0.55, (ex - c * 0.05, c * 1.3), (ex + c * 0.05, c * 1.3), c * 0.08);
        }
    }
    let (mx, my, mw) = (c, c * 1.46, c * 0.2);
    if face.open > 0.0 {
        p.ellipse(0x7a2c3c, (mx, my), (mw * (0.7 + 0.3 * face.open), c * 0.05 + c * 0.1 * face.open));
    } else {
        let curve: Vec<(f32, f32)> = (0..=8)
            .map(|i| {
                let t = i as f32 / 8.0 * 2.0 - 1.0;
                (mx + t * mw, my + (t * t - 0.5) * face.smile * c * 0.12)
            })
            .collect();
        p.polyline(0x7a2c3c, &curve, c * 0.022);
    }
    p.finish()
}

// ---------------------------------------------------------------------------------------------------------------
// The sheet.
// ---------------------------------------------------------------------------------------------------------------

/// Skin, hair and outfit colours are areas; the dark hair colour and the accent are also used for text.
fn palette() -> Vec<Swatch> {
    vec![
        Swatch::fill("Skin", Colour::hex(0xf2c9b5)),
        Swatch::fill("Skin shadow", Colour::hex(0xd9a58f)),
        Swatch::text("Hair", Colour::hex(0x3d3550)),
        Swatch::fill("Hair light", Colour::hex(0x8a6fb8)),
        Swatch::fill("Eyes", Colour::hex(0x2e8aa6)),
        Swatch::fill("Outfit", Colour::hex(0x6c63a8)),
        Swatch::fill("Trim", Colour::hex(0xf4d35e)),
        Swatch::text("Accent", Colour::hex(0xe0457b)),
    ]
}

/// Joints for a 1.5 m figure with a 0.2 m head: 7.5 heads, shoulders 1.18 x hips, legs 1.41 x torso.
fn body() -> JointFit {
    let at = |bone, x, y| (JointRef::Humanoid(bone), Vec3::new(x, y, 0.0));
    JointFit::new([
        at(HumanoidBone::Hips, 0.0, 0.62),
        at(HumanoidBone::Neck, 0.0, 1.06),
        at(HumanoidBone::LeftUpperArm, 0.118, 1.0),
        at(HumanoidBone::RightUpperArm, -0.118, 1.0),
        at(HumanoidBone::LeftUpperLeg, 0.1, 0.6),
        at(HumanoidBone::RightUpperLeg, -0.1, 0.6),
    ])
}

fn title(date: Option<SheetDate>) -> TitleBlock {
    TitleBlock {
        kicker: "Character sheet".into(),
        name: "Mira Quill".into(),
        template: "soft-sd".into(),
        variant: "Game face".into(),
        build: "9111d06".into(),
        date,
    }
}

/// What the acceptance sheet drew, with the geometry the tests check.
struct Rendered {
    image: RgbaImage,
    rulers: Vec<Ruler>,
    span: RulerSpan,
    /// The column where the rulers start; their ticks sit just left of it.
    ruler_x: i32,
    /// The four view cells.
    views: Vec<Rect>,
}

/// The acceptance sheet on `page`: title block, four-view turnaround under head-unit rulers, twelve expression tiles
/// and eight swatches.
fn acceptance_sheet(page: Page, date: Option<SheetDate>) -> Rendered {
    themed_sheet(page, Theme::light(), date)
}

fn themed_sheet(page: Page, theme: Theme, date: Option<SheetDate>) -> Rendered {
    let mut sheet = SheetLayout::new(page, theme).unwrap();
    let gap = sheet.metrics().gap;
    let content = sheet.title_block(&title(date));
    // Paper stacks the turnaround over the expressions and palette; a wide board puts the expressions beside the
    // turnaround and the palette in a strip across the bottom.
    let wide = page.width as f32 / page.height as f32 > 1.6;
    let (turnaround, expressions, palette_area) = if wide {
        let strip_h = sheet.section_heading_height() + sheet.swatch_rows_height(1);
        let (main, strip) = content.take_bottom(strip_h);
        let main = Rect::new(main.x, main.y, main.w, main.h - gap * 2);
        let [a, b] = <[Rect; 2]>::try_from(main.columns(&[0.46, 0.54], gap * 2)).unwrap();
        (a, b, strip)
    } else {
        let [top, bottom] = <[Rect; 2]>::try_from(content.rows(&[0.64, 0.36], gap * 2)).unwrap();
        let [a, b] = <[Rect; 2]>::try_from(bottom.columns(&[0.58, 0.42], gap * 2)).unwrap();
        (top, a, b)
    };

    // Turnaround.
    let proportions = Proportions::from_body(&body(), 1.5, 0.2).unwrap();
    let heading = sheet.section_heading(turnaround, "Turnaround", sheet.proportions_readout_width(&proportions));
    sheet.proportions_readout(heading.aside, heading.aside_baseline, &proportions);
    let (gutter, views) = heading.body.take_left(sheet.metrics().ruler_gutter);
    let cells = views.columns(&[1.0; 4], gap);
    let figures: Vec<Cell> =
        [(View::Front, "Front"), (View::ThreeQuarter, "3/4 view"), (View::Side, "Side"), (View::Back, "Back")]
            .into_iter()
            .map(|(view, label)| Cell::image(silhouette(view)).label(label).on_ground())
            .collect();

    // Rulers first, so the figures cover them: they are measured from where the first cell will put its picture.
    let picture = sheet.cell_geometry(cells[0], &figures[0]).image;
    let span = RulerSpan { top: picture.y_at(CROWN), bottom: picture.y_at(SOLES) };
    let ruler_x = gutter.right();
    let rulers = sheet.rulers(&proportions, span, (ruler_x, views.right())).unwrap();
    for (rect, cell) in cells.iter().zip(&figures) {
        sheet.place(*rect, cell);
    }

    // Expressions and palette.
    let body = sheet.section_heading(expressions, "Expressions", 0).body;
    for (rect, face) in body.grid(6, 2, gap, gap).into_iter().zip(&FACES) {
        sheet.place(rect, &Cell::image(face_tile(face)).caption(face.name).panel());
    }
    let body = sheet.section_heading(palette_area, "Palette", 0).body;
    sheet.swatches(body, &palette(), if wide { 8 } else { 4 });

    Rendered { image: sheet.finish(), rulers, span, ruler_x, views: cells }
}

fn a4(dpi: Dpi) -> Page {
    Page::paper(Paper::A4, Orientation::Landscape, dpi)
}

/// The date the shot clock gives: the clock here counts Unix seconds, as `SheetDate::from_clock` requires.
fn shot_date() -> SheetDate {
    SheetDate::from_clock(&FakeClock::new(SHOT_CLOCK))
}

// ---------------------------------------------------------------------------------------------------------------
// Comparison against the golden.
// ---------------------------------------------------------------------------------------------------------------

/// Pixels where any premultiplied channel differs by more than `per_channel`.
fn bad_pixels(a: &RgbaImage, b: &RgbaImage, per_channel: i32) -> usize {
    let premul = |p: &Rgba<u8>| p.0.map(|c| i32::from(c) * i32::from(p.0[3]) / 255);
    a.pixels()
        .zip(b.pixels())
        .filter(|(p, q)| {
            let (pa, qa) = (premul(p), premul(q));
            (0..3).any(|i| (pa[i] - qa[i]).abs() > per_channel)
                || (i32::from(p.0[3]) - i32::from(q.0[3])).abs() > per_channel
        })
        .count()
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(name)
}

#[test]
fn the_acceptance_sheet_matches_its_golden_at_150_dpi() {
    let actual = acceptance_sheet(a4(Dpi::D150), Some(shot_date())).image;
    assert_eq!((actual.width(), actual.height()), (1754, 1240));
    let path = golden_path("sheet_a4_150.png");
    if std::env::var_os("OMUZAKI_BLESS").is_some() {
        // The sheet is opaque, so the golden is stored as RGB with the best compression: about 150 KB.
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rgb = image::DynamicImage::ImageRgba8(actual.clone()).to_rgb8();
        let file = std::fs::File::create(&path).unwrap();
        let encoder = PngEncoder::new_with_quality(file, CompressionType::Best, FilterType::Adaptive);
        rgb.write_with_encoder(encoder).unwrap();
    }
    let golden = image::open(&path).expect("tests/golden/sheet_a4_150.png (OMUZAKI_BLESS=1 writes it)").to_rgba8();
    assert_eq!(golden.dimensions(), actual.dimensions(), "size changed");
    assert_eq!(bad_pixels(&actual, &golden, 2), 0, "the sheet drifted from the golden by more than 2 per channel");
}

#[test]
fn rendering_is_deterministic_to_the_byte() {
    let (first, second) =
        (acceptance_sheet(a4(Dpi::D150), Some(shot_date())), acceptance_sheet(a4(Dpi::D150), Some(shot_date())));
    assert_eq!(first.image.as_raw(), second.image.as_raw());
    assert_eq!(first.rulers, second.rulers);
}

#[test]
fn the_title_date_comes_from_the_shot_clock() {
    let clock = FakeClock::new(SHOT_CLOCK);
    let page = Page::custom(900, 300, 150.0).unwrap();
    let render = |date| {
        let mut sheet = SheetLayout::new(page, Theme::light()).unwrap();
        sheet.title_block(&title(Some(date)));
        sheet.finish()
    };
    let before = render(SheetDate::from_clock(&clock));
    assert_eq!(SheetDate::from_clock(&clock).to_string(), "2026-10-12");
    assert_eq!(before.as_raw(), render(SheetDate::from_clock(&clock)).as_raw(), "same clock, same bytes");
    clock.advance(Seconds(86_400.0 * 3.0));
    assert_eq!(SheetDate::from_clock(&clock).to_string(), "2026-10-15");
    assert_ne!(before.as_raw(), render(SheetDate::from_clock(&clock)).as_raw(), "a later shot clock changes the sheet");
}

#[test]
fn a_sheet_without_a_shot_date_leaves_the_date_out() {
    let (with, without) = (acceptance_sheet(a4(Dpi::D150), Some(shot_date())), acceptance_sheet(a4(Dpi::D150), None));
    assert_ne!(with.image.as_raw(), without.image.as_raw(), "the date column is the difference");
    assert_eq!(with.rulers, without.rulers, "and nothing else moved");
    let ink_in_title = |img: &RgbaImage| {
        (0..200).map(|y| (0..img.width()).filter(|&x| img.get_pixel(x, y).0[0] < 200).count()).sum::<usize>()
    };
    assert!(ink_in_title(&without.image) < ink_in_title(&with.image));
}

#[test]
fn a_tiny_dpi_is_refused_up_front_instead_of_hanging_the_rulers() {
    // `Page`'s fields are public, so a page can skip `Page::custom`: `SheetLayout::new` checks it again.
    let tiny = Page { width: 30, height: 20, dpi: 1.0 };
    assert!(
        matches!(SheetLayout::new(tiny, Theme::light()), Err(LayoutError::Dpi { dpi }) if (dpi - 1.0).abs() < f32::EPSILON)
    );
    assert!(matches!(Page::custom(30, 20, 1.0), Err(LayoutError::Dpi { .. })));

    // The smallest accepted resolution still rules a figure, in finite time.
    let mut sheet = SheetLayout::new(Page::custom(400, 300, 72.0).unwrap(), Theme::light()).unwrap();
    let proportions = Proportions { heads: 7.5, shoulder_to_hip: 1.0, leg_to_torso: 1.0 };
    let rulers = sheet.rulers(&proportions, RulerSpan { top: 20.0, bottom: 280.0 }, (40, 380)).unwrap();
    assert_eq!(rulers.len(), 8);
}

#[test]
fn nine_point_text_is_legible_at_300_dpi() {
    let sheet = SheetLayout::new(a4(Dpi::D300), Theme::light()).unwrap();
    let caption = sheet.styles().caption;
    assert!((caption.size_pt - 9.0).abs() < f32::EPSILON);
    let m = sheet.text_metrics(&caption);
    assert!(m.cap_height >= 24.0, "a 9 pt capital is {} px tall at 300 dpi, needs 24", m.cap_height);

    // And in the pixels: draw an H and measure its inked rows.
    let mut sheet = SheetLayout::new(Page::custom(200, 120, 300.0).unwrap(), Theme::light()).unwrap();
    let black = TextStyle::new(9.0, Weight::Regular, Colour::BLACK);
    sheet.text("H", &black, (20.0, 30), omuzaki_stage::sheet::layout::Align::Left, None);
    let img = sheet.finish();
    let inked_rows = (0..img.height()).filter(|&y| (0..img.width()).any(|x| img.get_pixel(x, y).0[0] < 100)).count();
    assert!(inked_rows >= 24, "{inked_rows} inked rows for a 9 pt H at 300 dpi");
}

#[test]
fn rulers_sit_on_the_head_unit_positions_to_a_pixel() {
    let r = acceptance_sheet(a4(Dpi::D150), Some(shot_date()));
    assert_eq!(r.rulers.len(), 8, "the crown line and rulers 1..7 for a 7.5-head figure");

    // The head unit is the figure's height over 7.5; each ruler is that many units below the crown.
    let unit = (r.span.bottom - r.span.top) / 7.5;
    let paper = Theme::light().background;
    let accent = Theme::light().accent;
    for ruler in &r.rulers {
        let expected = r.span.top + ruler.index as f32 * unit;
        assert!((ruler.y - expected).abs() <= 1.0, "ruler {} at {} not {expected}", ruler.index, ruler.y);
        // And it is drawn there: the tick left of the gutter edge is solid accent on that row, paper two rows off.
        let (x, row) = ((r.ruler_x - 2) as u32, expected.round() as u32);
        let pixel = |y: u32| r.image.get_pixel(x, y).0;
        if ruler.index > 0 {
            assert_eq!(pixel(row)[..3], [accent.r, accent.g, accent.b], "tick of ruler {}", ruler.index);
        }
        assert_ne!(pixel(row)[..3], [paper.r, paper.g, paper.b], "ruler {} is inked at row {row}", ruler.index);
        assert_eq!(pixel(row + 3)[..3], [paper.r, paper.g, paper.b], "ruler {} is one hairline", ruler.index);
    }

    // The art agrees: the first view's hair starts at the crown ruler and its shoes end at the soles, within the
    // half-pixel an anti-aliased edge can hide.
    let cell = r.views[0];
    let is_hair = |x: i32, y: i32| {
        let p = r.image.get_pixel(x as u32, y as u32).0;
        p[3] == 255 && p[..3].iter().zip([0x3d, 0x35, 0x50]).all(|(a, b): (&u8, i32)| (i32::from(*a) - b).abs() < 6)
    };
    let rows: Vec<i32> = (cell.y..cell.bottom()).filter(|&y| (cell.x..cell.right()).any(|x| is_hair(x, y))).collect();
    let (crown, soles) = (*rows.first().unwrap() as f32, (*rows.last().unwrap() + 1) as f32);
    assert!((crown - r.span.top).abs() <= 1.5, "crown {crown} vs ruler {}", r.span.top);
    assert!((soles - r.span.bottom).abs() <= 1.5, "soles {soles} vs {}", r.span.bottom);
}

#[test]
fn swatches_show_the_hex_and_the_wcag_ratio_against_the_sheet() {
    let page = Page::custom(1000, 500, 150.0).unwrap();
    let mut sheet = SheetLayout::new(page, Theme::light()).unwrap();
    let area = sheet.content();
    let reports = sheet.swatches(area, &palette(), 4);
    let background = Theme::light().background;
    assert_eq!(reports.len(), 8);
    let image = sheet.finish();
    for (report, swatch) in reports.iter().zip(palette()) {
        assert!((report.ratio - contrast_ratio(swatch.colour, background)).abs() < 1e-6);
        let centre =
            image.get_pixel((report.chip.x + report.chip.w / 2) as u32, (report.chip.y + report.chip.h / 2) as u32);
        assert_eq!(centre.0, [swatch.colour.r, swatch.colour.g, swatch.colour.b, 255], "{} chip", swatch.slot);
    }
    assert_eq!(WcagGrade::of(reports[2].ratio), WcagGrade::Aaa, "the dark hair clears 7:1 on the paper");
    assert_eq!(reports[6].grade, WcagGrade::Fail, "yellow trim on white paper is below AA");
}

#[test]
fn the_wcag_level_is_only_drawn_for_colours_used_as_text() {
    let page = Page::custom(500, 300, 150.0).unwrap();
    let colour = Colour::hex(0x3d3550); // 10.9:1 on the paper: AAA
    let render = |swatch: Swatch| {
        let mut sheet = SheetLayout::new(page, Theme::light()).unwrap();
        let area = sheet.content();
        let report = sheet.swatches(area, &[swatch], 1)[0];
        (sheet.finish(), report)
    };
    let ink = |img: &RgbaImage| img.pixels().filter(|p| p.0[0] > 60 && p.0[0] < 200).count();
    let (fill, fill_report) = render(Swatch::fill("Hair", colour));
    let (text, text_report) = render(Swatch::text("Hair", colour));
    assert_eq!((fill_report.ratio, fill_report.grade), (text_report.ratio, text_report.grade), "same measurement");
    assert!(ink(&text) > ink(&fill), "only the text colour carries the AAA badge");
}

#[test]
fn compositing_is_premultiplied_source_over() {
    // A 50% red pixel field over the paper: straight and premultiplied inputs must agree, and match the maths.
    let paper = Theme::light().background;
    let straight = RgbaImage::from_pixel(8, 8, Rgba([255, 0, 0, 128]));
    let premultiplied = RgbaImage::from_pixel(8, 8, Rgba([128, 0, 0, 128]));
    let expect = |channel: u8, src: u8| (f32::from(src) + f32::from(channel) * (1.0 - 128.0 / 255.0)).round() as i32;
    for cell in [Cell::image(straight), Cell::premultiplied(premultiplied)] {
        let mut sheet = SheetLayout::new(Page::custom(60, 60, 150.0).unwrap(), Theme::light()).unwrap();
        let rect = Rect::new(10, 10, 40, 40);
        sheet.place(rect, &cell);
        let px = sheet.finish().get_pixel(30, 30).0;
        for (i, (got, want)) in
            px.iter().zip([expect(paper.r, 128), expect(paper.g, 0), expect(paper.b, 0)]).enumerate()
        {
            assert!((i32::from(*got) - want).abs() <= 1, "channel {i}: {got} vs {want}");
        }
        assert_eq!(px[3], 255, "the sheet stays opaque");
    }
}

#[test]
fn every_page_preset_lays_out_a_full_sheet() {
    let mut pages = Vec::new();
    for paper in [Paper::A4, Paper::A3, Paper::Letter] {
        for orientation in [Orientation::Landscape, Orientation::Portrait] {
            pages.push(Page::paper(paper, orientation, Dpi::D150));
        }
    }
    pages.extend([Page::board(Board::Hd1080), Page::board(Board::Uhd4k), a4(Dpi::D300)]);
    for page in pages {
        let img = acceptance_sheet(page, Some(shot_date())).image;
        assert_eq!((img.width(), img.height()), (page.width, page.height));
    }
}

#[test]
fn the_font_is_embedded_small_and_licensed() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
    let size = |name: &str| std::fs::metadata(dir.join(name)).unwrap().len();
    assert!(size("Inter-Regular.subset.ttf") + size("Inter-SemiBold.subset.ttf") < 300 * 1024);
    let ofl = std::fs::read_to_string(dir.join("OFL.txt")).unwrap();
    assert!(ofl.contains("SIL OPEN FONT LICENSE Version 1.1") && ofl.contains("The Inter Project Authors"));
    let register =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/licensing/register.md"))
            .unwrap();
    for item in ["Inter", "ab_glyph"] {
        assert!(register.lines().any(|l| l.starts_with(&format!("| {item}"))), "{item} has a register row");
    }
}

#[test]
fn bad_inputs_are_errors_not_panics() {
    assert!(matches!(Page::custom(0, 10, 150.0), Err(LayoutError::Page { .. })));
    assert!(matches!(
        Proportions::from_body(&JointFit::default(), 1.5, 0.2),
        Err(LayoutError::MissingJoint { joint: "hips" })
    ));
}

/// Writes the evidence renders when `OMUZAKI_SHEET_PNG` names a directory.
#[test]
fn write_evidence_when_asked() {
    let Some(dir) = std::env::var_os("OMUZAKI_SHEET_PNG") else { return };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, page) in [
        ("sheet_a4_150dpi.png", a4(Dpi::D150)),
        ("sheet_a4_300dpi.png", a4(Dpi::D300)),
        ("board_16x9_1080p.png", Page::board(Board::Hd1080)),
        ("board_16x9_4k.png", Page::board(Board::Uhd4k)),
    ] {
        acceptance_sheet(page, Some(shot_date())).image.save(dir.join(name)).unwrap();
    }
    themed_sheet(Page::board(Board::Hd1080), Theme::dark(), Some(shot_date()))
        .image
        .save(dir.join("board_16x9_1080p_dark.png"))
        .unwrap();
}
