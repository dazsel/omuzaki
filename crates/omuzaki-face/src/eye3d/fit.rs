//! Fitting an eye to a photograph: given landmarks measured on a picture (the opening's outline, the iris and the pupil,
//! in its pixels), find the opening's shape, the iris's and pupil's sizes, the gaze and the camera that put the eye's
//! own outlines on them. Only those numbers are used, never the picture's pixels: the eye is still drawn by its design.
//!
//! The eye's outlines are projected the way the close-up renders them (the shot's camera, the margin laid on the eye's
//! surface, the iris seen through the cornea), and the fit is a pattern search over the parameters inside the design's
//! own ranges. Besides the landmarks it keeps the opening on the eyeball as the camera sees it: lids rest on the
//! globe, so an opening reaching past the ball's silhouette would show the socket behind it where a photograph shows
//! the white. It is plain arithmetic, well under a second.

use omuzaki_core::glam::{Vec2, Vec3, Vec4};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::closeup::Shot;
use super::design::EyeDesign;
use super::geometry::{LIMBUS, MM, margin_point};
use super::noise::smoothstep;

/// How much the cornea magnifies the pupil against the iris, as the close-up renders it: the pupil sits nearer the
/// cornea's centre, where its lens is strongest (measured on the renderer's macro view).
const PUPIL_MAGNIFY: f32 = 1.17;
/// How far in front of the limbus's plane the iris appears through the cornea, metres.
const IRIS_LIFT: f32 = 0.000_3;
/// Samples along each lid when the outline is projected.
const ALONG: usize = 32;

/// A circle in a picture, pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Circle {
    /// Its centre, `[x, y]` from the picture's top left.
    pub centre: [f32; 2],
    /// Its radius.
    pub radius: f32,
}

/// Landmarks measured on a picture of an eye, in its pixels from the top left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Landmarks {
    /// The picture's width and height.
    pub image: [u32; 2],
    /// Points along the edge of the visible eye (where the lids meet the white and the iris), in order round the
    /// opening from either corner; 8 or more, the more the closer the fit.
    pub outline: Vec<[f32; 2]>,
    /// The iris: its centre and radius (half its width).
    pub iris: Circle,
    /// The pupil, if it was measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pupil: Option<Circle>,
    /// Points on the eyeball's own edge where the picture shows it (the white running out to the eyeball's curve
    /// rather than to a lid, often toward the inner corner of an eye seen from the side); none if it doesn't.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ball_edge: Vec<[f32; 2]>,
}

impl Landmarks {
    /// Why the landmarks can't be fitted, if they can't.
    ///
    /// # Errors
    /// What is wrong, naming the field.
    pub fn check(&self) -> Result<(), String> {
        let [w, h] = self.image;
        if !(16..=16384).contains(&w) || !(16..=16384).contains(&h) {
            return Err("image: width and height must be 16 to 16384".to_owned());
        }
        if self.outline.len() < 8 {
            return Err("outline: needs at least 8 points round the opening".to_owned());
        }
        let points = self
            .outline
            .iter()
            .chain(&self.ball_edge)
            .chain([&self.iris.centre])
            .chain(self.pupil.as_ref().map(|p| &p.centre));
        if points.flat_map(|p| p.iter()).any(|v| !v.is_finite()) {
            return Err("every point must be finite".to_owned());
        }
        for (name, circle) in [("iris", Some(&self.iris)), ("pupil", self.pupil.as_ref())] {
            if let Some(c) = circle
                && !(c.radius.is_finite() && c.radius > 0.0)
            {
                return Err(format!("{name}: the radius must be more than 0"));
            }
        }
        Ok(())
    }
}

/// How far the fitted eye's outlines are from the landmarks, pixels.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Residuals {
    /// The mean distance between the outline and the landmarks' outline, both ways.
    pub outline: f32,
    /// The iris's centre's distance.
    pub iris_centre: f32,
    /// The iris's radius's difference.
    pub iris_radius: f32,
    /// The pupil's centre's distance (0 when no pupil was measured).
    pub pupil_centre: f32,
    /// The pupil's radius's difference (0 when no pupil was measured).
    pub pupil_radius: f32,
    /// How far the opening reaches past the eyeball's silhouette, on average over its outline (0 when it rests on the
    /// ball everywhere, as a real eye's does).
    #[serde(default)]
    pub off_ball: f32,
    /// The mean distance from the eyeball's edge points to its outline as the camera sees it (0 when none were
    /// measured).
    #[serde(default)]
    pub ball_edge: f32,
}

