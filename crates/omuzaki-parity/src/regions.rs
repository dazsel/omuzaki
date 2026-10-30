//! The regions a parity score is broken into: shapes given in the reference's pixels (rectangles, ellipses, polygons,
//! mask images), and regions found from the two images themselves: the silhouette, the background, the outline band,
//! and, from eye, mouth and chin points, the eyes, the mouth, the face, the hair and the body.
//!
//! A found region is the union of what it is in our render and in the reference, so a part that is missing on one side
//! or extra on the other counts against the score instead of falling outside it.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::metrics::distance;
use crate::image::ImageRef;

/// Named points on an image, in its pixels: `eye_right` (the character's right eye, on the viewer's left),
/// `eye_left`, `mouth` and `chin` drive the found regions and the point alignment; other names only align. The same
/// names `render.preview` returns in `face_landmarks`.
pub type Points = BTreeMap<String, [f64; 2]>;

/// A region given by hand, in the reference's pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RegionSpec {
    /// The region's name, such as `hair` or `left_sleeve`. A given region replaces a found one of the same name.
    pub name: String,
    /// Its shape.
    #[serde(flatten)]
    pub shape: Shape,
}

/// The shape of a given region.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum Shape {
    /// An axis-aligned rectangle.
    Rect {
        /// The left edge.
        x: f64,
        /// The top edge.
        y: f64,
        /// The width.
        w: f64,
        /// The height.
        h: f64,
    },
    /// An ellipse.
    Ellipse {
        /// The centre.
        center: [f64; 2],
        /// The half-axes, along the ellipse's own x and y.
        radii: [f64; 2],
        /// The turn of its x axis, degrees clockwise on screen.
        #[serde(default)]
        angle_deg: f64,
    },
    /// A closed polygon (even-odd fill).
    Polygon {
        /// The corners, at least three.
        points: Vec<[f64; 2]>,
    },
    /// A mask image the reference's size: the pixels that are opaque and light (alpha and luma above one half).
    Mask {
        /// The mask image.
        image: ImageRef,
    },
}

/// A region as the scorer uses it.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    /// Its name.
    pub name: String,
    /// The pixels it holds.
    pub mask: Vec<bool>,
    /// `given` or `found`.
    pub source: &'static str,
}

/// Fills `shape` (not a mask image) into a `w x h` grid, testing each pixel's centre.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // pixel grids
pub fn fill(shape: &Shape, w: usize, h: usize) -> Vec<bool> {
    let mut out = vec![false; w * h];
    let mut set = |test: &dyn Fn(f64, f64) -> bool, bounds: [f64; 4]| {
        let x0 = bounds[0].floor().max(0.0) as usize;
        let y0 = bounds[1].floor().max(0.0) as usize;
        let x1 = (bounds[2].ceil().max(0.0) as usize).min(w);
        let y1 = (bounds[3].ceil().max(0.0) as usize).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                if test(x as f64 + 0.5, y as f64 + 0.5) {
                    out[y * w + x] = true;
                }
            }
        }
    };
    match shape {
        Shape::Rect { x, y, w: rw, h: rh } => {
            let (x, y, rw, rh) = (*x, *y, *rw, *rh);
            set(&|px, py| px >= x && px < x + rw && py >= y && py < y + rh, [x, y, x + rw, y + rh]);
        }
        Shape::Ellipse { center, radii, angle_deg } => {
            let e = Ellipse { center: *center, radii: *radii, angle_deg: *angle_deg };
            let r = radii[0].abs().max(radii[1].abs());
            set(&|px, py| e.contains(px, py), [center[0] - r, center[1] - r, center[0] + r, center[1] + r]);
        }
        Shape::Polygon { points } => {
            if points.len() >= 3 {
                let xs = points.iter().map(|p| p[0]);
                let ys = points.iter().map(|p| p[1]);
                let bounds = [
                    xs.clone().fold(f64::INFINITY, f64::min),
                    ys.clone().fold(f64::INFINITY, f64::min),
                    xs.fold(f64::NEG_INFINITY, f64::max),
                    ys.fold(f64::NEG_INFINITY, f64::max),
                ];
                set(&|px, py| inside_polygon(points, px, py), bounds);
            }
        }
        Shape::Mask { .. } => {}
    }
    out
}

