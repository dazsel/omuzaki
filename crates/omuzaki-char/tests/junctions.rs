//! The silhouette of every template body is C1 across the hip: no step and no kink where the thigh leaves the
//! pelvis, for every template at every body-shape preset and at the corners of the shape cube. The shoulder is measured
//! too, but only reported: the body mesher owns it (`body/junction.rs` grows the arm a root and fixes its
//! epaulette there).
//!
//! **What is measured.** The front outline is the outer edge of the regions' lofts. Where two regions share it, each
//! contributes its *edge curve*: the line through the outer tip of its sections (the torso's side, the thigh's outside,
//! the top of the arm). The outline is C1 across a junction if those curves meet with the same value and the same
//! direction, so that is read off the sections themselves (a raster would add the noise of its pixels):
//! - **hip**: where the torso ends at the crotch, how far the thigh's edge is from the torso's (the *step*) and the angle
//!   between their tangents (the *break*); and how far the thigh reaches outside the torso above the crotch;
//! - **shoulder** (reported, not asserted): how far the torso's shoulder line stands above the arm's top (the torso's corner showing through a
//!   thin arm), at every place where the arm's top crosses the shoulder line the angle between their tangents, and,
//!   read off a picture of the front silhouette, how far the outline climbs again after it has started down from the
//!   neck (the notch and the epaulette; an arm that starts beyond the torso's side, as the first templates' did, is
//!   invisible to the section curves but not to this).
//!
//! Steps are fractions of the torso's half-width, so one tolerance serves a chibi and an adult.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // pixel indices
#![allow(clippy::similar_names, clippy::many_single_char_names)] // x/y/z, a/b/c of a fit
#![allow(clippy::disallowed_methods, reason = "test evidence, written to folders the developer names; not an op")]

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use omuzaki_char::template::layout::{BodyLayout, section_basis};
use omuzaki_char::template::registry;
use omuzaki_char::template::shape;
use omuzaki_char_api::{BodyRegion, BodyShape, HandParams, Proportions};
use omuzaki_core::glam::DVec2;

#[path = "common/silhouette.rs"]
mod silhouette;
use silhouette::{REGIONS, View, silhouette_of};

/// The most a junction may step, as a fraction of the torso's half-width.
const MAX_STEP: f64 = 0.01;
/// The most the outline may turn across a junction, in degrees.
const MAX_BREAK_DEG: f64 = 8.0;
/// The size of a pixel of the pictures the outline is read from, in the pictures' units (see [`unit_of`]).
const PIXEL: f64 = 0.000_25;
/// Sections an edge curve is read through.
const SAMPLES: usize = 400;
/// The distance the tangents are read over, as a fraction of the torso's half-width.
const REACH: f64 = 0.05;
/// How far apart, as a fraction of the torso's half-width, two curves must stand on both sides of a crossing for it to
/// be one (curves that lie on each other do not cross).
const SEPARATION: f64 = 0.004;

/// What a junction measurement is of.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Joint {
    /// Where the torso ends at the crotch and the thigh carries the outline on.
    Crotch,
    /// The thigh reaching outside the torso above the crotch.
    ThighOut,
    /// The torso's corner showing above the arm's top.
    ShoulderCorner,
    /// A place where the arm's top crosses the shoulder line.
    ShoulderCrossing,
    /// The outline of the shoulder rising again after it has started down from the neck (an epaulette).
    ShoulderRise,
}

/// One measured junction.
#[derive(Copy, Clone, Debug)]
struct Junction {
    joint: Joint,
    /// Gap between the two edges, as a fraction of the torso's half-width (zero or more for those that only count one way).
    step: f64,
    /// Angle between the two edges' tangents, in degrees.
    slope_break_deg: f64,
}

impl Joint {
    /// Whether the junction is at the shoulder, which is reported and not asserted (see the module's docs).
    fn is_shoulder(self) -> bool {
        matches!(self, Self::ShoulderCorner | Self::ShoulderCrossing | Self::ShoulderRise)
    }
}

impl Junction {
    /// Whether the junction is within tolerance.
    fn is_c1(&self) -> bool {
        self.step.abs() <= MAX_STEP && self.slope_break_deg.abs() <= MAX_BREAK_DEG
    }
}

// ------------------------------------------------------------------------------------------------ edge curves