impl Residuals {
    fn total(&self) -> f32 {
        self.outline
            + self.iris_centre
            + self.iris_radius
            + f32::midpoint(self.pupil_centre, self.pupil_radius)
            + self.off_ball
            + self.ball_edge
    }
}

/// An eye fitted to landmarks: the design with its fitted opening, iris and pupil, the shot that frames it, and what is
/// left of the error.
#[derive(Clone, Debug, PartialEq)]
pub struct Fitted {
    /// The design: its aperture and its eyeball's, iris's and pupil's sizes fitted, everything else as it was.
    pub design: EyeDesign,
    /// The camera and the gaze.
    pub shot: Shot,
    /// The remaining error, pixels.
    pub residuals: Residuals,
    /// The error before the fit, pixels.
    pub before: Residuals,
}

/// The eye's outlines in a picture of `size` taken as `shot` takes it, pixels from the top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Projected {
    /// The opening's edge: the upper lid from the outer corner to the inner, then the lower lid back.
    pub outline: Vec<Vec2>,
    /// The iris's centre and radius.
    pub iris: (Vec2, f32),
    /// The pupil's centre and radius.
    pub pupil: (Vec2, f32),
    /// How far the opening reaches past the eyeball's silhouette, averaged over the outline (the lids'
    /// tuck, under a millimetre, allowed, and the corners left out).
    pub off_ball: f32,
    /// The eyeball's outline as the camera sees it: the circle where the lines of sight graze it.
    pub ball: Vec<Vec2>,
}

/// Where `design`'s outlines land in a `size` picture taken as `shot` takes it.
pub fn project(design: &EyeDesign, shot: &Shot, size: [u32; 2]) -> Projected {
    let (w, h) = (pixels(size[0]), pixels(size[1]));
    let camera = shot.camera();
    let clip = camera.projection_matrix(w / h) * camera.view();
    let to_px = |p: Vec3| {
        let c = clip * Vec4::new(p.x, p.y, p.z, 1.0);
        let n = c.truncate() / c.w.max(1e-9);
        Vec2::new(f32::midpoint(n.x, 1.0) * w, f32::midpoint(1.0, -n.y) * h)
    };
    // Pixels per metre at unit distance, for the silhouette's reach.
    let focal = 0.5 * h / (0.5 * shot.fov.to_radians()).tan();
    let eye = Vec3::from(shot.camera);
    let tuck = design.ball.radius + 0.8 * MM;
    let mut outline = Vec::with_capacity(2 * (ALONG + 1));
    let mut off_ball = 0.0;
    for upper in [true, false] {
        for i in 0..=ALONG {
            #[allow(clippy::cast_precision_loss, reason = "a few dozen samples")]
            let t = i as f32 / ALONG as f32;
            let u = if upper { t } else { 1.0 - t };
            let point = margin_point(design, u, upper);
            outline.push(to_px(point));
            // How far the line of sight to the margin passes outside the ball, in pixels at its depth. The corners are
            // left out: the inner one runs past the ball to the caruncle, and the lids meet in a point at the outer.
            let corners = smoothstep(0.0, 0.2, u) * (1.0 - smoothstep(0.8, 1.0, u));
            let ray = (point - eye).normalize_or_zero();
            let along = -eye.dot(ray);
            let miss = (eye + ray * along).length();
            if along > 0.0 && miss > tuck {
                off_ball += corners * (miss - tuck) * focal / along;
            }
        }
    }
    #[allow(clippy::cast_precision_loss, reason = "a few dozen samples")]
    let off_ball = off_ball / outline.len() as f32;
    // The iris as the cornea shows it: the limbus's circle, a little forward, turned with the gaze.
    let forward = shot.forward();
    let right = Vec3::Y.cross(forward).normalize_or(Vec3::X);
    let up = forward.cross(right);
    let ball = &design.ball;
    let edge = ball.iris_radius * LIMBUS;
    let depth = (ball.radius * ball.radius - edge * edge).max(0.0).sqrt() + IRIS_LIFT;
    let circle = |radius: f32| {
        let centre = to_px(forward * depth);
        let mut sum = 0.0;
        for k in 0..16 {
            #[allow(clippy::cast_precision_loss, reason = "sixteen samples")]
            let a = std::f32::consts::TAU * k as f32 / 16.0;
            sum += (to_px(forward * depth + (right * a.cos() + up * a.sin()) * radius) - centre).length();
        }
        (centre, sum / 16.0)
    };
    Projected {
        outline,
        iris: circle(ball.iris_radius),
        pupil: circle(ball.iris_radius * ball.pupil * PUPIL_MAGNIFY),
        off_ball,
        ball: silhouette(ball.radius, eye).into_iter().map(to_px).collect(),
    }
}

