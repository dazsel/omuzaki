//! Hats (CB-43): `hat`, five styles (beret, cap, witch hat, beanie, crown) built on the head's surface.
//!
//! **Build.** The crown is a thin closed shell over the head: its outer and inner faces are the head's surface pushed out
//! by the thickness and by `lift`, from the top of the head down to the rim, so the inside face rests on the head
//! within `lift` of it. The rim is a latitude `rim_deg` from the top of the head, turned by `tilt_deg` (positive lowers
//! the front). The style adds to that: a beanie's cuff and pom-pom, a beret's puff and stalk, a cap's bill, a witch
//! hat's cone and brim, a crown's spikes and gems. A `band` ring in the accent colour follows the rim.
//!
//! **The face.** The rim never reaches below the brow line at the front: `rim_deg` is limited to [`MAX_RIM_DEG`] and a
//! brim may not point down. With `brim_down` a brim may hang over the eyes (down to [`MIN_BRIM_TILT_DEG`]).
//!
//! **Hair.** A hat sits on the head, not on the hair: the hair today stands up to 5.4 cm off the head and passes through
//! the hat. Raise the hat with `lift` to sit on the hair; the hair cap leaving a gap under the hat is the hair
//! part's job (#141, #1402).
//!
//! Skinned rigidly to the head joint. With an `attach` the whole hat moves as one piece.

use std::f64::consts::{PI, TAU};

use omuzaki_char_api::{
    BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput, RecolourCtx, Recoloured, RigContribution,
};
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::rigid::{Detail, kit_error};
use super::shells::{Shells, Slot, ellipsoid, head_frames, on_head, polar, recolour};
use crate::kit::KitError;
use crate::kit::curve::{fraction, sstep};
use crate::kit::grid::Grid;

/// The lowest a hat's rim may sit, degrees from the top of the head: above the brow line on every template.
pub const MAX_RIM_DEG: f64 = 80.0;
/// How far down from the top of the head the witch hat's cone reaches, as a fraction of the rim's angle.
const WITCH_CONE: f64 = 0.45;
/// The narrowest the brim ring gets, metres: a bill has no brim at the back, and a ring of no width would fold flat.
const MIN_LIP: f64 = 0.002;
/// The steepest a brim may point down, degrees, with `brim_down`.
pub const MIN_BRIM_TILT_DEG: f64 = -50.0;

/// The style of the hat.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HatStyle {
    /// A flat, puffed cap with a stalk.
    #[default]
    Beret,
    /// A domed cap with a bill at the front.
    Cap,
    /// A tall bent cone over a wide brim.
    Witch,
    /// A close knit cap with a cuff and a pom-pom.
    Beanie,
    /// A gold band with spikes and gems.
    Crown,
}

/// What a style does when the parameters leave it open.
struct StyleDefaults {
    rim_deg: f64,
    brim: f64,
    height: f64,
}

impl HatStyle {
    fn defaults(self) -> StyleDefaults {
        match self {
            Self::Beret => StyleDefaults { rim_deg: 70.0, brim: 0.0, height: 0.03 },
            Self::Cap => StyleDefaults { rim_deg: 70.0, brim: 0.07, height: 0.012 },
            Self::Witch => StyleDefaults { rim_deg: 66.0, brim: 0.1, height: 0.26 },
            Self::Beanie => StyleDefaults { rim_deg: 72.0, brim: 0.0, height: 0.02 },
            Self::Crown => StyleDefaults { rim_deg: 62.0, brim: 0.0, height: 0.05 },
        }
    }
}

/// A hat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HatParams {
    /// The style.
    pub style: HatStyle,
    /// Where the lower edge sits, degrees from the top of the head. Left out: the style's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rim_deg: Option<f64>,
    /// Tips the hat: positive lowers the front and raises the back, degrees.
    pub tilt_deg: f64,
    /// Thickness of the crown's shell, metres.
    pub thickness: f64,
    /// How far the hat's inside stands off the head, metres. `0.0004` rests on the head; to sit on the hair, raise it.
    pub lift: f64,
    /// Width of the brim (a cap's bill, a witch hat's brim), metres; `0` for none. Left out: the style's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brim: Option<f64>,
    /// How the brim points: up (positive) or down (negative, only with `brim_down`), degrees.
    pub brim_tilt_deg: f64,
    /// How much the brim's edge curls up, `0..=1`.
    pub brim_curl: f64,
    /// Let the brim hang down over the face.
    pub brim_down: bool,
    /// The style's height: the witch cone's, a crown's spikes', a beret's puff. Left out: the style's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    /// Height of the band round the rim, metres.
    pub band_height: f64,
    /// The hat's colour.
    pub hat: ColourRef,
    /// The band's, cuff's and gems' colour.
    pub band: ColourRef,
}

