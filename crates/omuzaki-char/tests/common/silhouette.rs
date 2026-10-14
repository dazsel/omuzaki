//! Silhouettes of a built body, shared by the tests that judge its outline: every region is a loft of its section
//! ellipses, drawn as the hull of each pair of neighbouring sections, so a tilted arm or a leaning calf is drawn as it is.

#![allow(dead_code)] // each test target uses its own part
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // pixel indices

use omuzaki_char::template::layout::{BodyLayout, section_basis};
use omuzaki_char_api::BodyRegion;
use omuzaki_core::glam::{DVec2, DVec3};

// ------------------------------------------------------------------------------------------------ silhouettes

/// Which way the body is looked at.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// From the front: screen x is the body's x.
    Front,
    /// From the body's left: screen x is forward (z).
    Side,
    /// From the front left, turned 45 degrees: screen x is `(x + z) / sqrt 2`.
    ThreeQuarter,
}

/// A black-and-white picture of a body in a fixed window of the world, so two bodies' masks line up pixel for pixel.
#[derive(Clone)]
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub on: Vec<bool>,
}

/// The window the masks cover, in metres (screen x across, y up): wide enough for the broadest, tallest corner.
pub const WINDOW_X: (f64, f64) = (-0.31, 0.31);
pub const WINDOW_Z: (f64, f64) = (-0.14, 0.14);
pub const WINDOW_Y: (f64, f64) = (-0.01, 0.70);
/// Sections drawn along each region, and points around each section.
const RINGS: usize = 48;
const AROUND: usize = 24;

pub const REGIONS: [BodyRegion; 8] = [
    BodyRegion::Torso,
    BodyRegion::Neck,
    BodyRegion::ArmL,
    BodyRegion::ArmR,
    BodyRegion::LegL,
    BodyRegion::LegR,
    BodyRegion::FootL,
    BodyRegion::FootR,
];

impl View {
    /// The window of screen x this view needs.
    pub fn window(self) -> (f64, f64) {
        if self == Self::Side { WINDOW_Z } else { WINDOW_X }
    }

    /// Where `p` falls across the screen.
    pub fn across(self, p: DVec3) -> f64 {
        match self {
            Self::Front => p.x,
            Self::Side => p.z,
            Self::ThreeQuarter => (p.x + p.z) * std::f64::consts::FRAC_1_SQRT_2,
        }
    }
}

impl Mask {
    pub fn blank(window: (f64, f64), pixel: f64) -> Self {
        let width = ((window.1 - window.0) / pixel).round() as usize;
        let height = ((WINDOW_Y.1 - WINDOW_Y.0) / pixel).round() as usize;
        Self { width, height, on: vec![false; width * height] }
    }

    /// Fills the convex polygon `hull` (screen coordinates in metres) into the mask.
    pub fn fill(&mut self, hull: &[DVec2], window: (f64, f64), pixel: f64) {
        let (lo, hi) = hull.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
        let first = (((lo - WINDOW_Y.0) / pixel).floor().max(0.0)) as usize;
        let last = (((hi - WINDOW_Y.0) / pixel).ceil().max(0.0) as usize).min(self.height.saturating_sub(1));
        for row in first..=last {
            let y = WINDOW_Y.0 + (row as f64 + 0.5) * pixel;
            let (mut a, mut b) = (f64::MAX, f64::MIN);
            for (p, q) in hull.iter().zip(hull.iter().cycle().skip(1)) {
                if (p.y - y) * (q.y - y) <= 0.0 && (q.y - p.y).abs() > 1e-15 {
                    let x = p.x + (q.x - p.x) * (y - p.y) / (q.y - p.y);
                    a = a.min(x);
                    b = b.max(x);
                }
            }
            if a > b {
                continue;
            }
            let from = (((a - window.0) / pixel - 0.5).ceil().max(0.0)) as usize;
            let to = (((b - window.0) / pixel - 0.5).floor()).max(-1.0);
            if to < 0.0 {
                continue;
            }
            for col in from..=(to as usize).min(self.width - 1) {
                // Image rows run top-down.
                self.on[(self.height - 1 - row) * self.width + col] = true;
            }
        }
    }

    /// The share of pixels that are on in exactly one of the two masks, out of those on in either: `0` for the same
    /// silhouette, `1` for none in common.
    pub fn difference(&self, other: &Self) -> f64 {
        let (mut either, mut one) = (0_usize, 0_usize);
        for (&a, &b) in self.on.iter().zip(&other.on) {
            either += usize::from(a || b);
            one += usize::from(a != b);
        }
        one as f64 / either.max(1) as f64
    }
}

/// The convex hull of `points`, counter-clockwise (Andrew's monotone chain).
fn hull(mut points: Vec<DVec2>) -> Vec<DVec2> {
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    let cross = |o: DVec2, a: DVec2, b: DVec2| (a - o).perp_dot(b - o);
    let mut chain: Vec<DVec2> = Vec::with_capacity(points.len() * 2);
    for pass in 0..2 {
        let start = chain.len();
        for &p in &points {
            while chain.len() >= start + 2 && cross(chain[chain.len() - 2], chain[chain.len() - 1], p) <= 0.0 {
                chain.pop();
            }
            chain.push(p);
        }
        chain.pop();
        if pass == 0 {
            points.reverse();
        }
    }
    chain
}

/// The silhouette of a body seen from `view`: every region is a loft of its section ellipses, drawn as the hull of
/// each pair of neighbouring sections, so a tilted arm or a leaning calf is drawn as it is. The window is fixed in
/// `unit`s of the body (one for soft-sd's 0.56 m to the head base, more for a taller template), `pixel` is in those.
pub fn silhouette(l: &BodyLayout, view: View, pixel: f64, unit: f64) -> Mask {
    silhouette_of(l, &REGIONS, view, pixel, unit)
}

/// The silhouette of some of a body's regions only.
pub fn silhouette_of(l: &BodyLayout, regions: &[BodyRegion], view: View, pixel: f64, unit: f64) -> Mask {
    let window = view.window();
    let mut mask = Mask::blank(window, pixel);
    for &region in regions {
        let rings: Vec<Vec<DVec2>> = (0..=RINGS)
            .map(|k| {
                let s = l.section64(region, k as f64 / RINGS as f64).unwrap();
                let (across, through) = section_basis(region, s.axis.as_vec3());
                let (across, through) = (across.as_dvec3(), through.as_dvec3());
                (0..AROUND)
                    .map(|j| {
                        let a = j as f64 / AROUND as f64 * std::f64::consts::TAU;
                        let p: DVec3 = s.centre + across * (s.radii.x * a.sin()) + through * (s.radii.y * a.cos());
                        DVec2::new(view.across(p), p.y) / unit
                    })
                    .collect()
            })
            .collect();
        for pair in rings.windows(2) {
            mask.fill(&hull(pair.concat()), window, pixel);
        }
    }
    mask
}
