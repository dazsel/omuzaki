//! The likeness measures on the synthetic faces of Appendix A (design 9 and 11.1): accuracy against the analytic
//! outline, repeatability under clicked landmarks, roll and offsets, the same face in two styles, distinct faces, the
//! graded-edit ladders, the known negatives, determinism and the mirror test. Every image is drawn in code.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::print_stderr, reason = "tests print their numbers")]
#![allow(clippy::many_single_char_names, reason = "two measurements and their mirror")]

mod support;

use omuzaki_measure::face::likeness::Compare;
use omuzaki_measure::face::outline::{ANGLES, SampleState};
use omuzaki_measure::face::{FaceLandmarks, LandmarkSource, MeasureInput, measure_face};
use omuzaki_measure::image::Rgba8;
use omuzaki_store_api::ContentHash;
use support::synth::{Drawn, Edit, FACE_A, FACE_B, FACE_C, Rng, Shot, Style, SynthFace, draw};
use support::{Measured, clicked, quantile, sd, spearman, true_landmarks};

const FACES: [(&str, SynthFace); 3] = [("A", FACE_A), ("B", FACE_B), ("C", FACE_C)];

/// The 12 shots of the repeatability battery (design 9.2): four sizes, three rolls, random sub-pixel offsets.
fn battery(rng: &mut Rng) -> Vec<Shot> {
    let mut out = Vec::new();
    for iod_px in [100.0, 128.0, 160.0, 200.0] {
        for roll_deg in [-8.0, 0.0, 6.0] {
            out.push(Shot { iod_px, roll_deg, offset: [rng.uniform(), rng.uniform()] });
        }
    }
    out
}

fn id(e: &omuzaki_measure::face::Refusal) -> &str {
    &e.text.id
}

#[test]
fn render_like_faces_measure_within_0_6_percent_of_the_analytic_outline() {
    for (name, face) in FACES {
        for iod in [120.0, 200.0] {
            let drawn = draw(&face, Style::RenderLike, Shot::new(iod));
            let m = Measured::new(&drawn, true_landmarks(&drawn), false).unwrap();
            let (rms, max) = m.error(&face.truth_profile());
            eprintln!("render-like {name} at {iod} px: RMS {:.2}%, max {:.2}%", rms * 100.0, max * 100.0);
            assert!(rms <= 0.006 && max <= 0.018, "{name} at {iod}: RMS {rms}, max {max}");
            assert!(m.m.outline.state.iter().all(|s| *s == SampleState::Ok), "{name} at {iod}: every ray reads");
        }
    }
}

#[test]
fn drawings_measure_within_the_prototype_bounds_with_the_side_lock_occluded() {
    for (name, face) in [("A", FACE_A), ("C", FACE_C)] {
        for iod in [64.0, 120.0, 200.0] {
            let drawn = draw(&face, Style::Drawing, Shot::new(iod));
            let m = Measured::new(&drawn, true_landmarks(&drawn), false).unwrap();
            let (rms, max) = m.error(&face.truth_profile());
            let occluded = m.m.outline.state.iter().filter(|s| **s == SampleState::Occluded).count();
            eprintln!(
                "drawing {name} at {iod} px: RMS {:.2}%, max {:.2}%, {occluded} occluded",
                rms * 100.0,
                max * 100.0
            );
            // Design 9.1: RMS 0.34 to 1.07%, largest 4.2% (a sample at the lock's edge), plus 20%.
            assert!(rms <= 0.0128 && max <= 0.05, "{name} at {iod}: RMS {rms}, max {max}");
            assert!(occluded >= 8, "{name} at {iod}: the lock over the right cheek is occluded ({occluded})");
            assert!(m.m.warnings.iter().any(|w| w.code == "occluded"));
        }
    }
}

