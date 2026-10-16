//! The pull tab hanging off the back of the cuff (`boots.js` `buildPullTab`).

use omuzaki_core::glam::DVec3;

use super::Tess;
use super::profile::Boot;
use crate::kit::KitError;
use crate::kit::grid::{GridOptions, MeshBuilder, Skin};
use crate::kit::sweep::{SweepOptions, sweep};

/// Boot-space heights of the tab's path, and how far behind the boot's back it hangs at each (metres).
const HEIGHTS: [f64; 6] = [0.0805, 0.0905, 0.1015, 0.1120, 0.1220, 0.1248];
const HANG: [f64; 6] = [0.0016, 0.0030, 0.0075, 0.0090, 0.0062, 0.0020];
/// The boot-space height the tab's `x` is read at.
const CENTRE_AT: f64 = 0.11;
/// The tab's cross-section: half width (across the boot) and half thickness, metres. Constant, so its ends are open.
const SECTION: (f64, f64) = (0.0032, 0.00095);
/// The tab's leather, a little lighter than the boot.
const SHADE: [f32; 4] = [0.92, 0.88, 0.84, 1.0];

/// Adds the pull tab to `builder`: a flat loop of leather swept along its path.
pub(super) fn add(boot: &Boot, tess: &Tess, builder: &mut MeshBuilder) -> Result<(), KitError> {
    let cx = boot.secs(CENTRE_AT).cx;
    let space = boot.space();
    let points: Vec<DVec3> =
        HEIGHTS.iter().zip(HANG).map(|(&y, dz)| DVec3::new(cx, space.up(y), boot.secs(y).zb - dz)).collect();
    let swept = sweep(&points, |_| SECTION, tess.tab_seg, &SweepOptions { up: DVec3::X, ..SweepOptions::default() })?;
    let inside = |i: usize, _: usize| swept.centres[i];
    let skin = |p: DVec3, _: usize, _: usize| boot.skin(p);
    let paint = |_: usize, _: usize, _: DVec3| SHADE;
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        skin: Skin::Per(&skin),
        colour: Some(&paint),
        ..GridOptions::default()
    };
    builder.add_grid(&swept.grid, &options)
}
