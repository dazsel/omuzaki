//! How far the garments below the waist stand out, by height and direction, published as [`HangReachFact`].
//!
//! A garment's envelope ([`Envelope`](crate::outfit::common::Envelope)) widens the body's regions, which is right for a
//! fitted layer; a skirt hangs away from them, and the fit says nothing of it. A skirt or a coat publishes its reach so
//! the coat, the apron or the sash built over it clears the cloth it is worn over, not the legs inside it.

use omuzaki_core::glam::{DVec2, DVec3, Vec3};

use crate::kit::curve::{TAU, fraction};

/// Directions round the body's axis.
pub const BINS: usize = 48;
/// The height of one row, metres.
pub const STEP: f64 = 0.01;
/// The most rows: a 4 m garment (heights are clamped to it).
const MAX_ROWS: usize = 400;

/// The furthest the garments stand from the body's vertical axis, by height and direction.
#[derive(Clone, Debug, PartialEq)]
pub struct HangReach {
    /// The axis: `(x, z)`.
    axis: DVec2,
    /// The first row's height.
    top: f64,
    /// `BINS` distances per row, top down; 0 where nothing is.
    rows: Vec<[f32; BINS]>,
}

/// The bin of the direction `(x, z)` from the axis: 0 at the front, rising to the left.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small indices
fn bin(d: DVec2) -> usize {
    let a = d.x.atan2(d.y).rem_euclid(TAU);
    ((a / TAU * BINS as f64) as usize).min(BINS - 1)
}

impl HangReach {
    /// The reach of `points` round the vertical axis through `axis` (`(x, z)`), with `under` (the reach of the
    /// garments inside them) taken in.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // row indices from clamped heights
    pub fn of<'a>(axis: DVec2, points: impl IntoIterator<Item = &'a Vec3>, under: Option<&Self>) -> Option<Self> {
        let mut all: Vec<DVec3> = points.into_iter().map(|p| p.as_dvec3()).collect();
        if let Some(under) = under {
            all.extend(under.points());
        }
        let top = all.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
        let bottom = all.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
        if !(top.is_finite() && bottom.is_finite()) {
            return None;
        }
        let count = (((top - bottom) / STEP).ceil() as usize + 1).min(MAX_ROWS);
        let mut rows = vec![[0.0f32; BINS]; count];
        for p in &all {
            let row = (((top - p.y) / STEP).round().max(0.0) as usize).min(count - 1);
            let d = DVec2::new(p.x, p.z) - axis;
            let slot = &mut rows[row][bin(d)];
            *slot = slot.max(d.length() as f32);
        }
        Some(Self { axis, top, rows })
    }

    /// Points `(x, z)` on the reach's outline at height `y`, from the row there and its neighbours: each bin's
    /// distance at its two edges and its middle, so a polygon through them encloses what was published.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    pub fn points_at(&self, y: f64) -> Vec<DVec2> {
        let at = (self.top - y) / STEP;
        if at < -1.0 || at > self.rows.len() as f64 {
            return Vec::new();
        }
        let row = at.round().clamp(0.0, (self.rows.len() - 1) as f64) as usize;
        let mut out = Vec::new();
        let half = TAU / BINS as f64 / 2.0;
        for b in 0..BINS {
            let r = (row.saturating_sub(1)..=(row + 1).min(self.rows.len() - 1))
                .map(|k| f64::from(self.rows[k][b]))
                .fold(0.0, f64::max);
            if r <= 0.0 {
                continue;
            }
            // Out to the bin's corners: a point at its edge angle that still reaches `r` across the bin.
            let r = r / half.cos();
            let mid = TAU * fraction(b, BINS) + half;
            for a in [mid - half, mid, mid + half] {
                out.push(self.axis + DVec2::new(a.sin(), a.cos()) * r);
            }
        }
        out
    }

    /// Every row's outline as points.
    #[allow(clippy::cast_precision_loss)]
    fn points(&self) -> Vec<DVec3> {
        let mut out = Vec::new();
        for k in 0..self.rows.len() {
            let y = self.top - STEP * k as f64;
            out.extend(self.points_at(y).into_iter().map(|q| DVec3::new(q.x, y, q.y)));
        }
        out
    }

    /// The lowest height it reaches.
    #[allow(clippy::cast_precision_loss)]
    pub fn bottom(&self) -> f64 {
        self.top - STEP * (self.rows.len().saturating_sub(1)) as f64
    }
}

omuzaki_char_api::fact! {
    /// How far the garments hanging below the waist stand out ([`HangReach`]), published by skirts, dresses and coats;
    /// each takes in the reach of the ones under it, so the last one built holds them all.
    pub HangReachFact = "hang_reach", omuzaki_char_api::Last<HangReach>;
}

omuzaki_char_api::register_fact!(HangReachFact);
// Compared with `==`: an axis and rows of plain floats.
omuzaki_char_api::register_fact_probe!(HangReachFact);