/// The outer tip of `region`'s sections, in the front view, from the first section to the last.
fn edge(l: &BodyLayout, region: BodyRegion) -> Vec<DVec2> {
    (0..=SAMPLES)
        .map(|k| {
            let s = l.section64(region, k as f64 / SAMPLES as f64).unwrap();
            let (across, _) = section_basis(region, s.axis.as_vec3());
            let tip = s.centre + across.as_dvec3() * s.radii.x;
            DVec2::new(tip.x, tip.y)
        })
        .collect()
}

/// A curve through points whose `key` (`x` or `y`, by `by`) runs one way: the other coordinate at `at`, linearly
/// between points, held at the ends.
fn value_at(points: &[DVec2], at: f64, key: fn(DVec2) -> f64, other: fn(DVec2) -> f64) -> f64 {
    let rising = key(points[points.len() - 1]) >= key(points[0]);
    let i = points
        .windows(2)
        .position(|w| if rising { key(w[1]) >= at } else { key(w[1]) <= at })
        .unwrap_or(points.len() - 2);
    let (a, b) = (points[i], points[i + 1]);
    let span = key(b) - key(a);
    let t = if span.abs() < 1e-15 { 0.0 } else { ((at - key(a)) / span).clamp(0.0, 1.0) };
    other(a) + (other(b) - other(a)) * t
}

fn x_of(p: DVec2) -> f64 {
    p.x
}

fn y_of(p: DVec2) -> f64 {
    p.y
}

/// Degrees between the directions with slopes `a` and `b`.
fn angle_between(a: f64, b: f64) -> f64 {
    (a.atan() - b.atan()).to_degrees()
}

// ------------------------------------------------------------------------------------------------ the junctions

/// The torso's greatest half-width: the unit steps are measured in.
fn half_width(l: &BodyLayout) -> f64 {
    (0..=20).map(|k| l.section64(BodyRegion::Torso, f64::from(k) / 20.0).unwrap().radii.x).fold(0.0, f64::max)
}

/// The crotch and the thigh against the torso's side, seen as `x` over `y`.
fn hip(l: &BodyLayout, r: f64) -> Vec<Junction> {
    let (torso, leg) = (edge(l, BodyRegion::Torso), edge(l, BodyRegion::LegL));
    let xt = |y: f64| value_at(&torso, y, y_of, x_of);
    let xl = |y: f64| value_at(&leg, y, y_of, x_of);
    let (bottom, length) = (torso[0].y, torso[SAMPLES].y - torso[0].y);
    let h = REACH * r;
    // The torso's side meets the thigh's where the torso ends: the slope each reaches the bottom with, read one-sided
    // (second order, so that curvature does not read as a break).
    let slope_torso = (-3.0 * xt(bottom) + 4.0 * xt(bottom + h) - xt(bottom + 2.0 * h)) / (2.0 * h);
    let slope_leg = (3.0 * xl(bottom) - 4.0 * xl(bottom - h) + xl(bottom - 2.0 * h)) / (2.0 * h);
    let crotch = Junction {
        joint: Joint::Crotch,
        step: (xt(bottom) - xl(bottom)) / r,
        slope_break_deg: angle_between(slope_torso, slope_leg),
    };
    // Only where the leg is: its top is where its edge curve ends.
    let reach = (0.3 * length).min(leg[0].y - bottom);
    let poke = (0..=SAMPLES / 2)
        .map(|k| bottom + reach * k as f64 / (SAMPLES / 2) as f64)
        .map(|y| xl(y) - xt(y))
        .fold(0.0, f64::max);
    vec![crotch, Junction { joint: Joint::ThighOut, step: poke / r, slope_break_deg: 0.0 }]
}

