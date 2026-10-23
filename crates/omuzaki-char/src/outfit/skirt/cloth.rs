//! The skirt's cloth: the sheet it drapes on when [`ClothSpec::enabled`](crate::outfit::common::ClothSpec) is on.
//!
//! The pleats' outer skin is the grid, the waist row pinned; the inner skin and the rolled hem ride on it
//! ([`drape`](crate::outfit::drape)).

use crate::outfit::drape::Drape;

/// Grid rows from the waist that stay on the hips: the waistband.
pub const PIN_ROWS: usize = 1;

/// Where the sheet lies in the skirt's cloth mesh (mesh 0): its outer skin, `rows x cols` from the waist down.
pub fn drape(rows: usize, cols: usize) -> Drape {
    Drape { mesh: 0, start: 0, rows, cols, pinned_first: true, pin_rows: PIN_ROWS }
}