impl Default for HatParams {
    fn default() -> Self {
        Self {
            style: HatStyle::Beret,
            rim_deg: None,
            tilt_deg: 0.0,
            thickness: 0.006,
            lift: 0.0004,
            brim: None,
            brim_tilt_deg: 6.0,
            brim_curl: 0.3,
            brim_down: false,
            height: None,
            band_height: 0.016,
            hat: ColourRef::rgb(0xb0, 0x3a, 0x52),
            band: ColourRef::rgb(0xe8, 0xc8, 0x70),
        }
    }
}

/// The parameters with the style's defaults filled in.
struct Resolved {
    rim: f64,
    brim: f64,
    height: f64,
}

impl HatParams {
    fn resolved(&self) -> Resolved {
        let d = self.style.defaults();
        Resolved {
            rim: self.rim_deg.unwrap_or(d.rim_deg).to_radians(),
            brim: self.brim.unwrap_or(d.brim),
            height: self.height.unwrap_or(d.height),
        }
    }
}

impl Validate for HatParams {
    fn check(&self, c: &mut Check) {
        if let Some(rim) = self.rim_deg {
            c.range("rim_deg", rim, 20.0, MAX_RIM_DEG);
        }
        c.range("tilt_deg", self.tilt_deg, -20.0, 20.0);
        c.range("thickness", self.thickness, 0.002, 0.03);
        c.range("lift", self.lift, 0.0, 0.08);
        if let Some(brim) = self.brim {
            c.range("brim", brim, 0.0, 0.25);
        }
        let lowest = if self.brim_down { MIN_BRIM_TILT_DEG } else { 0.0 };
        c.range("brim_tilt_deg", self.brim_tilt_deg, lowest, 60.0);
        c.range("brim_curl", self.brim_curl, 0.0, 1.0);
        if let Some(height) = self.height {
            c.range("height", height, 0.0, 0.5);
        }
        c.range("band_height", self.band_height, 0.0, 0.06);
        // The tilt lowers the front rim by this much, so the rim plus the tilt must stay above the brow line too.
        let front = self.rim_deg.unwrap_or_else(|| self.style.defaults().rim_deg) + self.tilt_deg.max(0.0);
        if front > MAX_RIM_DEG + 10.0 {
            c.fail(
                "tilt_deg",
                format!(
                    "the front of the rim would sit {front:.0} degrees from the top: lower the tilt or raise the rim"
                ),
            );
        }
    }
}

/// The `hat` kind.
pub struct Hat;

/// How a hat is cut: the head, the resolved parameters and the grid size.
struct Cut<'a> {
    head: &'a dyn omuzaki_char_api::HeadSurface,
    p: &'a HatParams,
    r: Resolved,
    rows: usize,
    cols: usize,
}