#[allow(clippy::cast_precision_loss, reason = "a picture's side, at most 16384")]
fn pixels(side: u32) -> f32 {
    side as f32
}

/// How far `design` seen through `shot` is from `marks`.
pub fn residuals(design: &EyeDesign, shot: &Shot, marks: &Landmarks) -> Residuals {
    let p = project(design, shot, marks.image);
    let ours = dense(&p.outline);
    let theirs: Vec<Vec2> = marks.outline.iter().map(|&q| Vec2::from(q)).collect();
    let mut closed = theirs.clone();
    if let Some(&first) = theirs.first() {
        closed.push(first);
    }
    let theirs = dense(&closed);
    let iris_centre = Vec2::from(marks.iris.centre);
    let (pupil_centre, pupil_radius) = marks
        .pupil
        .as_ref()
        .map_or((0.0, 0.0), |m| ((p.pupil.0 - Vec2::from(m.centre)).length(), (p.pupil.1 - m.radius).abs()));
    Residuals {
        outline: f32::midpoint(mean_nearest(&ours, &theirs), mean_nearest(&theirs, &ours)),
        iris_centre: (p.iris.0 - iris_centre).length(),
        iris_radius: (p.iris.1 - marks.iris.radius).abs(),
        pupil_centre,
        pupil_radius,
        off_ball: p.off_ball,
        ball_edge: if marks.ball_edge.is_empty() {
            0.0
        } else {
            let edge: Vec<Vec2> = marks.ball_edge.iter().map(|&q| Vec2::from(q)).collect();
            mean_nearest(&edge, &dense(&p.ball))
        },
    }
}

/// The circle where lines of sight from `eye` graze a ball of `radius` at the origin, closed (64 points and the first
/// again).
fn silhouette(radius: f32, eye: Vec3) -> Vec<Vec3> {
    let d2 = eye.length_squared().max(radius * radius * 1.0001);
    let centre = eye * (radius * radius / d2);
    let rho = radius * (1.0 - radius * radius / d2).sqrt();
    let axis = eye.normalize_or(Vec3::Z);
    let a = axis.any_orthonormal_vector();
    let b = axis.cross(a);
    (0..=64)
        .map(|k| {
            #[allow(clippy::cast_precision_loss, reason = "64 samples")]
            let t = std::f32::consts::TAU * (k % 64) as f32 / 64.0;
            centre + (a * t.cos() + b * t.sin()) * rho
        })
        .collect()
}

/// `line` with five more points on each of its segments.
fn dense(line: &[Vec2]) -> Vec<Vec2> {
    let mut out = Vec::with_capacity(line.len() * 6);
    for pair in line.windows(2) {
        for k in 0..6 {
            #[allow(clippy::cast_precision_loss, reason = "six steps")]
            out.push(pair[0].lerp(pair[1], k as f32 / 6.0));
        }
    }
    out.extend(line.last());
    out
}

/// The mean distance from each point of `from` to the nearest point of `to`.
fn mean_nearest(from: &[Vec2], to: &[Vec2]) -> f32 {
    if from.is_empty() || to.is_empty() {
        return 0.0;
    }
    let sum: f32 =
        from.iter().map(|p| to.iter().map(|q| p.distance_squared(*q)).fold(f32::INFINITY, f32::min).sqrt()).sum();
    #[allow(clippy::cast_precision_loss, reason = "a few hundred points")]
    let n = from.len() as f32;
    sum / n
}

/// One parameter the fit moves: how to read and write it, its first step and its range.
struct Knob {
    get: fn(&EyeDesign, &Shot) -> f32,
    set: fn(&mut EyeDesign, &mut Shot, f32),
    step: f32,
    range: (f32, f32),
    /// Whether it is part of the eye's shape (held near where it started) rather than the camera or the gaze.
    shape: bool,
}

