//! Whether the hair stays out of the body (#2485): after the simulation has hung the hair at rest (`hair.settle`), how
//! deep any of its points lies inside the body or the head.
//!
//! The body is the fit the body part publishes, sampled once ([`BodyDepth`]: the distance outside along the ray from the
//! nearest section's centre, an over-estimate, so a point it calls outside is outside). The head is the head surface,
//! along the ray from its centre. A point counts as clipping when it lies deeper than [`TOLERANCE`]: a lock's inner face
//! resting on a collider sphere touches the skin, and a fraction of a millimetre of that is the mesh's own chord.

use omuzaki_char_api::{BodyFit, HeadSurface};
use omuzaki_core::glam::Vec3;

use crate::cloth::collide::BodyDepth;

/// How deep, metres, a point may sit inside before it counts as clipping.
pub const TOLERANCE: f32 = 0.002;

/// What [`Clearance::measure`] found.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Clipping {
    /// The deepest any point lies inside the body, metres (0 when none does).
    pub body: f32,
    /// The deepest any point lies inside the head, metres (0 when none does).
    pub head: f32,
    /// How many points lie deeper than [`TOLERANCE`] in either.
    pub points: usize,
    /// How many points were measured.
    pub of: usize,
}

impl Clipping {
    /// Whether no point lies deeper than [`TOLERANCE`].
    pub fn clear(&self) -> bool {
        self.points == 0
    }
}

/// The body and head a hair is checked against.
pub struct Clearance<'a> {
    body: Option<BodyDepth>,
    head: &'a dyn HeadSurface,
}

impl<'a> Clearance<'a> {
    /// Checks against `head` and, when there is one, the body `fit` (sampled once here).
    pub fn new(head: &'a dyn HeadSurface, fit: Option<&dyn BodyFit>) -> Self {
        Self { body: fit.map(BodyDepth::new), head }
    }

    /// How deep `points` (bind space, the character at rest) lie inside the body and the head.
    pub fn measure(&self, points: &[Vec3]) -> Clipping {
        let centre = self.head.centre();
        let mut out = Clipping { of: points.len(), ..Clipping::default() };
        for p in points {
            let body = self.body.as_ref().map_or(0.0, |b| (-b.depth(*p)).max(0.0));
            let d = *p - centre;
            let head = d.try_normalize().map_or(0.0, |u| (self.head.radius(u) - d.length()).max(0.0));
            out.body = out.body.max(body);
            out.head = out.head.max(head);
            if body > TOLERANCE || head > TOLERANCE {
                out.points += 1;
            }
        }
        out
    }
}
