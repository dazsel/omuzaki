//! The reference bar's face marks (#2393: #2403 to #2408) on the real painter: the anger vein, blank, dot, stare and
//! pill eyes, sharp teeth and single fangs, a right eye of its own colour, and deep gloom. Each one paints, reads apart
//! from the resting face at stream size, and keeps its promises (the vein pops, a single fang is on its side, the right
//! iris differs only on the right). With `OMUZAKI_FACE_EVIDENCE_DIR` set, `render_the_reference_marks_when_asked`
//! writes the contact sheet the pull request shows.

// Integration-test helpers may panic: a panic is a clear test failure. Pixel coordinates are small.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use image::RgbaImage;
use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_char_api::HeadParams;
use omuzaki_core::colour::Colour;
use omuzaki_face::preview::sheet::{Cell, compose, png};
use omuzaki_face::preview::{FaceSubject, Preview};
use omuzaki_face_api::{ColourRef, FaceLayerId, FaceProfile, FangKind, IrisColours, Palette};

const SKIN: Colour = Colour::rgb(0xf9, 0xd3, 0xc5);

fn subject(profile: &FaceProfile) -> FaceSubject {
    FaceSubject {
        profile: profile.resolve_colours(&Palette::default()).expect("fixed colours"),
        projection: HeadParams::default().face_projection(),
        skin: SKIN,
    }
}

fn bus(pairs: &[(Channel, f32)]) -> ChannelBuf {
    let mut b = ChannelBuf::NEUTRAL;
    for (c, v) in pairs {
        b.set(*c, *v);
    }
    b
}

fn picture(profile: &FaceProfile, pairs: &[(Channel, f32)], width: u32) -> RgbaImage {
    let mut preview = Preview::new(subject(profile), width);
    preview.paint(&bus(pairs)).unwrap();
    preview.picture()
}

/// Pixels that differ from `a` by more than a small tolerance in any channel.
fn changed(a: &RgbaImage, b: &RgbaImage) -> usize {
    a.pixels().zip(b.pixels()).filter(|(p, q)| p.0.iter().zip(q.0).any(|(x, y)| x.abs_diff(y) > 24)).count()
}

/// The share of a picture's pixels in its left (character's right) and right (character's left) halves that changed.
fn changed_by_half(a: &RgbaImage, b: &RgbaImage) -> (usize, usize) {
    let w = a.width();
    let (mut left, mut right) = (0, 0);
    for ((x, _, p), q) in a.enumerate_pixels().zip(b.pixels()) {
        if p.0.iter().zip(q.0).any(|(u, v)| u.abs_diff(v) > 24) {
            if x < w / 2 { left += 1 } else { right += 1 }
        }
    }
    (left, right)
}

fn heterochromia() -> FaceProfile {
    let mut p = FaceProfile::default();
    p.eyes.iris.top = ColourRef::rgb(0x6a, 0x10, 0x18);
    p.eyes.iris.mid = ColourRef::rgb(0xc8, 0x26, 0x30);
    p.eyes.iris.low = ColourRef::rgb(0xf0, 0x8a, 0x80);
    p.eyes.iris.ring = ColourRef::rgb(0x3a, 0x08, 0x0c);
    p.eyes.right_iris = Some(IrisColours {
        top: Some(ColourRef::rgb(0x1a, 0x2c, 0x5a)),
        mid: Some(ColourRef::rgb(0x50, 0x78, 0xb8)),
        low: Some(ColourRef::rgb(0xa8, 0xc4, 0xe8)),
        ring: Some(ColourRef::rgb(0x10, 0x18, 0x38)),
        pupil: None,
    });
    p
}