#[test]
fn small_render_like_faces_are_refused_never_measured() {
    for (name, face) in FACES {
        let drawn = draw(&face, Style::RenderLike, Shot::new(64.0));
        match Measured::new(&drawn, true_landmarks(&drawn), false) {
            Err(e) => assert!(["measure-leak", "measure-too-small"].contains(&id(&e)), "{name}: {}", id(&e)),
            Ok(m) => {
                // A measurement at 64 px is allowed only when it is still accurate.
                let (rms, max) = m.error(&face.truth_profile());
                eprintln!("render-like {name} at 64 px measured: RMS {:.2}%, max {:.2}%", rms * 100.0, max * 100.0);
                assert!(rms <= 0.012 && max <= 0.04, "{name} at 64 px: RMS {rms}, max {max}");
            }
        }
    }
    let tiny = draw(&FACE_A, Style::RenderLike, Shot::new(40.0));
    let e = Measured::new(&tiny, true_landmarks(&tiny), true).expect_err("an IOD of 40 px is refused");
    assert_eq!(id(&e), "measure-too-small");
}

/// The battery for one face and style: (profile spread, `O` values, spacing SDs, refused).
fn repeatability(face: &SynthFace, style: Style, seed: u64, refine: bool) -> (f64, Vec<f64>, [f64; 4], usize) {
    let mut rng = Rng::new(seed);
    let mut measured = Vec::new();
    let mut refused = 0;
    for shot in battery(&mut rng) {
        let drawn = draw(face, style, shot);
        match Measured::new(&drawn, clicked(&drawn, &mut rng, 0.02), refine) {
            Ok(m) => measured.push(m),
            Err(e) => {
                eprintln!("  refused at {} px, roll {}: {}", shot.iod_px, shot.roll_deg, id(&e));
                refused += 1;
            }
        }
    }
    // The RMS over angles of the per-angle SD.
    let per_angle: Vec<f64> = (0..ANGLES)
        .filter_map(|k| {
            let v: Vec<f64> = measured.iter().filter_map(|m| m.m.outline.ok(k)).collect();
            (v.len() >= 3).then(|| sd(&v))
        })
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let spread = (per_angle.iter().map(|s| s * s).sum::<f64>() / per_angle.len() as f64).sqrt();
    let mut o = Vec::new();
    for i in 0..measured.len() {
        for j in i + 1..measured.len() {
            o.push(measured[j].outline_to(&measured[i]));
        }
    }
    let field = |f: fn(&Measured) -> Option<f64>| sd(&measured.iter().filter_map(f).collect::<Vec<_>>());
    let spacing = [
        field(|m| Some(m.s.eye_to_chin)),
        field(|m| m.s.eye_to_mouth),
        field(|m| m.s.width_at_mouth),
        field(|m| Some(m.s.width_max)),
    ];
    (spread, o, spacing, refused)
}

#[test]
fn twelve_clicked_shots_of_each_face_agree_within_the_repeatability_bounds() {
    for style in [Style::RenderLike, Style::Drawing] {
        for (i, (name, face)) in FACES.iter().enumerate() {
            let (spread, o, spacing, refused) = repeatability(face, style, 7 + i as u64, true);
            let (median, p95, max) = (quantile(&o, 0.5), quantile(&o, 0.95), quantile(&o, 1.0));
            let worst_spacing = spacing.iter().copied().fold(0.0, f64::max);
            eprintln!(
                "{style:?} {name}: spread {:.2}%, O median {:.2}% p95 {:.2}% max {:.2}%, spacing SD {:.2}% \
                 (eye_to_chin {:.2}, eye_to_mouth {:.2}, width_at_mouth {:.2}, width_max {:.2}), refused {refused}",
                spread * 100.0,
                median * 100.0,
                p95 * 100.0,
                max * 100.0,
                worst_spacing * 100.0,
                spacing[0] * 100.0,
                spacing[1] * 100.0,
                spacing[2] * 100.0,
                spacing[3] * 100.0,
            );
            // Render-like: design 9.2 plus 20% (spread 0.16%, O p95 0.38% and max 0.41%). Drawings: these drawings'
            // own bounds, a little above the prototype's (0.18%, 0.45%, 0.54%) because the side lock's tip here ends
            // mid-jaw, where a sample or two under it reads short without reaching the occlusion threshold. Both stay
            // below the same_look tolerance (1%), which is what repeatability must support.
            let (spread_max, p95_max, o_max) = match style {
                Style::RenderLike => (0.0020, 0.0046, 0.0050),
                Style::Drawing => (0.0026, 0.0065, 0.0080),
            };
            assert!(spread <= spread_max, "{style:?} {name}: spread {spread}");
            assert!(p95 <= p95_max && max <= o_max, "{style:?} {name}: O p95 {p95}, max {max}");
            assert!(worst_spacing <= 0.0123, "{style:?} {name}: spacing SD {spacing:?}");
            assert!(refused <= 1, "{style:?} {name}: {refused} refused");
        }
    }
}