/// The arm's top against the torso's shoulder line, seen as `y` over `x`.
fn shoulder(l: &BodyLayout, r: f64) -> Vec<Junction> {
    let torso = edge(l, BodyRegion::Torso);
    // The shoulder line: from the middle of the torso's top, out along its top and down its side to where it is widest.
    let mut line = vec![DVec2::new(0.0, torso[SAMPLES].y)];
    for p in torso.iter().rev() {
        if line.last().is_some_and(|last| p.x < last.x - 1e-12) {
            break;
        }
        line.push(*p);
    }
    let wall = line[line.len() - 1].x;
    let yt = |x: f64| value_at(&line, x, x_of, y_of);
    let arm: Vec<DVec2> = edge(l, BodyRegion::ArmL).into_iter().take_while(|p| p.x <= wall).collect();
    let gaps: Vec<f64> = arm.iter().map(|p| p.y - yt(p.x)).collect();
    let corner = -gaps.iter().copied().fold(f64::MAX, f64::min).min(0.0);
    let mut found = vec![Junction { joint: Joint::ShoulderCorner, step: corner / r, slope_break_deg: 0.0 }];
    let h = REACH * r;
    let near = (SEPARATION * r, 6_usize);
    for i in 1..arm.len().saturating_sub(near.1) {
        let (a, b) = (gaps[i - 1], gaps[i]);
        let separated = |range: std::ops::Range<usize>| range.map(|k| gaps[k].abs()).fold(0.0, f64::max) > near.0;
        if a * b < 0.0 && separated(i.saturating_sub(near.1)..i) && separated(i..(i + near.1).min(gaps.len())) {
            let x = arm[i].x;
            let slope_arm = (arm[(i + 2).min(arm.len() - 1)].y - arm[i.saturating_sub(2)].y)
                / (arm[(i + 2).min(arm.len() - 1)].x - arm[i.saturating_sub(2)].x);
            let slope_line = (yt(x + h) - yt(x - h)) / (2.0 * h);
            found.push(Junction {
                joint: Joint::ShoulderCrossing,
                step: 0.0,
                slope_break_deg: angle_between(slope_arm, slope_line),
            });
        }
    }
    found
}

/// How far the top of the torso and the left arm, seen from the front, climbs again after the outline has started down
/// from the neck, as a fraction of the torso's half-width.
fn shoulder_rise(l: &BodyLayout, r: f64, unit: f64) -> f64 {
    let mask = silhouette_of(l, &[BodyRegion::Torso, BodyRegion::ArmL], View::Front, PIXEL, unit);
    let at = |row_up: usize, col: usize| mask.on[(mask.height - 1 - row_up) * mask.width + col];
    let top_at = |col: usize| (0..mask.height).rev().find(|&row| at(row, col));
    // From the neck's base outward: the torso's top is flat to there.
    let neck = l.section64(BodyRegion::Torso, 1.0).unwrap().radii.x / unit;
    let first = ((neck - View::Front.window().0) / PIXEL).ceil() as usize;
    let last = (0..mask.width).rev().find(|&c| (0..mask.height).any(|row| at(row, c))).unwrap_or(first);
    let (mut lowest, mut rise) = (usize::MAX, 0_usize);
    for col in first..=last {
        if let Some(row) = top_at(col) {
            lowest = lowest.min(row);
            rise = rise.max(row - lowest);
        }
    }
    rise as f64 * PIXEL / (r / unit)
}

/// Every hip and shoulder junction of `l`'s front outline; `unit` is the scale of its pictures ([`unit_of`]).
fn measure(l: &BodyLayout, unit: f64) -> Vec<Junction> {
    let r = half_width(l);
    let mut all = hip(l, r);
    all.extend(shoulder(l, r));
    all.push(Junction { joint: Joint::ShoulderRise, step: shoulder_rise(l, r, unit), slope_break_deg: 0.0 });
    all
}

// ------------------------------------------------------------------------------------------------ the bodies

fn templates() -> Vec<&'static str> {
    registry::templates().iter().map(|t| t.id()).collect()
}

fn layout_of(template: &str, shape: &BodyShape) -> BodyLayout {
    registry::find(template).unwrap().layout(&Proportions::default(), shape, &HandParams::default())
}

/// The scale of a template's masks: its height to the head base over soft-sd's, so a taller body fits the same window.
fn unit_of(template: &str) -> f64 {
    layout_of(template, &BodyShape::default()).head_base().unwrap().y / 0.56
}

/// Every shape the junctions must hold at: the template as it is, the ten presets and the 32 corners of the cube.
fn shapes() -> Vec<(String, BodyShape)> {
    let mut out = vec![("template".to_owned(), BodyShape::default())];
    out.extend(shape::presets().unwrap().iter().map(|p| (p.id.as_str().to_owned(), p.shape.clone())));
    for c in 0..32_u32 {
        let pick = |bit: u32| if c >> bit & 1 == 1 { 1.0 } else { -1.0 };
        let shape = BodyShape {
            height: pick(0),
            build: pick(1),
            curves: pick(2),
            musculature: pick(3),
            shoulder_hip: pick(4),
            preset: None,
        };
        out.push((format!("corner-{c:02}"), shape));
    }
    out
}

// ------------------------------------------------------------------------------------------------ the tests

