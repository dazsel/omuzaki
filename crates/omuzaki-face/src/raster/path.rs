//! A reusable path buffer: the outline a fill, a clip or a stroke hands to the scan converter.

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::PathEl;

use super::geom::Conic;
use super::scan::Scan;

/// What a segment of an [`Outline`] is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Verb {
    Move,
    Line,
    Quad,
    Conic,
    Cubic,
    Close,
}

/// Verbs with their points (a move or line has one, a quad or conic two, a cubic three) and the weights of its conics.
/// `clear` keeps the memory, so refilling an outline of the same size allocates nothing.
#[derive(Debug, Default)]
pub(super) struct Outline {
    verbs: Vec<Verb>,
    points: Vec<Vec2>,
    weights: Vec<f32>,
}

impl Outline {
    pub fn clear(&mut self) {
        self.verbs.clear();
        self.points.clear();
        self.weights.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.verbs.is_empty()
    }

    /// Starts a contour; two moves in a row leave one, at the second point.
    pub fn move_to(&mut self, p: Vec2) {
        if self.verbs.last() == Some(&Verb::Move) {
            if let Some(last) = self.points.last_mut() {
                *last = p;
            }
        } else {
            self.verbs.push(Verb::Move);
            self.points.push(p);
        }
    }

    pub fn line_to(&mut self, p: Vec2) {
        self.verbs.push(Verb::Line);
        self.points.push(p);
    }

    pub fn quad_to(&mut self, c: Vec2, p: Vec2) {
        self.verbs.push(Verb::Quad);
        self.points.extend([c, p]);
    }

    /// A conic from the pen through control point `c` to `p`, as Skia's `conicTo` stores it: a weight of 1 is a quad, a
    /// weight that is not positive is a line, and one that is not finite is two lines through the control point.
    #[allow(clippy::float_cmp)] // Skia tests the weight against exactly 1
    pub fn conic_to(&mut self, c: Vec2, p: Vec2, weight: f32) {
        if weight.is_nan() || weight <= 0.0 {
            self.line_to(p);
        } else if !weight.is_finite() {
            self.line_to(c);
            self.line_to(p);
        } else if weight == 1.0 {
            self.quad_to(c, p);
        } else {
            self.verbs.push(Verb::Conic);
            self.points.extend([c, p]);
            self.weights.push(weight);
        }
    }

    pub fn cubic_to(&mut self, c1: Vec2, c2: Vec2, p: Vec2) {
        self.verbs.push(Verb::Cubic);
        self.points.extend([c1, c2, p]);
    }

    /// Closes the contour; a close first in the path or right after another is dropped.
    pub fn close(&mut self) {
        if !self.verbs.is_empty() && self.verbs.last() != Some(&Verb::Close) {
            self.verbs.push(Verb::Close);
        }
    }

    pub fn last_point(&self) -> Option<Vec2> {
        self.points.last().copied()
    }

    pub fn set_last_point(&mut self, p: Vec2) {
        match self.points.last_mut() {
            Some(last) => *last = p,
            None => self.move_to(p),
        }
    }

    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// Whether every point from index `start` on is the same point.
    pub fn is_zero_length_since_point(&self, start: usize) -> bool {
        let Some(&first) = self.points.get(start) else { return true };
        self.points[start..].iter().all(|&p| p == first)
    }

    /// Appends all of `other`.
    pub fn append(&mut self, other: &Outline) {
        self.verbs.extend_from_slice(&other.verbs);
        self.points.extend_from_slice(&other.points);
        self.weights.extend_from_slice(&other.weights);
    }

    /// Appends the last contour of `other` backwards, from its last point (which the pen is assumed to be at) to its first.
    pub fn reverse_path_to(&mut self, other: &Outline) {
        if other.is_empty() {
            return;
        }
        let mut at = other.points.len() - 1;
        let mut weight = other.weights.len();
        for verb in other.verbs.iter().rev() {
            match verb {
                Verb::Move => break,
                Verb::Line => {
                    at -= 1;
                    self.line_to(other.points[at]);
                }
                Verb::Quad => {
                    at -= 2;
                    self.quad_to(other.points[at + 1], other.points[at]);
                }
                Verb::Conic => {
                    at -= 2;
                    weight -= 1;
                    self.conic_to(other.points[at + 1], other.points[at], other.weights[weight]);
                }
                Verb::Cubic => {
                    at -= 3;
                    self.cubic_to(other.points[at + 2], other.points[at + 1], other.points[at]);
                }
                Verb::Close => {}
            }
        }
    }