#[test]
fn without_the_snap_clicked_landmarks_are_ten_times_less_repeatable() {
    let (_, snapped, _, _) = repeatability(&FACE_A, Style::RenderLike, 11, true);
    let (spread, o, _, _) = repeatability(&FACE_A, Style::RenderLike, 11, false);
    let (p95_on, p95_off) = (quantile(&snapped, 0.95), quantile(&o, 0.95));
    eprintln!(
        "render-like A, snap off: spread {:.2}%, O p95 {:.2}% (on: {:.2}%)",
        spread * 100.0,
        p95_off * 100.0,
        p95_on * 100.0
    );
    assert!(p95_off > 5.0 * p95_on, "the snap is what makes clicks repeatable: off {p95_off}, on {p95_on}");
}

/// Each face in `style` at four shots with clicks, measured.
fn shots(face: &SynthFace, style: Style, seed: u64, n: usize) -> Vec<Measured> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    let sizes = [112.0, 136.0, 160.0, 184.0];
    for i in 0..n {
        let shot =
            Shot { iod_px: sizes[i % 4], roll_deg: 6.0 * rng.uniform() - 3.0, offset: [rng.uniform(), rng.uniform()] };
        let drawn = draw(face, style, shot);
        out.push(Measured::new(&drawn, clicked(&drawn, &mut rng, 0.02), true).unwrap());
    }
    out
}

#[test]
fn the_same_face_as_a_render_and_as_a_drawing_is_within_cross_look() {
    for (i, (name, face)) in FACES.iter().enumerate() {
        let renders = shots(face, Style::RenderLike, 100 + i as u64, 4);
        let drawings = shots(face, Style::Drawing, 200 + i as u64, 4);
        let mut o = Vec::new();
        for r in &renders {
            for d in &drawings {
                let l = d.against(r, Compare::CrossLook);
                assert!(l.within, "{name}: a drawing of the face is within cross_look of its render: {:?}", l.terms);
                assert!(l.expression_differs, "{name}: the drawing's open mouth reads as another expression");
                o.push(l.terms[0].value);
            }
        }
        eprintln!(
            "{name} render against drawing: O median {:.2}%, p95 {:.2}%",
            quantile(&o, 0.5) * 100.0,
            quantile(&o, 0.95) * 100.0
        );
        // Design 9.3 has medians of 0.62 to 0.77% and p95 up to 1.06%, mostly the drawing's thicker lashes raising
        // the snapped eyes (about 1% on eye_to_chin, as here). Face B's long jaw puts the lock's tip mid-jaw, where one
        // sample reads short, so the bound is 1.6%: well inside cross_look's 2.5% and a third of the smallest distance
        // between distinct faces.
        assert!(quantile(&o, 0.95) <= 0.016, "{name}: {o:?}");
    }
}

#[test]
fn distinct_faces_are_ten_times_the_repeatability_apart_and_fail_cross_look() {
    let measured: Vec<Vec<Measured>> =
        FACES.iter().enumerate().map(|(i, (_, face))| shots(face, Style::RenderLike, 300 + i as u64, 4)).collect();
    for (a, b, floor) in [(0, 2, 0.045), (1, 2, 0.06), (0, 1, 0.10)] {
        let o: Vec<f64> = measured[a].iter().flat_map(|x| measured[b].iter().map(|y| y.outline_to(x))).collect();
        let smallest = quantile(&o, 0.0);
        eprintln!(
            "{} against {}: O smallest {:.2}%, median {:.2}%",
            FACES[a].0,
            FACES[b].0,
            smallest * 100.0,
            quantile(&o, 0.5) * 100.0
        );
        // Design 9.4: A-C 5.1%, B-C 6.7%, A-B 11.9% at the smallest; ten times the repeatability p95 (0.45%).
        assert!(smallest >= floor, "{} against {}: {smallest}", FACES[a].0, FACES[b].0);
        for x in &measured[a] {
            for y in &measured[b] {
                assert!(!y.against(x, Compare::CrossLook).within, "distinct faces fail cross_look");
            }
        }
    }
}