impl Cut<'_> {
    /// The rim's polar angle at azimuth `phi`.
    fn rim(&self, phi: f64) -> f64 {
        self.r.rim + self.p.tilt_deg.to_radians() * phi.cos()
    }

    /// The head's surface at the point `u` (0 at the top of the head, 1 at the rim) of azimuth `phi`, pushed `offset` out.
    fn surface(&self, u: f64, phi: f64, offset: f64) -> DVec3 {
        on_head(self.head, polar(u * self.rim(phi), phi), offset)
    }

    /// How far the outer face stands off the head at `u` (0 top, 1 rim) and `phi`, beyond the shell's thickness.
    fn bulge(&self, u: f64, phi: f64) -> f64 {
        let h = self.r.height;
        match self.p.style {
            HatStyle::Beret => {
                // A puffed pillow: widest a little above the rim, flat on top, tilted with the hat.
                let lean = 1.0 + 0.25 * phi.cos();
                h * lean * (PI * (0.15 + 0.85 * (1.0 - u))).sin().powf(0.8)
            }
            HatStyle::Cap => h * (1.0 - u * u),
            HatStyle::Beanie => h * (1.0 - u) * 0.5 + 0.8 * h * sstep(0.78, 0.9, u) * (1.0 - sstep(0.97, 1.0, u)),
            // The cone rises from the top of the head and ends at the pole; `shift` bends it.
            HatStyle::Witch => h * (1.0 - u / WITCH_CONE).max(0.0).powf(1.3),
            HatStyle::Crown => 0.0,
        }
    }

    /// How far the outer face is bent aside at `u` and `phi`: a witch hat's point droops back and to the side.
    fn shift(&self, u: f64, phi: f64) -> DVec3 {
        if self.p.style != HatStyle::Witch {
            return DVec3::ZERO;
        }
        let height = self.bulge(u, phi) / self.r.height.max(1e-9);
        (DVec3::X * 0.1 - DVec3::Z * 0.35) * (self.r.height * height * height)
    }

    /// The crown's shell: outer face from the top pole to the rim, then the inner face back to the pole. One closed
    /// grid, rows along the profile, columns round the head.
    fn shell(&self) -> Result<(Grid, Vec<DVec3>), KitError> {
        let m = self.rows;
        let (lift, t) = (self.p.lift, self.p.thickness);
        let cols = self.cols;
        let point = |side_out: bool, k: usize, j: usize| {
            let (u, phi) = (fraction(k, m), TAU * fraction(j, cols));
            if side_out {
                self.surface(u, phi, lift + t + self.bulge(u, phi)) + self.shift(u, phi)
            } else {
                self.surface(u, phi, lift)
            }
        };
        let source = |i: usize| if i <= m { (true, i) } else { (false, 2 * m + 1 - i) };
        let grid = Grid::from_fn(2 * (m + 1), cols + 1, |i, j| {
            let (out, k) = source(i);
            point(out, k, j)
        })?;
        // The interior reference of each vertex: the middle of the shell where it stands.
        let mids = (0..2 * (m + 1))
            .flat_map(|i| {
                let (_, k) = source(i);
                (0..=cols).map(move |j| (k, j))
            })
            .map(|(k, j)| 0.5 * (point(true, k, j) + point(false, k, j)))
            .collect();
        Ok((grid, mids))
    }

    /// A ring following the rim: the band round the crown (and a crown's own body). `edge(phi)` is its height in metres
    /// at `phi`; `out` is how far the ring stands off the surface.
    fn ring(&self, edge: impl Fn(f64) -> f64, out: f64) -> Result<(Grid, Vec<DVec3>), KitError> {
        let cols = self.cols;
        let radius = f64::from(self.head.radius(omuzaki_core::glam::Vec3::Y));
        let (lift, t) = (self.p.lift, 0.8 * self.p.thickness);
        let corner = |i: usize, phi: f64| {
            let rim = self.rim(phi);
            let up = edge(phi) / radius;
            let (theta_bottom, theta_top) = (rim, rim - up);
            let on = |theta: f64, offset: f64| on_head(self.head, polar(theta, phi), offset);
            match i % 4 {
                0 => on(theta_bottom, lift),
                1 => on(theta_bottom, lift + out + t),
                2 => on(theta_top, lift + out + t),
                _ => on(theta_top, lift),
            }
        };
        let grid = Grid::from_fn(5, cols + 1, |i, j| corner(i, TAU * fraction(j, cols)))?;
        let centres = (0..5)
            .flat_map(|_| 0..=cols)
            .map(|j| {
                let phi = TAU * fraction(j, cols);
                (0..4).map(|i| corner(i, phi)).sum::<DVec3>() / 4.0
            })
            .collect();
        Ok((grid, centres))
    }

    /// The brim: a ring from the rim outwards, `width(phi)` wide, turned by the brim's tilt and curled up at its edge.
    fn brim(&self, width: impl Fn(f64) -> f64) -> Result<(Grid, Vec<DVec3>), KitError> {
        const PROFILE: usize = 3;
        let cols = self.cols;
        let (lift, t) = (self.p.lift, 0.6 * self.p.thickness);
        let tilt = self.p.brim_tilt_deg.to_radians();
        let curl = self.p.brim_curl;
        // The loop: out along the top, round the edge, back along the bottom, and closed at the rim.
        let loop_at = |i: usize, phi: f64| {
            // The last row of a closed ring repeats the first.
            let i = i % (2 * PROFILE + 2);
            let rim = self.rim(phi);
            let root = on_head(self.head, polar(rim, phi), lift);
            let outward = DVec3::new(phi.sin(), 0.0, phi.cos());
            let w = width(phi);
            let along = |s: f64| s * w;
            let rise = |s: f64| along(s) * tilt.tan() + curl * s * s * w * 0.35;
            let (s, side) = match i {
                0 => (0.0, 1.0),
                1..=PROFILE => (fraction(i, PROFILE), 1.0),
                _ => (fraction(2 * PROFILE + 1 - i.min(2 * PROFILE + 1), PROFILE), -1.0),
            };
            root + outward * along(s) + DVec3::Y * (rise(s) + side * 0.5 * t)
        };
        let rows = 2 * PROFILE + 3;
        let grid = Grid::from_fn(rows, cols + 1, |i, j| loop_at(i, TAU * fraction(j, cols)))?;
        #[allow(clippy::cast_precision_loss)] // a handful of rows
        let count = (rows - 1) as f64;
        let centres = (0..rows)
            .flat_map(|_| 0..=cols)
            .map(|j| {
                let phi = TAU * fraction(j, cols);
                (0..rows - 1).map(|i| loop_at(i, phi)).sum::<DVec3>() / count
            })
            .collect();
        Ok((grid, centres))
    }
}

