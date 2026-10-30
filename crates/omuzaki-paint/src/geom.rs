//! Shapes a painter fills (SVG path data, polygons, smooth blobs, ellipses, rounded rectangles) and the paint that
//! fills them (a colour or a gradient).

use kurbo::{BezPath, Ellipse, Point, RoundedRect, Shape as _};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::colour::{Colour, Px};
use crate::error::PaintError;

/// A closed region of the canvas, in pixels from the top left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Geometry {
    /// SVG path data (`M 10 10 C 40 0 60 0 90 10 L 90 60 Z`): lines, curves and arcs, any number of subpaths.
    Path {
        /// The path data, as in an SVG `d` attribute.
        d: String,
    },
    /// Straight lines through the points, closed.
    Polygon {
        /// The corners, `[x, y]`.
        points: Vec<[f64; 2]>,
    },
    /// A smooth closed curve through the points (a Catmull-Rom spline): hair locks, blobs, shadow shapes.
    Spline {
        /// The points it passes through, `[x, y]`.
        points: Vec<[f64; 2]>,
    },
    /// An ellipse.
    Ellipse {
        /// Centre x.
        cx: f64,
        /// Centre y.
        cy: f64,
        /// Radius along its own x axis.
        rx: f64,
        /// Radius along its own y axis.
        ry: f64,
        /// Turn, in degrees clockwise on screen.
        #[serde(default)]
        angle: f64,
    },
    /// A rectangle, optionally with round corners.
    Rect {
        /// Left.
        x: f64,
        /// Top.
        y: f64,
        /// Width.
        w: f64,
        /// Height.
        h: f64,
        /// Corner radius.
        #[serde(default)]
        radius: f64,
    },
}

/// The most points a polygon or spline may have.
pub const MAX_SHAPE_POINTS: usize = 100_000;
/// The longest SVG path data, in bytes.
pub const MAX_PATH_DATA: usize = 1 << 20;

fn finite(field: &str, v: f64) -> Result<f64, PaintError> {
    if v.is_finite() && v.abs() <= 1e6 {
        Ok(v)
    } else {
        Err(PaintError::invalid(field, format!("{v} is not a usable number")))
    }
}

impl Geometry {
    /// The region as a path.
    ///
    /// # Errors
    /// [`PaintError::Invalid`] for bad path data, too few or too many points, or a non-finite number, under `field`.
    pub fn to_path(&self, field: &str) -> Result<BezPath, PaintError> {
        match self {
            Self::Path { d } => {
                if d.len() > MAX_PATH_DATA {
                    return Err(PaintError::invalid(
                        format!("{field}/d"),
                        format!("longer than {MAX_PATH_DATA} bytes"),
                    ));
                }
                let p = BezPath::from_svg(d).map_err(|e| PaintError::invalid(format!("{field}/d"), e.to_string()))?;
                let b = p.bounding_box();
                for v in [b.x0, b.y0, b.x1, b.y1] {
                    finite(&format!("{field}/d"), v)?;
                }
                Ok(p)
            }
            Self::Polygon { points } => {
                let pts = check_points(&format!("{field}/points"), points, 3)?;
                let mut p = BezPath::new();
                p.move_to(pts[0]);
                for &q in &pts[1..] {
                    p.line_to(q);
                }
                p.close_path();
                Ok(p)
            }
            Self::Spline { points } => {
                let pts = check_points(&format!("{field}/points"), points, 3)?;
                Ok(closed_spline(&pts))
            }
            Self::Ellipse { cx, cy, rx, ry, angle } => {
                for (n, v) in [("cx", cx), ("cy", cy), ("rx", rx), ("ry", ry), ("angle", angle)] {
                    finite(&format!("{field}/{n}"), *v)?;
                }
                if *rx <= 0.0 || *ry <= 0.0 {
                    return Err(PaintError::invalid(field, "rx and ry must be above 0"));
                }
                let e = Ellipse::new((*cx, *cy), (*rx, *ry), angle.to_radians());
                Ok(e.to_path(0.05))
            }
            Self::Rect { x, y, w, h, radius } => {
                for (n, v) in [("x", x), ("y", y), ("w", w), ("h", h), ("radius", radius)] {
                    finite(&format!("{field}/{n}"), *v)?;
                }
                if *w <= 0.0 || *h <= 0.0 || *radius < 0.0 {
                    return Err(PaintError::invalid(field, "w and h must be above 0 and radius not below 0"));
                }
                Ok(RoundedRect::new(*x, *y, x + w, y + h, *radius).to_path(0.05))
            }
        }
    }
}