    /// Replaces the contents with a face-plane path. Drawing with no contour open (at the start, or after a close) starts
    /// one at the last move, the origin before any, as Skia does.
    pub fn set_from_elements(&mut self, els: &[PathEl]) {
        self.clear();
        let (mut last_move, mut open) = (Vec2::ZERO, false);
        for el in els {
            if !open && !matches!(el, PathEl::Move(_) | PathEl::Close) {
                self.move_to(last_move);
                open = true;
            }
            match *el {
                PathEl::Move(p) => {
                    self.move_to(p);
                    last_move = p;
                    open = true;
                }
                PathEl::Line(p) => self.line_to(p),
                PathEl::Quad(c, p) => self.quad_to(c, p),
                PathEl::Cubic(c1, c2, p) => self.cubic_to(c1, c2, p),
                PathEl::Close => {
                    self.close();
                    open = false;
                }
            }
        }
    }

    /// Feeds every segment, mapped through `map`, to the scan converter. Each contour is closed implicitly, as a fill's is.
    pub fn feed(&self, map: Affine2, scan: &mut Scan) {
        let at = |i: usize| map.transform_point2(self.points[i]);
        let (mut start, mut last) = (Vec2::ZERO, Vec2::ZERO);
        let (mut open, mut next_point, mut next_weight) = (false, 0, 0);
        for verb in &self.verbs {
            match verb {
                Verb::Move => {
                    if open {
                        scan.line(last, start);
                    }
                    start = at(next_point);
                    last = start;
                    open = true;
                    next_point += 1;
                }
                Verb::Line => {
                    let p = at(next_point);
                    scan.line(last, p);
                    last = p;
                    next_point += 1;
                }
                Verb::Quad => {
                    let (c, p) = (at(next_point), at(next_point + 1));
                    scan.quad([last, c, p]);
                    last = p;
                    next_point += 2;
                }
                Verb::Conic => {
                    let (c, p) = (at(next_point), at(next_point + 1));
                    scan.conic(&Conic { points: [last, c, p], weight: self.weights[next_weight] });
                    last = p;
                    next_point += 2;
                    next_weight += 1;
                }
                Verb::Cubic => {
                    let (c1, c2, p) = (at(next_point), at(next_point + 1), at(next_point + 2));
                    scan.cubic([last, c1, c2, p]);
                    last = p;
                    next_point += 3;
                }
                Verb::Close => {
                    if open {
                        scan.line(last, start);
                        last = start;
                    }
                }
            }
        }
        if open {
            scan.line(last, start);
        }
    }
}

/// One segment of an [`Outline`], for tests that compare outlines.
#[cfg(test)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) enum Seg {
    Move(Vec2),
    Line(Vec2),
    Quad(Vec2, Vec2),
    Conic(Vec2, Vec2, f32),
    Cubic(Vec2, Vec2, Vec2),
    Close,
}

#[cfg(test)]
impl Outline {
    /// The segments in order.
    pub fn segments(&self) -> Vec<Seg> {
        let (mut p, mut w) = (0, 0);
        let mut out = Vec::new();
        for verb in &self.verbs {
            out.push(match verb {
                Verb::Move => {
                    p += 1;
                    Seg::Move(self.points[p - 1])
                }
                Verb::Line => {
                    p += 1;
                    Seg::Line(self.points[p - 1])
                }
                Verb::Quad => {
                    p += 2;
                    Seg::Quad(self.points[p - 2], self.points[p - 1])
                }
                Verb::Conic => {
                    p += 2;
                    w += 1;
                    Seg::Conic(self.points[p - 2], self.points[p - 1], self.weights[w - 1])
                }
                Verb::Cubic => {
                    p += 3;
                    Seg::Cubic(self.points[p - 3], self.points[p - 2], self.points[p - 1])
                }
                Verb::Close => Seg::Close,
            });
        }
        out
    }
}