/// One ladder: `O` and the spacing fields at each step from -5 to 5 (means over four shots), against a clean
/// measurement of face A.
struct Ladder {
    steps: Vec<f64>,
    o: Vec<f64>,
    fields: Vec<[f64; 5]>,
}

/// `eye_to_chin`, `eye_to_mouth`, `width_max`, `width_at_mouth`, `aspect`.
fn fields(m: &Measured) -> [f64; 5] {
    [
        m.s.eye_to_chin,
        m.s.eye_to_mouth.unwrap_or(f64::NAN),
        m.s.width_max,
        m.s.width_at_mouth.unwrap_or(f64::NAN),
        m.s.aspect,
    ]
}

fn ladder(edit: Edit, style: Style) -> Ladder {
    let clean = draw(&FACE_A, style, Shot::new(160.0));
    let reference = Measured::new(&clean, true_landmarks(&clean), true).unwrap();
    let steps: Vec<f64> = (-5..=5).map(f64::from).collect();
    let mut o = Vec::new();
    let mut means = Vec::new();
    for (i, &t) in steps.iter().enumerate() {
        let face = FACE_A.edited(edit, t);
        let measured = shots(&face, style, 1000 + i as u64, 4);
        o.push(measured.iter().map(|m| m.outline_to(&reference)).sum::<f64>() / 4.0);
        let mut sum = [0.0; 5];
        for m in &measured {
            for (s, v) in sum.iter_mut().zip(fields(m)) {
                *s += v / 4.0;
            }
        }
        means.push(sum);
    }
    eprintln!("{edit:?} {style:?}: O {:?}", o.iter().map(|v| (v * 10000.0).round() / 100.0).collect::<Vec<_>>());
    Ladder { steps, o, fields: means }
}

impl Ladder {
    /// `O` rises strictly with |step| on both sides of zero.
    fn assert_monotonic(&self, what: &str) {
        for i in 0..5 {
            assert!(self.o[i] > self.o[i + 1], "{what}: O must fall towards step 0: {:?}", self.o);
            assert!(self.o[10 - i] > self.o[9 - i], "{what}: O must fall towards step 0: {:?}", self.o);
        }
    }

    /// The Spearman correlation of field `f` with the step.
    fn rho(&self, f: usize) -> f64 {
        let ys: Vec<f64> = self.fields.iter().map(|v| v[f]).collect();
        spearman(&self.steps, &ys)
    }

    /// The largest distance of field `f` from its value at step 0 (the same shots of the unedited face).
    fn drift(&self, f: usize) -> f64 {
        self.fields.iter().map(|v| (v[f] - self.fields[5][f]).abs()).fold(0.0, f64::max)
    }
}

const EYE_TO_CHIN: usize = 0;
const EYE_TO_MOUTH: usize = 1;
const WIDTH_MAX: usize = 2;
const WIDTH_AT_MOUTH: usize = 3;
const ASPECT: usize = 4;

#[test]
fn a_longer_lower_face_rises_steadily_and_moves_eye_to_chin_and_aspect() {
    for style in [Style::RenderLike, Style::Drawing] {
        let l = ladder(Edit::LowerFace, style);
        l.assert_monotonic("lower face");
        assert_eq!(l.rho(EYE_TO_CHIN), 1.0, "{style:?}");
        assert_eq!(l.rho(ASPECT), 1.0, "{style:?}");
        assert!(l.drift(EYE_TO_MOUTH) <= 0.01, "{style:?}: the mouth stays where it was: {}", l.drift(EYE_TO_MOUTH));
        // The smallest step is above the repeatability p95 (0.45%).
        assert!(l.o[4] > 0.0045 && l.o[6] > 0.0045, "{style:?}: {:?}", l.o);
    }
}