/// How hard the fit holds the eye's shape near where it started: pixels for moving one shape parameter across its whole
/// range.
const SHAPE_PRIOR: f32 = 8.0;

const KNOBS: [Knob; 19] = [
    Knob { get: |_, s| s.camera[0], set: |_, s, v| s.camera[0] = v, step: 0.002, range: (-0.2, 0.2), shape: false },
    Knob { get: |_, s| s.camera[1], set: |_, s, v| s.camera[1] = v, step: 0.002, range: (-0.2, 0.2), shape: false },
    Knob { get: |_, s| s.camera[2], set: |_, s, v| s.camera[2] = v, step: 0.003, range: (0.02, 0.3), shape: false },
    Knob { get: |_, s| s.look[0], set: |_, s, v| s.look[0] = v, step: 0.001, range: (-0.02, 0.02), shape: false },
    Knob { get: |_, s| s.look[1], set: |_, s, v| s.look[1] = v, step: 0.001, range: (-0.02, 0.02), shape: false },
    Knob { get: |_, s| s.roll, set: |_, s, v| s.roll = v, step: 3.0, range: (-60.0, 60.0), shape: false },
    Knob { get: |_, s| s.gaze[0], set: |_, s, v| s.gaze[0] = v, step: 0.05, range: (-0.8, 0.8), shape: false },
    Knob { get: |_, s| s.gaze[1], set: |_, s, v| s.gaze[1] = v, step: 0.05, range: (-0.8, 0.8), shape: false },
    Knob {
        get: |d, _| d.aperture.inner_x,
        set: |d, _, v| d.aperture.inner_x = v,
        step: 0.001,
        range: (0.008, 0.018),
        shape: true,
    },
    Knob {
        get: |d, _| d.aperture.outer_x,
        set: |d, _, v| d.aperture.outer_x = v,
        step: 0.001,
        range: (-0.018, -0.008),
        shape: true,
    },
    Knob {
        get: |d, _| d.aperture.inner_y,
        set: |d, _, v| d.aperture.inner_y = v,
        step: 0.001,
        range: (-0.006, 0.006),
        shape: true,
    },
    Knob {
        get: |d, _| d.aperture.outer_y,
        set: |d, _, v| d.aperture.outer_y = v,
        step: 0.001,
        range: (-0.006, 0.006),
        shape: true,
    },
    Knob {
        get: |d, _| d.aperture.upper,
        set: |d, _, v| d.aperture.upper = v,
        step: 0.000_8,
        range: (0.0005, 0.012),
        shape: true,
    },
    Knob {
        get: |d, _| d.aperture.upper_peak,
        set: |d, _, v| d.aperture.upper_peak = v,
        step: 0.05,
        range: (0.15, 0.85),
        shape: true,
    },
    Knob {
        get: |d, _| d.aperture.lower,
        set: |d, _, v| d.aperture.lower = v,
        step: 0.000_8,
        range: (0.0005, 0.008),
        shape: true,
    },
    Knob {
        get: |d, _| d.aperture.lower_peak,
        set: |d, _, v| d.aperture.lower_peak = v,
        step: 0.05,
        range: (0.15, 0.85),
        shape: true,
    },
    Knob {
        get: |d, _| d.ball.iris_radius,
        set: |d, _, v| d.ball.iris_radius = v,
        step: 0.000_3,
        range: (0.003, 0.008),
        shape: true,
    },
    Knob { get: |d, _| d.ball.pupil, set: |d, _, v| d.ball.pupil = v, step: 0.02, range: (0.05, 0.8), shape: true },
    Knob {
        get: |d, _| d.ball.radius,
        set: |d, _, v| d.ball.radius = v,
        step: 0.000_3,
        range: (0.010_5, 0.013_5),
        shape: true,
    },
];

/// The parameters the fit moves, read from a design and a shot.
type Params = [f32; KNOBS.len()];

/// What the fit searches over: the design and the shot it started from, and the landmarks.
struct Search<'a> {
    design: &'a EyeDesign,
    shot: &'a Shot,
    marks: &'a Landmarks,
}