impl PartKind for Hat {
    type Params = HatParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "hat",
            summary: "Build a hat (beret, cap, witch hat, beanie or crown) on the head's surface.",
            category: PartCategory::Accessory,
            requires: &["head"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &HatParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let (attach, default) = head_frames(ctx)?;
        let detail = Detail::of(ctx.quality);
        let err = kit_error(ctx);
        let mut shells = Shells::new(&attach, &default, 2);
        let cut =
            Cut { head: head.as_ref(), p, r: p.resolved(), rows: detail.rows / 2 - 1, cols: detail.lathe * 3 / 2 };
        let at = |mids: Vec<DVec3>, cols: usize| move |i: usize, j: usize| mids[i * (cols + 1) + j];
        let cols = cut.cols;
        if p.style == HatStyle::Crown {
            let spikes = 5.0;
            let base = 0.5 * cut.r.height;
            let edge = |phi: f64| base + cut.r.height * (1.0 - (2.0 * (spikes * phi / TAU).fract() - 1.0).abs());
            let (grid, mids) = cut.ring(edge, 0.0).map_err(&err)?;
            shells.bind(1, &grid, (true, true), &at(mids, cols)).map_err(&err)?;
            for k in 0..5 {
                let phi = TAU * (f64::from(k) + 0.5) / spikes;
                let rim = cut.rim(phi);
                let radius = f64::from(head.radius(omuzaki_core::glam::Vec3::Y));
                let top =
                    on_head(head.as_ref(), polar(rim - (base + cut.r.height) / radius, phi), p.lift + p.thickness);
                let top = top + DVec3::Y * 0.007;
                let gem = ellipsoid(top, DVec3::splat(0.008), 8, 10).map_err(&err)?;
                shells.bind(0, &gem, (false, true), &|_, _| top).map_err(&err)?;
            }
        } else {
            let (grid, mids) = cut.shell().map_err(&err)?;
            shells.bind(0, &grid, (false, true), &at(mids, cols)).map_err(&err)?;
            if p.band_height > 0.0 {
                let (ring, mids) = cut.ring(|_| p.band_height, 0.5 * p.thickness).map_err(&err)?;
                shells.bind(1, &ring, (true, true), &at(mids, cols)).map_err(&err)?;
            }
        }
        let brim = cut.r.brim;
        if brim > 0.0 && p.style != HatStyle::Crown {
            let width = |phi: f64| match p.style {
                // The bill is in front only; round the back the ring keeps a lip, so the shell stays closed.
                HatStyle::Cap => (brim * sstep(0.0, 0.5, phi.cos() - 0.25).powf(0.7)).max(MIN_LIP),
                _ => brim,
            };
            let (grid, mids) = cut.brim(width).map_err(&err)?;
            shells.bind(0, &grid, (true, true), &|i, j| mids[i * (cols + 1) + j]).map_err(&err)?;
        }
        match p.style {
            HatStyle::Beanie => {
                let top = on_head(head.as_ref(), polar(0.0, 0.0), p.lift + p.thickness + 0.5 * cut.r.height);
                // The pom-pom's lowest point rests on the cap's top.
                let centre = top + DVec3::Y * 0.025;
                let pom = ellipsoid(centre, DVec3::splat(0.028), 10, 14).map_err(&err)?;
                shells.bind(1, &pom, (false, true), &|_, _| centre).map_err(&err)?;
            }
            HatStyle::Beret => {
                let top = on_head(head.as_ref(), polar(0.0, 0.0), p.lift + p.thickness + cut.bulge(0.0, 0.0));
                let centre = top + DVec3::Y * 0.01;
                let stalk = ellipsoid(centre, DVec3::new(0.008, 0.012, 0.008), 8, 10).map_err(&err)?;
                shells.bind(1, &stalk, (false, true), &|_, _| centre).map_err(&err)?;
            }
            HatStyle::Witch | HatStyle::Cap | HatStyle::Crown => {}
        }
        shells.output(ctx, &Self::slots(p), RigContribution::default())
    }

    fn recolour(p: &HatParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour(out, ctx, &Self::slots(p))
    }
}

impl Hat {
    fn slots(p: &HatParams) -> [Slot; 2] {
        [Slot::opaque("hat", &p.hat), Slot::opaque("band", &p.band)]
    }
}

omuzaki_char_api::register_part_kind!(Hat);