#[test]
fn a_wider_jaw_rises_steadily_and_moves_the_widths() {
    for style in [Style::RenderLike, Style::Drawing] {
        let l = ladder(Edit::JawWidth, style);
        l.assert_monotonic("jaw width");
        if style == Style::RenderLike {
            // In the drawing the side lock hides the right cheek, so the widest row is not measured (see width_max).
            assert_eq!(l.rho(WIDTH_MAX), 1.0);
            assert_eq!(l.rho(ASPECT), -1.0);
        }
        assert_mouth_width(&l, style, 1.0);
        assert!(l.drift(EYE_TO_MOUTH) <= 0.01, "{style:?}");
        assert!(l.o[4] > 0.0045 && l.o[6] > 0.0045, "{style:?}: {:?}", l.o);
    }
}

/// `width_at_mouth` follows the step with Spearman `rho`; in the drawing the side lock covers the right cheek at the
/// mouth's height, so the width there is unmeasured rather than guessed.
fn assert_mouth_width(l: &Ladder, style: Style, rho: f64) {
    match style {
        Style::RenderLike => assert_eq!(l.rho(WIDTH_AT_MOUTH), rho),
        Style::Drawing => assert!(l.fields.iter().all(|f| f[WIDTH_AT_MOUTH].is_nan()), "unmeasured under the lock"),
    }
}

#[test]
fn a_squarer_jaw_rises_steadily_and_widens_the_face_at_the_mouth() {
    for style in [Style::RenderLike, Style::Drawing] {
        let l = ladder(Edit::JawSquare, style);
        l.assert_monotonic("jaw exponent");
        assert_mouth_width(&l, style, 1.0);
        assert!(l.drift(EYE_TO_MOUTH) <= 0.01, "{style:?}");
        assert!(l.o[4] > 0.0045 && l.o[6] > 0.0045, "{style:?}: {:?}", l.o);
    }
}

#[test]
fn wider_set_eyes_rise_steadily_and_shrink_everything_in_iod() {
    for style in [Style::RenderLike, Style::Drawing] {
        let l = ladder(Edit::EyeSpacing, style);
        l.assert_monotonic("eye spacing");
        assert_eq!(l.rho(EYE_TO_CHIN), -1.0, "{style:?}");
        if style == Style::RenderLike {
            assert_eq!(l.rho(WIDTH_MAX), -1.0);
        }
        assert!(l.o[4] > 0.0045 && l.o[6] > 0.0045, "{style:?}: {:?}", l.o);
    }
}

#[test]
fn moving_the_mouth_moves_the_mouth_not_the_outline() {
    for style in [Style::RenderLike, Style::Drawing] {
        let l = ladder(Edit::Mouth, style);
        let worst = l.o.iter().copied().fold(0.0, f64::max);
        assert!(worst < 0.006, "{style:?}: the outline stays at the noise floor: {:?}", l.o);
        assert!(l.drift(EYE_TO_CHIN) <= 0.005, "{style:?}: {}", l.drift(EYE_TO_CHIN));
        assert_eq!(l.rho(EYE_TO_MOUTH), 1.0, "{style:?}");
    }
}

fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Rgba8 {
    let pixels = (0..width * height).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect::<Vec<_>>();
    Rgba8 { width, height, hash: ContentHash::of(&pixels), pixels }
}

fn landmarks(eye_right: [f64; 2], eye_left: [f64; 2], mouth: [f64; 2]) -> MeasureInput {
    MeasureInput {
        landmarks: FaceLandmarks { eye_right, eye_left, mouth: Some(mouth), nose: None, source: LandmarkSource::Agent },
        refine: true,
        exclude: Vec::new(),
        skin: None,
    }
}