/// Whether `(x, y)` is inside the polygon (even-odd rule).
fn inside_polygon(points: &[[f64; 2]], x: f64, y: f64) -> bool {
    let mut inside = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (a, b) = (points[i], points[j]);
        if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// An ellipse in pixels.
#[derive(Copy, Clone, Debug)]
struct Ellipse {
    center: [f64; 2],
    radii: [f64; 2],
    angle_deg: f64,
}

impl Ellipse {
    fn contains(&self, x: f64, y: f64) -> bool {
        let (s, c) = self.angle_deg.to_radians().sin_cos();
        let (dx, dy) = (x - self.center[0], y - self.center[1]);
        let (u, v) = (c * dx + s * dy, -s * dx + c * dy);
        let (rx, ry) = (self.radii[0].abs().max(1e-9), self.radii[1].abs().max(1e-9));
        (u / rx).powi(2) + (v / ry).powi(2) <= 1.0
    }
}

/// The face frame of one image's points: the eye midpoint, the inter-eye distance and the axes along and down the
/// face. `None` without both eyes, or with the eyes on one pixel.
#[derive(Copy, Clone, Debug)]
struct FaceFrame {
    mid: [f64; 2],
    iod: f64,
    u: [f64; 2],
    v: [f64; 2],
    roll_deg: f64,
    mouth: [f64; 2],
    chin: [f64; 2],
}

impl FaceFrame {
    fn of(points: &Points) -> Option<Self> {
        let (r, l) = (points.get("eye_right")?, points.get("eye_left")?);
        let d = [l[0] - r[0], l[1] - r[1]];
        let iod = d[0].hypot(d[1]);
        if iod < 1.0 {
            return None;
        }
        let u = [d[0] / iod, d[1] / iod];
        let v = [-u[1], u[0]];
        let mid = [f64::midpoint(r[0], l[0]), f64::midpoint(r[1], l[1])];
        let along = |k: f64| [mid[0] + v[0] * k * iod, mid[1] + v[1] * k * iod];
        Some(Self {
            mid,
            iod,
            u,
            v,
            roll_deg: u[1].atan2(u[0]).to_degrees(),
            // Anime proportions when the points are missing: the mouth at 0.85 and the chin at 1.25 eye distances.
            mouth: points.get("mouth").copied().unwrap_or_else(|| along(0.85)),
            chin: points.get("chin").copied().unwrap_or_else(|| along(1.25)),
        })
    }

    /// `(s, t)`: a pixel's coordinates along and down the face, in eye distances from the eye midpoint.
    fn local(&self, x: f64, y: f64) -> (f64, f64) {
        let (dx, dy) = (x - self.mid[0], y - self.mid[1]);
        ((dx * self.u[0] + dy * self.u[1]) / self.iod, (dx * self.v[0] + dy * self.v[1]) / self.iod)
    }

    fn chin_t(&self) -> f64 {
        self.local(self.chin[0], self.chin[1]).1
    }

    /// The face's own regions in a `w x h` grid: the eyes, the mouth, the face and the head box (everything above
    /// just below the chin, within 2.2 eye distances to either side).
    #[allow(clippy::cast_precision_loss)] // pixel grids
    fn masks(&self, points: &Points, w: usize, h: usize) -> BTreeMap<&'static str, Vec<bool>> {
        let iod = self.iod;
        let eye = |name: &str| {
            points.get(name).map(|c| {
                fill(&Shape::Ellipse { center: *c, radii: [0.36 * iod, 0.30 * iod], angle_deg: self.roll_deg }, w, h)
            })
        };
        let mut out = BTreeMap::new();
        if let Some(m) = eye("eye_right") {
            out.insert("eye_right", m);
        }
        if let Some(m) = eye("eye_left") {
            out.insert("eye_left", m);
        }
        out.insert(
            "mouth",
            fill(
                &Shape::Ellipse { center: self.mouth, radii: [0.32 * iod, 0.2 * iod], angle_deg: self.roll_deg },
                w,
                h,
            ),
        );
        let (top, bottom) = (-0.65, self.chin_t());
        let centre_t = f64::midpoint(top, bottom);
        let centre = [self.mid[0] + self.v[0] * centre_t * iod, self.mid[1] + self.v[1] * centre_t * iod];
        out.insert(
            "face",
            fill(
                &Shape::Ellipse {
                    center: centre,
                    radii: [1.0 * iod, (bottom - top) / 2.0 * iod],
                    angle_deg: self.roll_deg,
                },
                w,
                h,
            ),
        );
        let limit = bottom + 0.15;
        let head = (0..w * h)
            .map(|i| {
                let (s, t) = self.local((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
                s.abs() <= 2.2 && t <= limit
            })
            .collect();
        out.insert("head", head);
        out
    }
}

/// The boundary of a mask: its pixels with a 4-neighbour outside it (or on the image's edge).
fn boundary(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    (0..w * h)
        .map(|i| {
            if !mask[i] {
                return false;
            }
            let (x, y) = (i % w, i / w);
            x == 0 || y == 0 || x + 1 == w || y + 1 == h || !mask[i - 1] || !mask[i + 1] || !mask[i - w] || !mask[i + w]
        })
        .collect()
}

fn or(a: &[bool], b: &[bool]) -> Vec<bool> {
    a.iter().zip(b).map(|(a, b)| *a || *b).collect()
}

/// The regions found from both images' silhouettes and points (see the module's notes), then the given regions, which
/// replace found ones of the same name. `outline_px` is the half-width of the outline band.
#[must_use]
pub fn found_regions(
    w: usize,
    h: usize,
    ours_silhouette: &[bool],
    reference_silhouette: &[bool],
    ours_points: Option<&Points>,
    reference_points: Option<&Points>,
    outline_px: f32,
) -> Vec<Region> {
    let both = or(ours_silhouette, reference_silhouette);
    let mut out = vec![
        Region { name: "silhouette".into(), mask: both.clone(), source: "found" },
        Region { name: "background".into(), mask: both.iter().map(|s| !s).collect(), source: "found" },
    ];
    let edge = or(&boundary(ours_silhouette, w, h), &boundary(reference_silhouette, w, h));
    let near = distance(w, h, &edge);
    out.push(Region {
        name: "outline".into(),
        mask: near.v.iter().map(|d| *d <= outline_px).collect(),
        source: "found",
    });
    // Each side's face regions from its own points (or the other side's, when it has none), then their union.
    let ours_frame = ours_points.and_then(FaceFrame::of);
    let ref_frame = reference_points.and_then(FaceFrame::of);
    let sides: Vec<(FaceFrame, &Points)> = [
        ours_frame.zip(ours_points).or(ref_frame.zip(reference_points)),
        ref_frame.zip(reference_points).or(ours_frame.zip(ours_points)),
    ]
    .into_iter()
    .flatten()
    .collect();
    if sides.is_empty() {
        return out;
    }
    let mut merged: BTreeMap<&'static str, Vec<bool>> = BTreeMap::new();
    for (frame, points) in &sides {
        for (name, mask) in frame.masks(points, w, h) {
            merged.entry(name).and_modify(|m| *m = or(m, &mask)).or_insert(mask);
        }
    }
    let empty = vec![false; w * h];
    let head = merged.remove("head").unwrap_or_else(|| empty.clone());
    let face = merged.get("face").cloned().unwrap_or_else(|| empty.clone());
    for name in ["eye_right", "eye_left", "mouth"] {
        if let Some(mask) = merged.remove(name) {
            out.push(Region { name: name.into(), mask, source: "found" });
        }
    }
    let and = |a: &[bool], b: &[bool]| a.iter().zip(b).map(|(a, b)| *a && *b).collect::<Vec<_>>();
    out.push(Region { name: "face".into(), mask: and(&face, &both), source: "found" });
    let hair = (0..w * h).map(|i| both[i] && head[i] && !face[i]).collect();
    out.push(Region { name: "hair".into(), mask: hair, source: "found" });
    let body = (0..w * h).map(|i| both[i] && !head[i]).collect();
    out.push(Region { name: "body".into(), mask: body, source: "found" });
    out
}

/// Puts the given regions in: each replaces a found region of its name, or is added.
pub fn merge_given(regions: &mut Vec<Region>, given: Vec<Region>) {
    for g in given {
        if let Some(r) = regions.iter_mut().find(|r| r.name == g.name) {
            *r = g;
        } else {
            regions.push(g);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_fill_their_pixels() {
        let rect = fill(&Shape::Rect { x: 2.0, y: 1.0, w: 3.0, h: 2.0 }, 8, 4);
        assert_eq!(rect.iter().filter(|b| **b).count(), 6);
        let tri = fill(&Shape::Polygon { points: vec![[0.0, 0.0], [8.0, 0.0], [0.0, 8.0]] }, 8, 8);
        assert_eq!(tri.iter().filter(|b| **b).count(), 28);
        let disc = fill(&Shape::Ellipse { center: [10.0, 10.0], radii: [5.0, 5.0], angle_deg: 0.0 }, 20, 20);
        let n = disc.iter().filter(|b| **b).count();
        assert!((70..=88).contains(&n), "{n}");
    }

    #[test]
    fn a_face_splits_into_eyes_mouth_face_hair_and_body() {
        let (w, h) = (200, 300);
        let sil: Vec<bool> = (0..w * h).map(|i: usize| (i % w).abs_diff(100) < 70).collect();
        let points: Points =
            [("eye_right".to_owned(), [80.0, 100.0]), ("eye_left".to_owned(), [120.0, 100.0])].into_iter().collect();
        let regions = found_regions(w, h, &sil, &sil, Some(&points), Some(&points), 2.0);
        let names: Vec<&str> = regions.iter().map(|r| r.name.as_str()).collect();
        for want in ["silhouette", "background", "outline", "eye_right", "eye_left", "mouth", "face", "hair", "body"] {
            assert!(names.contains(&want), "{names:?}");
        }
        let get = |n: &str| &regions.iter().find(|r| r.name == n).unwrap().mask;
        assert!(get("eye_right")[100 * w + 80] && get("face")[110 * w + 100] && !get("hair")[110 * w + 100]);
        assert!(get("hair")[40 * w + 100] && get("body")[290 * w + 100]);
    }
}