impl Search<'_> {
    fn read(d: &EyeDesign, s: &Shot) -> Params {
        std::array::from_fn(|i| (KNOBS[i].get)(d, s))
    }

    /// The design and shot `params` make, each parameter inside its range and the iris under 0.7 of the eyeball's
    /// radius.
    fn write(&self, params: &Params) -> (EyeDesign, Shot) {
        let (mut d, mut s) = (self.design.clone(), self.shot.clone());
        for (knob, v) in KNOBS.iter().zip(params) {
            (knob.set)(&mut d, &mut s, v.clamp(knob.range.0, knob.range.1));
        }
        d.ball.iris_radius = d.ball.iris_radius.min(0.7 * d.ball.radius - 1e-5);
        (d, s)
    }

    /// `params` as they will be written, and their error; `None` for a shot `render.eye` would refuse.
    fn eval(&self, params: &Params) -> Option<(Params, f32)> {
        let (d, s) = self.write(params);
        s.check().ok()?;
        let params = Self::read(&d, &s);
        Some((params, residuals(&d, &s, self.marks).total() + self.prior(&params)))
    }

    /// The pull back toward the design the fit started from, pixels: one picture leaves the eye's shape and the
    /// camera partly trading off against each other, and without it the fit bends the shape to suit the one view
    /// (an inner corner tucked in and dropped, say), which reads wrong from every other side.
    fn prior(&self, params: &Params) -> f32 {
        let start = Self::read(self.design, self.shot);
        KNOBS
            .iter()
            .zip(params.iter().zip(&start))
            .filter(|(knob, _)| knob.shape)
            .map(|(knob, (v, s))| ((v - s) / (knob.range.1 - knob.range.0)).powi(2))
            .sum::<f32>()
            * SHAPE_PRIOR
    }

    /// One pass over the parameters from `x`: each takes a step either way if that lowers the error.
    fn explore(&self, mut x: Params, mut fx: f32, steps: &Params) -> (Params, f32) {
        for (i, step) in steps.iter().enumerate() {
            for sign in [1.0, -1.0] {
                let mut y = x;
                y[i] += sign * step;
                if let Some((y, fy)) = self.eval(&y)
                    && fy < fx - 1e-5
                {
                    (x, fx) = (y, fy);
                    break;
                }
            }
        }
        (x, fx)
    }
}

/// Fits `design`'s opening, iris and pupil and the shot (starting from `start`) to `marks`, in at most `rounds` passes
/// over the parameters. The fitted design stays inside its ranges; the iris stays under 0.7 of the eyeball's radius.
///
/// The search is a pattern search (Hooke and Jeeves): a pass over the parameters one at a time, then a jump along the
/// way the pass went while that keeps helping, the steps shrinking when nothing helps; it starts over with full steps
/// from where it settled until a start gains nothing, which walks it out of the long valleys the camera's position,
/// aim and roll make together.
pub fn fit(design: &EyeDesign, start: &Shot, marks: &Landmarks, rounds: u32) -> Fitted {
    let search = Search { design, shot: start, marks };
    let before = residuals(design, start, marks);
    let mut x = Search::read(design, start);
    let mut fx = search.eval(&x).map_or(f32::INFINITY, |(_, f)| f);
    let initial: Params = std::array::from_fn(|i| KNOBS[i].step);
    let mut passes = 0;
    while passes < rounds {
        let settled = fx;
        let mut steps = initial;
        while passes < rounds && steps.iter().zip(&initial).any(|(s, i)| *s > i * 1e-3) {
            passes += 1;
            let (mut y, mut fy) = search.explore(x, fx, &steps);
            if fy < fx - 1e-5 {
                // Jump along the way the pass went, and keep going while it helps.
                while passes < rounds {
                    let pattern: Params = std::array::from_fn(|i| 2.0 * y[i] - x[i]);
                    (x, fx) = (y, fy);
                    let Some((p, fp)) = search.eval(&pattern) else { break };
                    passes += 1;
                    let (z, fz) = search.explore(p, fp, &steps);
                    if fz < fx - 1e-5 {
                        (y, fy) = (z, fz);
                    } else {
                        break;
                    }
                }
                (x, fx) = if fy < fx { (y, fy) } else { (x, fx) };
            } else {
                for s in &mut steps {
                    *s *= 0.5;
                }
            }
        }
        if fx > settled - 1e-3 {
            break;
        }
    }
    let (d, s) = search.write(&x);
    let residuals = residuals(&d, &s, marks);
    Fitted { design: d, shot: s, residuals, before }
}