fn check_points(field: &str, points: &[[f64; 2]], min: usize) -> Result<Vec<Point>, PaintError> {
    if points.len() < min || points.len() > MAX_SHAPE_POINTS {
        return Err(PaintError::invalid(field, format!("{} points; give {min} to {MAX_SHAPE_POINTS}", points.len())));
    }
    points
        .iter()
        .enumerate()
        .map(|(i, p)| Ok(Point::new(finite(&format!("{field}/{i}"), p[0])?, finite(&format!("{field}/{i}"), p[1])?)))
        .collect()
}

/// A closed uniform Catmull-Rom spline through `pts`, as cubic Béziers.
fn closed_spline(pts: &[Point]) -> BezPath {
    let n = pts.len();
    let at = |i: usize| pts[i % n];
    let mut p = BezPath::new();
    p.move_to(pts[0]);
    for i in 0..n {
        let (p0, p1, p2, p3) = (at(i + n - 1), at(i), at(i + 1), at(i + 2));
        let c1 = p1 + (p2 - p0) / 6.0;
        let c2 = p2 - (p3 - p1) / 6.0;
        p.curve_to(c1, c2, p2);
    }
    p.close_path();
    p
}

/// Which way a gradient runs.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GradientKind {
    /// Along the line from `from` to `to`.
    #[default]
    Linear,
    /// Out from `from` (the centre) to the circle through `to`.
    Radial,
}

/// One colour of a gradient.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Stop {
    /// Where along the gradient, 0 to 1.
    pub at: f64,
    /// The colour there.
    pub colour: Colour,
}

/// A gradient: colours blended between stops along a line or out from a centre (sRGB, as painters blend).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Gradient {
    /// Linear or radial.
    #[serde(default)]
    pub kind: GradientKind,
    /// The start (linear) or the centre (radial), `[x, y]`.
    pub from: [f64; 2],
    /// The end (linear) or a point on the outer circle (radial), `[x, y]`.
    pub to: [f64; 2],
    /// Two or more stops, in order of `at`.
    pub stops: Vec<Stop>,
}

/// The most stops a gradient may have.
pub const MAX_STOPS: usize = 64;

impl Gradient {
    /// Checks the gradient can be drawn.
    ///
    /// # Errors
    /// [`PaintError::Invalid`] under `field` for fewer than 2 or more than [`MAX_STOPS`] stops, stops out of order or
    /// out of 0..1, or a zero-length gradient.
    pub fn check(&self, field: &str) -> Result<(), PaintError> {
        if self.stops.len() < 2 || self.stops.len() > MAX_STOPS {
            return Err(PaintError::invalid(format!("{field}/stops"), format!("give 2 to {MAX_STOPS} stops")));
        }
        let mut last = 0.0;
        for (i, s) in self.stops.iter().enumerate() {
            if !(0.0..=1.0).contains(&s.at) || s.at < last {
                return Err(PaintError::invalid(format!("{field}/stops/{i}/at"), "stops go from 0 to 1, in order"));
            }
            last = s.at;
        }
        for v in self.from.iter().chain(&self.to) {
            finite(field, *v)?;
        }
        if (self.to[0] - self.from[0]).hypot(self.to[1] - self.from[1]) < 1e-6 {
            return Err(PaintError::invalid(field, "from and to are the same point"));
        }
        Ok(())
    }