/// Every mark, alone at full strength (with the mouth open where it lives in the mouth), moves the face visibly at the
/// smallest emote size the readability measure uses (28 px), not only at full size.
#[test]
fn every_mark_reads_apart_from_the_resting_face_at_stream_size() {
    let profile = FaceProfile::default();
    let open = (Channel::JAW_OPEN, 0.6);
    let cases: [(&str, Vec<(Channel, f32)>); 7] = [
        ("angerVein", vec![(Channel::ANGER_VEIN, 1.0)]),
        ("eyesBlank", vec![(Channel::EYES_BLANK, 1.0)]),
        ("eyesDot", vec![(Channel::EYES_DOT, 1.0)]),
        ("eyesStare", vec![(Channel::EYES_STARE, 1.0)]),
        ("eyesPill", vec![(Channel::EYES_PILL, 1.0)]),
        ("teethSharp", vec![open, (Channel::TEETH_SHARP, 1.0)]),
        ("deep gloom", vec![(Channel::EYES_GLOOM, 1.0)]),
    ];
    for (name, pairs) in cases {
        let base: Vec<_> = pairs.iter().copied().filter(|(c, _)| *c == Channel::JAW_OPEN).collect();
        for width in [387_u32, 28] {
            let (rest, marked) = (picture(&profile, &base, width), picture(&profile, &pairs, width));
            let area = (rest.width() * rest.height()) as usize;
            let moved = changed(&rest, &marked);
            // At 28 px the face is a few hundred pixels: the mark must still change a visible share of them.
            // The default face's mouth is small (a few percent of the face), so a mouth mark needs fewer pixels.
            let share = if name == "teethSharp" { 2000 } else { 400 };
            let floor = if width == 28 { (area / (share / 4)).max(2) } else { area / share };
            assert!(moved >= floor, "{name} at {width} px changed {moved} of {area} pixels (need {floor})");
        }
    }
}

/// The vein pops: halfway through its fade-in it is bigger than at full strength (the overshoot), and it is on the
/// side the face style names. It is drawn in front of the hair (the Overlay layer) and nowhere else.
#[test]
fn the_anger_vein_pops_in_on_its_side_in_front_of_the_hair() {
    let mut profile = FaceProfile::default();
    let area = |p: &FaceProfile, v: f32| {
        let mut preview = Preview::new(subject(p), 387);
        preview.paint(&bus(&[(Channel::ANGER_VEIN, v)])).unwrap();
        let overlay = preview.layer(FaceLayerId::Overlay);
        let w = overlay.width as usize;
        let mut sides = (0_usize, 0_usize);
        for (i, px) in overlay.pixels.as_chunks::<4>().0.iter().enumerate() {
            if px[3] > 128 {
                if i % w < w / 2 { sides.0 += 1 } else { sides.1 += 1 }
            }
        }
        sides
    };
    let (half, full) = (area(&profile, 0.5), area(&profile, 1.0));
    assert!(full.1 > 200 && full.0 == 0, "the default vein is on the character's left (picture right): {full:?}");
    assert!(half.1 > full.1, "halfway in it overshoots: {} against {}", half.1, full.1);
    profile.effects.anger_vein.side = omuzaki_face_api::MarkSide::Right;
    let right = area(&profile, 1.0);
    assert!(right.0 > 200 && right.1 == 0, "the other side when the style says so: {right:?}");
    let mut preview = Preview::new(subject(&FaceProfile::default()), 387);
    preview.paint(&ChannelBuf::NEUTRAL).unwrap();
    let rest_base = preview.layer(FaceLayerId::Base).clone();
    preview.paint(&bus(&[(Channel::ANGER_VEIN, 1.0)])).unwrap();
    assert_eq!(&rest_base, preview.layer(FaceLayerId::Base), "nothing on the skin layer");
}

/// A single fang shows on its own side only: a face with one fang differs from a face with both only on the other
/// side, where the missing fang would be, and from a fangless face mostly on its own side.
#[test]
fn a_single_fang_is_on_its_own_side() {
    let fangs = [(Channel::JAW_OPEN, 0.45), (Channel::FANGS, 1.0)];
    let with = |kind: FangKind| {
        let mut p = FaceProfile::default();
        p.mouth.fang.kind = kind;
        picture(&p, &fangs, 774)
    };
    let (both, left, right, none) =
        (with(FangKind::Both), with(FangKind::OneLeft), with(FangKind::OneRight), with(FangKind::None));
    // The character's left is the picture's right half.
    let (l, r) = changed_by_half(&both, &left);
    assert!(l > 5 && r == 0, "one_left lacks only the right fang (the picture's left): {l} {r}");
    let (l, r) = changed_by_half(&both, &right);
    assert!(r > 5 && l == 0, "one_right lacks only the left fang (the picture's right): {l} {r}");
    let (l, r) = changed_by_half(&none, &left);
    assert!(r > 5, "one_left shows a fang on the character's left: {l} {r}");
}

