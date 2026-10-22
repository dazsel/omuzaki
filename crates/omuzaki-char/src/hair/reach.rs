//! How far the hair stands off the head in every direction, published by the `hair` part as [`HairReachFact`] so that
//! what sits on the head (headphones, hats, clips) clears the hair it was built over instead of a fixed guess.
//!
//! Directions from the head's centre are binned on a latitude-longitude grid ([`ROWS`] from the crown down, [`COLUMNS`]
//! round); each cell keeps the furthest any hair vertex in it stands above the head's surface. A query takes the
//! largest of the cell its direction falls in and the eight around it, so a direction between two cells never misses
//! a lock in either.

use std::f32::consts::{PI, TAU};

use omuzaki_char_api::HeadSurface;
use omuzaki_core::glam::Vec3;

/// Latitude bands, crown to chin.
pub const ROWS: usize = 18;
/// Longitude bands round the head.
pub const COLUMNS: usize = 36;

/// The hair's height above the head, by direction from the head's centre.
#[derive(Clone, Debug, PartialEq)]
pub struct HairReach {
    centre: Vec3,
    /// Metres above the head's surface, `ROWS` rows of `COLUMNS`, 0 where no hair is.
    cells: Vec<f32>,
}

impl HairReach {
    /// The reach of `points` (every vertex of the hair's meshes, bind space) over `head`.
    pub fn from_points<'a>(head: &dyn HeadSurface, points: impl IntoIterator<Item = &'a Vec3>) -> Self {
        let centre = head.centre();
        let mut cells = vec![0.0f32; ROWS * COLUMNS];
        for p in points {
            let d = *p - centre;
            let length = d.length();
            if !(length.is_finite() && length > 0.0) {
                continue;
            }
            let dir = d / length;
            let above = length - head.radius(dir);
            if above.is_finite() && above > 0.0 {
                let (row, column) = cell(dir);
                let slot = &mut cells[row * COLUMNS + column];
                *slot = slot.max(above);
            }
        }
        Self { centre, cells }
    }

    /// How far the hair stands above the head's surface around `dir` (from the head's centre), metres: 0 where there
    /// is none.
    pub fn above(&self, dir: Vec3) -> f32 {
        let Some(dir) = dir.try_normalize() else { return 0.0 };
        let (row, column) = cell(dir);
        let mut most = 0.0f32;
        for r in row.saturating_sub(1)..=(row + 1).min(ROWS - 1) {
            for c in [column + COLUMNS - 1, column, column + 1] {
                most = most.max(self.cells[r * COLUMNS + c % COLUMNS]);
            }
        }
        most
    }

    /// The head centre the directions are measured from.
    pub fn centre(&self) -> Vec3 {
        self.centre
    }
}

/// The grid cell of the unit direction `dir`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small indices
fn cell(dir: Vec3) -> (usize, usize) {
    let polar = dir.y.clamp(-1.0, 1.0).acos();
    let azimuth = dir.x.atan2(dir.z).rem_euclid(TAU);
    let row = ((polar / PI * ROWS as f32) as usize).min(ROWS - 1);
    let column = ((azimuth / TAU * COLUMNS as f32) as usize).min(COLUMNS - 1);
    (row, column)
}

omuzaki_char_api::fact! {
    /// How far the hair stands off the head by direction ([`HairReach`]), published by `hair`; the last hair part
    /// built wins. Parts that sit on the head read it to clear the hair; with no hair there is none.
    pub HairReachFact = "hair_reach", omuzaki_char_api::Last<HairReach>;
}

omuzaki_char_api::register_fact!(HairReachFact);
// Compared with `==`: a centre and a grid of plain floats.
omuzaki_char_api::register_fact_probe!(HairReachFact);