    /// The colour at canvas point `(x, y)`, premultiplied.
    pub fn at(&self, x: f64, y: f64) -> Px {
        let (dx, dy) = (self.to[0] - self.from[0], self.to[1] - self.from[1]);
        let t = match self.kind {
            GradientKind::Linear => ((x - self.from[0]) * dx + (y - self.from[1]) * dy) / (dx * dx + dy * dy),
            GradientKind::Radial => (x - self.from[0]).hypot(y - self.from[1]) / dx.hypot(dy),
        }
        .clamp(0.0, 1.0);
        let i = self.stops.iter().position(|s| s.at >= t).unwrap_or(self.stops.len() - 1);
        if i == 0 {
            return self.stops[0].colour.premul();
        }
        let (a, b) = (&self.stops[i - 1], &self.stops[i]);
        let k = if b.at > a.at { (t - a.at) / (b.at - a.at) } else { 1.0 };
        #[allow(clippy::cast_possible_truncation)] // 0..1
        let k = k as f32;
        let (pa, pb) = (a.colour.premul(), b.colour.premul());
        std::array::from_fn(|c| pa[c] + (pb[c] - pa[c]) * k)
    }
}

/// What fills a shape or a bucket fill: a colour (`"#rrggbb"`) or a gradient (an object).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Fill {
    /// One colour.
    Colour(Colour),
    /// A gradient.
    Gradient(Gradient),
}

impl Fill {
    /// Checks the fill can be drawn.
    ///
    /// # Errors
    /// As [`Gradient::check`].
    pub fn check(&self, field: &str) -> Result<(), PaintError> {
        match self {
            Self::Colour(_) => Ok(()),
            Self::Gradient(g) => g.check(field),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_makes_a_path() {
        let shapes = [
            Geometry::Path { d: "M 10 10 C 40 0 60 0 90 10 L 90 60 Z".into() },
            Geometry::Polygon { points: vec![[0.0, 0.0], [10.0, 0.0], [5.0, 8.0]] },
            Geometry::Spline { points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]] },
            Geometry::Ellipse { cx: 5.0, cy: 5.0, rx: 4.0, ry: 2.0, angle: 30.0 },
            Geometry::Rect { x: 0.0, y: 0.0, w: 10.0, h: 5.0, radius: 2.0 },
        ];
        for s in shapes {
            assert!(s.to_path("g").unwrap().elements().len() > 1);
        }
        assert!(Geometry::Path { d: "M 10 10 Q".into() }.to_path("g").is_err());
        assert!(Geometry::Polygon { points: vec![[0.0, 0.0], [1.0, f64::NAN], [2.0, 2.0]] }.to_path("g").is_err());
    }

    #[test]
    fn gradients_run_from_stop_to_stop() {
        let g = Gradient {
            kind: GradientKind::Linear,
            from: [0.0, 0.0],
            to: [100.0, 0.0],
            stops: vec![Stop { at: 0.0, colour: Colour::BLACK }, Stop { at: 1.0, colour: Colour::WHITE }],
        };
        g.check("g").unwrap();
        assert!((g.at(50.0, 30.0)[0] - 0.5).abs() < 1e-5);
        assert_eq!(g.at(-10.0, 0.0), Colour::BLACK.premul());
        assert_eq!(g.at(200.0, 0.0), Colour::WHITE.premul());
    }

    #[test]
    fn a_fill_reads_as_a_colour_or_a_gradient() {
        let c: Fill = serde_json::from_str("\"#ff0000\"").unwrap();
        assert_eq!(c, Fill::Colour(Colour::rgb(255, 0, 0)));
        let g: Fill = serde_json::from_str(
            r##"{"kind":"radial","from":[0,0],"to":[5,0],"stops":[{"at":0,"colour":"#ffffff"},{"at":1,"colour":"#000000"}]}"##,
        )
        .unwrap();
        assert!(matches!(g, Fill::Gradient(_)));
    }
}