/// A right iris of its own colours paints the right eye in them and leaves the left eye as it was; a profile without
/// one paints both eyes alike, exactly as before.
#[test]
fn the_right_eye_can_have_its_own_colour() {
    let plain = FaceProfile::default();
    let mut odd = FaceProfile::default();
    odd.eyes.right_iris = Some(IrisColours { mid: Some(ColourRef::rgb(0xc8, 0x26, 0x30)), ..IrisColours::default() });
    let (a, b) = (picture(&plain, &[], 387), picture(&odd, &[], 387));
    let (picture_left, picture_right) = changed_by_half(&a, &b);
    // The character's right eye is on the picture's left.
    assert!(picture_left > 50 && picture_right == 0, "{picture_left} {picture_right}");
    let reds = b.pixels().filter(|p| p.0[0] > 150 && p.0[1] < 90 && p.0[2] < 100).count();
    assert!(reds > 30, "the right iris is red: {reds}");
}

/// Deep gloom darkens the upper face only above 0.6, and its pupils stay bright: the brightest pixels of each eye under
/// full gloom are far brighter than the band round them.
#[test]
fn deep_gloom_darkens_the_upper_face_and_the_pupils_glow() {
    let profile = FaceProfile::default();
    let rest = picture(&profile, &[], 387);
    let mild = picture(&profile, &[(Channel::EYES_GLOOM, 0.5)], 387);
    let deep = picture(&profile, &[(Channel::EYES_GLOOM, 1.0)], 387);
    let luma = |p: &image::Rgba<u8>| (u32::from(p.0[0]) * 3 + u32::from(p.0[1]) * 6 + u32::from(p.0[2])) / 10;
    let mean = |img: &RgbaImage, rows: std::ops::Range<u32>| {
        let (mut sum, mut n) = (0_u64, 0_u64);
        for y in rows {
            for x in 0..img.width() {
                sum += u64::from(luma(img.get_pixel(x, y)));
                n += 1;
            }
        }
        sum / n.max(1)
    };
    let h = rest.height();
    let forehead = 0..h / 6;
    assert!(mean(&mild, forehead.clone()) + 25 > mean(&rest, forehead.clone()), "0.5 keeps the face lit");
    assert!(mean(&deep, forehead.clone()) + 60 < mean(&rest, forehead), "1.0 darkens the forehead");
    // The brightest pixel in the eye rows is the glow, far above the band's dark.
    let eye_rows = (h * 4 / 10)..(h * 3 / 4);
    let brightest = eye_rows
        .clone()
        .flat_map(|y| (0..deep.width()).map(move |x| (x, y)))
        .map(|(x, y)| luma(deep.get_pixel(x, y)))
        .max()
        .unwrap();
    assert!(brightest > 200, "the pupils glow: {brightest}");
}

/// The contact sheet for the pull request: each mark and the expressions that use them, on the kit's default face.
/// One picture of the sheet: its label, the face, and the channels pushed.
type Mark<'a> = (&'a str, &'a FaceProfile, Vec<(Channel, f32)>);