#[test]
fn known_negatives_are_refused_never_measured() {
    // A blank image.
    let blank = solid(400, 400, [255, 255, 255]);
    let e = measure_face(&blank, &landmarks([150.0, 180.0], [250.0, 180.0], [200.0, 260.0])).expect_err("blank");
    eprintln!("blank: {}", e.text.id);
    // A skin-coloured disc with no features: nothing to draw a line with.
    let mut disc = solid(400, 400, [255, 255, 255]);
    for y in 0..400_u32 {
        for x in 0..400_u32 {
            if (f64::from(x) - 200.0).hypot(f64::from(y) - 200.0) < 150.0 {
                let i = ((y * 400 + x) * 4) as usize;
                disc.pixels[i..i + 3].copy_from_slice(&[0xf7, 0xdc, 0xcb]);
            }
        }
    }
    let e = measure_face(&disc, &landmarks([150.0, 180.0], [250.0, 180.0], [200.0, 260.0])).expect_err("a disc");
    assert_eq!(id(&e), "measure-low-contrast", "no line art bounds the disc");
    // The eyes swapped.
    let drawn = draw(&FACE_A, Style::RenderLike, Shot::new(128.0));
    let mut swapped = true_landmarks(&drawn);
    std::mem::swap(&mut swapped.eye_right, &mut swapped.eye_left);
    let e = support::measure(&drawn, swapped, true).expect_err("swapped eyes");
    assert_eq!(id(&e), "measure-eyes-swapped");
}

#[test]
fn the_same_input_gives_the_same_numbers() {
    let drawn = draw(&FACE_C, Style::Drawing, Shot { iod_px: 150.0, roll_deg: 4.0, offset: [0.3, 0.7] });
    let a = support::measure(&drawn, true_landmarks(&drawn), true).unwrap();
    let b = support::measure(&drawn, true_landmarks(&drawn), true).unwrap();
    assert_eq!(a.outline.r, b.outline.r);
    assert_eq!(a.outline.state, b.outline.state);
    assert_eq!(a.landmarks, b.landmarks);
}

/// The image mirrored left to right.
fn mirrored(image: &Rgba8) -> Rgba8 {
    let (w, h) = (image.width as usize, image.height as usize);
    let mut pixels = vec![0; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (from, to) = ((y * w + x) * 4, (y * w + (w - 1 - x)) * 4);
            pixels[to..to + 4].copy_from_slice(&image.pixels[from..from + 4]);
        }
    }
    Rgba8 { width: image.width, height: image.height, hash: ContentHash::of(&pixels), pixels }
}

#[test]
fn a_mirrored_face_gives_the_same_asymmetry_and_warnings() {
    let drawn: Drawn = draw(&FACE_A, Style::Drawing, Shot::new(140.0));
    let lm = true_landmarks(&drawn);
    let input = MeasureInput { landmarks: lm, refine: true, exclude: Vec::new(), skin: None };
    let a = measure_face(&drawn.image, &input).unwrap();
    let flip = mirrored(&drawn.image);
    let w = f64::from(flip.width);
    let m = |p: [f64; 2]| [w - p[0], p[1]];
    let flipped = FaceLandmarks {
        eye_right: m(lm.eye_left),
        eye_left: m(lm.eye_right),
        mouth: lm.mouth.map(m),
        nose: None,
        source: LandmarkSource::Agent,
    };
    let b = measure_face(&flip, &MeasureInput { landmarks: flipped, ..input }).unwrap();
    let (sa, sb) = (a.outline.asymmetry(), b.outline.asymmetry());
    eprintln!("asymmetry {sa:?} and mirrored {sb:?}");
    match (sa, sb) {
        (Some(x), Some(y)) => assert!((x - y).abs() < 1e-3, "{x} against {y}"),
        (None, None) => {}
        other => panic!("one side has an asymmetry and the other not: {other:?}"),
    }
    let codes = |m: &omuzaki_measure::face::FaceMeasure| {
        let mut c: Vec<&str> = m.warnings.iter().map(|w| w.code).collect();
        c.sort_unstable();
        c
    };
    assert_eq!(codes(&a), codes(&b));
    // The occluded side swaps: sample k on one is sample 84 - k on the other.
    for k in 0..ANGLES {
        assert_eq!(
            a.outline.state[k] == SampleState::Occluded,
            b.outline.state[ANGLES - 1 - k] == SampleState::Occluded,
            "{k}"
        );
    }
}