#[test]
fn the_outline_is_c1_across_the_hip_of_every_template_at_every_preset_and_corner() {
    let mut failures = Vec::new();
    // The shoulder's worst per template, reported: epaulette rise as a share of the torso's half-width.
    let mut shoulder: Vec<(&str, f64)> = Vec::new();
    for id in templates() {
        for (name, shape) in shapes() {
            for j in measure(&layout_of(id, &shape), unit_of(id)) {
                if j.joint.is_shoulder() {
                    let worst = j.step.abs();
                    match shoulder.iter_mut().find(|(t, _)| *t == id) {
                        Some(entry) => entry.1 = entry.1.max(worst),
                        None => shoulder.push((id, worst)),
                    }
                } else if !j.is_c1() {
                    failures.push(format!(
                        "{id} {name}: {:?} step {:+.2}%, break {:+.1} deg",
                        j.joint,
                        j.step * 100.0,
                        j.slope_break_deg
                    ));
                }
            }
        }
    }
    for (id, worst) in shoulder {
        eprintln!("shoulder (reported): {id}: worst step or rise {:.1}% of the torso's half-width", worst * 100.0);
    }
    assert!(failures.is_empty(), "{} junctions are not C1:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn every_template_has_a_hip_and_a_shoulder_to_measure() {
    for id in templates() {
        let found = measure(&layout_of(id, &BodyShape::default()), unit_of(id));
        assert!(found.iter().any(|j| j.joint == Joint::Crotch), "{id}: no crotch found");
        assert!(found.iter().any(|j| j.joint == Joint::ShoulderCorner), "{id}: no shoulder found");
    }
}

/// Prints the table of every junction (`OMUZAKI_JUNCTIONS=<file>` writes it there).
#[test]
#[ignore = "writes the junction table to $OMUZAKI_JUNCTIONS"]
fn report() {
    let mut table = String::from("template\tshape\tjunction\tstep_pct\tbreak_deg\n");
    for id in templates() {
        for (name, shape) in shapes() {
            for j in measure(&layout_of(id, &shape), unit_of(id)) {
                writeln!(table, "{id}\t{name}\t{:?}\t{:+.2}\t{:+.1}", j.joint, j.step * 100.0, j.slope_break_deg)
                    .unwrap();
            }
        }
    }
    fs::write(PathBuf::from(std::env::var("OMUZAKI_JUNCTIONS").expect("set OMUZAKI_JUNCTIONS to a file")), table)
        .unwrap();
}

/// Writes the pictures `tools/junction-sheet.py` composes: for every template and shape, a PGM per view where each
/// region is a grey level (its index), so the sheet can colour the regions and draw the outline.
#[test]
#[ignore = "writes the junction pictures to $OMUZAKI_JUNCTION_DIR"]
fn pictures() {
    let dir = PathBuf::from(std::env::var("OMUZAKI_JUNCTION_DIR").expect("set OMUZAKI_JUNCTION_DIR to a directory"));
    fs::create_dir_all(&dir).unwrap();
    let mut manifest = String::new();
    let shapes = shapes();
    let wanted = ["template", "average", "curvy", "athletic"];
    for id in templates() {
        let unit = unit_of(id);
        for (name, shape) in shapes.iter().filter(|(n, _)| wanted.contains(&n.as_str())) {
            let l = layout_of(id, shape);
            let mut files = Vec::new();
            for view in [View::Front, View::Side, View::ThreeQuarter] {
                let mut bytes = Vec::new();
                let (mut w, mut h) = (0, 0);
                let mut label: Vec<u8> = Vec::new();
                for (k, &region) in REGIONS.iter().enumerate() {
                    let m = silhouette_of(&l, &[region], view, 0.001, unit);
                    (w, h) = (m.width, m.height);
                    label.resize(m.on.len(), 0);
                    for (px, &on) in label.iter_mut().zip(&m.on) {
                        if on {
                            *px = k as u8 + 1;
                        }
                    }
                }
                bytes.extend(format!("P5\n{w} {h}\n255\n").into_bytes());
                bytes.extend(label);
                let file = format!("{id}-{name}-{view:?}.pgm");
                fs::write(dir.join(&file), bytes).unwrap();
                files.push(file);
            }
            writeln!(manifest, "{id}\t{name}\t{}", files.join("\t")).unwrap();
        }
    }
    fs::write(dir.join("manifest.tsv"), manifest).unwrap();
}