#[test]
#[allow(clippy::disallowed_methods)] // writes the evidence pictures into the folder CI names; no op runs here
fn render_the_reference_marks_when_asked() {
    let Ok(dir) = std::env::var("OMUZAKI_FACE_EVIDENCE_DIR") else { return };
    let default = FaceProfile::default();
    let hetero = heterochromia();
    let mut one_left = FaceProfile::default();
    one_left.mouth.fang.kind = FangKind::OneLeft;
    let open = (Channel::JAW_OPEN, 0.55);
    let cells: Vec<Mark<'_>> = vec![
        ("rest", &default, vec![]),
        ("angerVein 0.5 (pop)", &default, vec![(Channel::ANGER_VEIN, 0.5)]),
        ("angerVein 1", &default, vec![(Channel::ANGER_VEIN, 1.0)]),
        (
            "eyesBlank",
            &default,
            vec![(Channel::EYES_BLANK, 1.0), (Channel::EYE_WIDE_LEFT, 0.6), (Channel::EYE_WIDE_RIGHT, 0.6)],
        ),
        ("eyesDot", &default, vec![(Channel::EYES_DOT, 1.0)]),
        ("eyesStare", &default, vec![(Channel::EYES_STARE, 1.0)]),
        ("eyesPill", &default, vec![(Channel::EYES_PILL, 1.0)]),
        ("eyesPill, two colours", &hetero, vec![(Channel::EYES_PILL, 1.0)]),
        ("right_iris (heterochromia)", &hetero, vec![]),
        ("teethSharp", &default, vec![open, (Channel::TEETH_SHARP, 1.0)]),
        ("fang one_left", &one_left, vec![(Channel::JAW_OPEN, 0.4), (Channel::FANGS, 1.0)]),
        ("eyesGloom 0.5", &default, vec![(Channel::EYES_GLOOM, 0.5)]),
        ("eyesGloom 1 (deep)", &hetero, vec![(Channel::EYES_GLOOM, 1.0)]),
        (
            "rage: squeeze, sharp, vein",
            &default,
            vec![
                (Channel::EYES_SQUEEZE, 1.0),
                (Channel::TEETH_SHARP, 1.0),
                (Channel::ANGER_VEIN, 1.0),
                (Channel::JAW_OPEN, 0.7),
                (Channel::BLUSH, 0.6),
                (Channel::BROW_DOWN_LEFT, 1.0),
                (Channel::BROW_DOWN_RIGHT, 1.0),
            ],
        ),
        (
            "menace: deep gloom grin",
            &hetero,
            vec![
                (Channel::EYES_GLOOM, 1.0),
                (Channel::EYES_STARE, 0.6),
                (Channel::MOUTH_SMILE_LEFT, 0.9),
                (Channel::MOUTH_SMILE_RIGHT, 0.9),
                (Channel::JAW_OPEN, 0.35),
                (Channel::FANGS, 1.0),
            ],
        ),
    ];
    let mut sheet = Vec::new();
    for (label, profile, pairs) in &cells {
        sheet.push(Cell { picture: picture(profile, pairs, 300), label: (*label).to_owned(), flagged: false });
    }
    let path = std::path::Path::new(&dir);
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("reference-marks.png"), png(&compose(&sheet, 5)).unwrap()).unwrap();
    // The mouths close up: a crop round the mouth line of a large picture, where the teeth and fangs can be judged.
    let rect = HeadParams::default().face_projection().rect;
    let width = 1600_u32;
    let to_px = |x: f32, y: f32, h: u32| {
        (
            ((x - rect.x0) / (rect.x1 - rect.x0) * width as f32) as u32,
            ((rect.y1 - y) / (rect.y1 - rect.y0) * h as f32) as u32,
        )
    };
    let mouths: Vec<Cell> = cells
        .iter()
        .filter(|(_, _, pairs)| pairs.iter().any(|(c, _)| *c == Channel::JAW_OPEN))
        .map(|(label, profile, pairs)| {
            let full = picture(profile, pairs, width);
            let mouth_y = omuzaki_face_api::narrow(profile.layout.mouth_y);
            let (x0, y0) = to_px(-0.04, mouth_y + 0.02, full.height());
            let (x1, y1) = to_px(0.04, mouth_y - 0.03, full.height());
            let crop = image::imageops::crop_imm(&full, x0, y0, x1 - x0, y1 - y0).to_image();
            Cell { picture: crop, label: (*label).to_owned(), flagged: false }
        })
        .collect();
    std::fs::write(path.join("reference-mouths.png"), png(&compose(&mouths, 4)).unwrap()).unwrap();
    // The same faces at the readability sizes, so the marks can be judged where a stream shows them.
    for size in [112_u32, 56, 28] {
        let small: Vec<Cell> = cells
            .iter()
            .map(|(label, profile, pairs)| Cell {
                picture: picture(profile, pairs, size),
                label: label.split(' ').next().unwrap_or("").to_owned(),
                flagged: false,
            })
            .collect();
        std::fs::write(path.join(format!("reference-marks-{size}px.png")), png(&compose(&small, 15)).unwrap()).unwrap();
    }
}
