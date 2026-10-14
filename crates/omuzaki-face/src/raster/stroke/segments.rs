//! Walks a path the way the stroker needs: Skia's segment iterator with auto-close on.
//!
//! Port of Skia (through tiny-skia). Copyright 2008 The Android Open Source Project; copyright 2020 Yevhenii Reizner.
//! BSD-3-Clause; the notice is in `raster/NOTICES.md`.

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::PathEl;

/// Walks a face-plane path as Skia's segment iterator does with auto-close on: a close first draws the line back to the
/// contour's start if the pen is elsewhere. Drawing after a close, or with no move at all, starts a contour at the last
/// move (the origin before any).
#[derive(Copy, Clone)]
pub(super) struct Segments<'a> {
    els: &'a [PathEl],
    index: usize,
    last_point: Vec2,
    last_move: Vec2,
    /// A move has been made and not yet closed.
    open: bool,
}

#[derive(Copy, Clone, Debug)]
pub(super) enum Segment {
    Move(Vec2),
    Line(Vec2),
    Quad(Vec2, Vec2),
    Cubic(Vec2, Vec2, Vec2),
    Close,
}

impl<'a> Segments<'a> {
    pub(super) fn new(els: &'a [PathEl]) -> Self {
        Self { els, index: 0, last_point: Vec2::ZERO, last_move: Vec2::ZERO, open: false }
    }

    pub(super) fn next(&mut self) -> Option<Segment> {
        if !self.open
            && let Some(el) = self.els.get(self.index)
            && !matches!(el, PathEl::Move(_) | PathEl::Close)
        {
            // Drawing needs a contour: inject the move Skia injects.
            self.open = true;
            self.last_point = self.last_move;
            return Some(Segment::Move(self.last_move));
        }
        let el = *self.els.get(self.index)?;
        match el {
            PathEl::Move(p) => {
                self.index += 1;
                self.last_move = p;
                self.last_point = p;
                self.open = true;
                Some(Segment::Move(p))
            }
            PathEl::Line(p) => {
                self.index += 1;
                self.last_point = p;
                Some(Segment::Line(p))
            }
            PathEl::Quad(c, p) => {
                self.index += 1;
                self.last_point = p;
                Some(Segment::Quad(c, p))
            }
            PathEl::Cubic(a, b, p) => {
                self.index += 1;
                self.last_point = p;
                Some(Segment::Cubic(a, b, p))
            }
            PathEl::Close => {
                if self.open && self.last_point != self.last_move {
                    self.last_point = self.last_move;
                    return Some(Segment::Line(self.last_move));
                }
                self.index += 1;
                self.open = false;
                Some(Segment::Close)
            }
        }
    }

    /// Whether the rest of this contour has a segment with a direction.
    pub(super) fn has_valid_tangent(&self) -> bool {
        let mut iter = *self;
        while let Some(segment) = iter.next() {
            match segment {
                Segment::Move(_) | Segment::Close => return false,
                Segment::Line(p) => {
                    if iter.last_point != p {
                        return true;
                    }
                }
                Segment::Quad(c, p) => {
                    if iter.last_point != c || iter.last_point != p {
                        return true;
                    }
                }
                Segment::Cubic(a, b, p) => {
                    if iter.last_point != a || iter.last_point != b || iter.last_point != p {
                        return true;
                    }
                }
            }
        }
        false
    }
}
