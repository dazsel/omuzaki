//! The `shoes` footwear (#385): sneakers, flats, loafers, heels and sandals (layer [`LAYER_FOOTWEAR`], over the
//! legwear), the low cousins of the [`boots`](super::boots).
//!
//! **Cut.** A shoe is cut from the boot the body would wear ([`Boot::fitted`]): the same outline, an ease clear of the
//! foot and leg at every height, the same welted sole. Its upper is that outline from the sole up to a collar line that
//! runs round the foot at a height given at the back, the sides and the front ([`Collar`], fractions of the ankle
//! joint's height above the foot's lowest point), lined inside and finished with a rolled edge. The styles add their
//! own pieces: the sneaker a rubber foxing band, a padded collar, a tongue and laces; the loafer a saddle across the
//! vamp; the sandal no upper at all, but straps over the foot, round the ankle and up the back.
//!
//! **Heels.** The foot is never re-posed: it stays flat as the body's rest pose has it, the sole an ease under it. A
//! heel taller than the sole hangs below the sole as a block under the heel and a platform under the ball of the foot,
//! with nothing under the arch, so the shoe stands on both and the character is that much taller. Up to 8 cm the foot
//! stays inside the shoe exactly as on a flat one.
//!
//! Built on the left foot and mirrored for the right, skinned as the boot is (the foot, the toes at the front, the
//! lower leg above the ankle).

// The geometry's own names (a point, a radius, an angle), and vertex colours, which the mesh stores in `f32`.
#![allow(clippy::many_single_char_names, clippy::cast_possible_truncation)]

use std::f64::consts::PI;

use omuzaki_char_api::{
    BodyFit, BodyRegion, BuildCtx, BuildError, KindProvenance, MaterialName, MaterialSpec, PartCategory, PartKind,
    PartMeta, PartOutput, Quality,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::boots::cuff::Band;
use super::boots::sole::add_sole;
use super::boots::{BOOT_TOP, Boot, BootsParams, Heel, LAYER_FOOTWEAR, Sole, Tess, Toe, outline, toon};
use super::coat::edge::{Patch, Tube, cord, loop_tube, patch};
use super::common::{Garment, GarmentCommon, GarmentError, publish_envelope};
use crate::kit::curve::{TAU, fraction, lerp, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};

// ----------------------------------------------------------------------------------------------------- parameters

/// The style of shoe.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShoeStyle {
    /// A canvas sneaker: a thick rubber sole and foxing, a padded collar, a tongue and laces.
    #[default]
    Sneaker,
    /// A ballet flat: cut low, a thin sole.
    Flat,
    /// A loafer: a saddle strap across the vamp, a lug sole and a low heel.
    Loafer,
    /// A pump on a heel: cut low, the heel and a platform under the ball of the foot.
    Heel,
    /// A sandal: an open sole held on by straps over the foot, round the ankle and up the back.
    Sandal,
}

/// The collar line: how high the upper reaches round the foot, each a fraction of the ankle joint's height above the
/// foot's lowest point (1 at the ankle joint).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Collar {
    /// At the back of the heel.
    pub back: f64,
    /// On either side, under the ankle bone.
    pub side: f64,
    /// At the front, over the instep (the throat).
    pub front: f64,
}

impl Collar {
    /// The height at angle `phi` round the foot (0 at the toe): a smooth curve through the three.
    pub fn at(&self, phi: f64) -> f64 {
        let c = phi.cos();
        self.side + 0.5 * (self.front - self.back) * c + (f64::midpoint(self.front, self.back) - self.side) * c * c
    }
}

/// The shoes' colours; each `None` is the style's.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ShoeColours {
    /// The upper (a sandal's straps).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upper: Option<ColourRef>,
    /// The sole, heel and platform (and a sneaker's foxing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sole: Option<ColourRef>,
    /// The collar's rolled edge, a sneaker's heel counter and a loafer's saddle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trim: Option<ColourRef>,
    /// The lining inside the upper.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lining: Option<ColourRef>,
    /// A sneaker's laces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub laces: Option<ColourRef>,
}

/// The parameters of `shoes`. Everything but the style and the ease defaults to the style's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ShoesParams {
    /// The ease (how far the shoe stands off the foot and whatever is worn over it); its `fabric` is not used: the
    /// colours are `colours`.
    pub common: GarmentCommon,
    /// The style.
    pub style: ShoeStyle,
    /// The collar line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collar: Option<Collar>,
    /// The toe box.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toe: Option<Toe>,
    /// How thick the sole is under the foot, metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sole: Option<f64>,
    /// How far the sole stands out round the upper, metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub welt: Option<f64>,
    /// How high the heel lifts the shoe off the floor, metres, from the sole's thickness (no heel) to 8 cm.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heel_height: Option<f64>,
    /// How much the heel narrows toward the floor, from 0 (a block) to 0.8 (slender).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heel_taper: Option<f64>,
    /// A sneaker's lace rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub laces: Option<u8>,
    /// The colours.
    pub colours: ShoeColours,
}

impl Default for ShoesParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon { ease: DEFAULT_EASE, ..GarmentCommon::default() },
            style: ShoeStyle::default(),
            collar: None,
            toe: None,
            sole: None,
            welt: None,
            heel_height: None,
            heel_taper: None,
            laces: None,
            colours: ShoeColours::default(),
        }
    }
}

/// The ease by default, metres.
pub const DEFAULT_EASE: f64 = 0.004;
/// The tallest heel, metres.
pub const MAX_HEEL: f64 = 0.08;
/// The most lace rows.
pub const MAX_LACES: u8 = 8;

impl Validate for ShoesParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        if let Some(collar) = &self.collar {
            c.range("collar/back", collar.back, 0.2, 1.4);
            c.range("collar/side", collar.side, 0.2, 1.4);
            c.range("collar/front", collar.front, 0.2, 1.4);
        }
        if let Some(toe) = &self.toe {
            c.nested("toe", toe);
        }
        if let Some(sole) = self.sole {
            c.range("sole", sole, 0.003, 0.04);
        }
        if let Some(welt) = self.welt {
            c.range("welt", welt, 0.0, 0.008);
        }
        if let Some(heel) = self.heel_height {
            c.range("heel_height", heel, 0.0, MAX_HEEL);
        }
        if let Some(taper) = self.heel_taper {
            c.range("heel_taper", taper, 0.0, 0.8);
        }
        if let Some(laces) = self.laces {
            c.range("laces", f64::from(laces), 0.0, f64::from(MAX_LACES));
        }
    }
}

/// What a style is when nothing overrides it.
struct Preset {
    collar: Collar,
    toe: Toe,
    sole: f64,
    welt: f64,
    heel: f64,
    taper: f64,
    upper: ColourRef,
    sole_colour: ColourRef,
    trim: ColourRef,
    lining: ColourRef,
}

impl ShoeStyle {
    fn preset(self) -> Preset {
        let lining = ColourRef::rgb(0xc9, 0xb4, 0x9a);
        match self {
            Self::Sneaker => Preset {
                collar: Collar { back: 0.95, side: 0.72, front: 1.05 },
                toe: Toe { width: 0.06, roundness: 0.55, cap: false },
                sole: 0.016,
                welt: 0.0015,
                heel: 0.016,
                taper: 0.0,
                upper: ColourRef::rgb(0x2e, 0x38, 0x56),
                sole_colour: ColourRef::rgb(0xf0, 0xed, 0xe6),
                trim: ColourRef::rgb(0xec, 0xe8, 0xe0),
                lining: ColourRef::rgb(0xe4, 0xe0, 0xd8),
            },
            Self::Flat => Preset {
                collar: Collar { back: 0.62, side: 0.42, front: 0.42 },
                toe: Toe { width: 0.04, roundness: 0.85, cap: false },
                sole: 0.005,
                welt: 0.0006,
                heel: 0.005,
                taper: 0.0,
                upper: ColourRef::rgb(0x24, 0x22, 0x2a),
                sole_colour: ColourRef::rgb(0x1a, 0x16, 0x16),
                trim: ColourRef::rgb(0x18, 0x16, 0x1c),
                lining,
            },
            Self::Loafer => Preset {
                collar: Collar { back: 0.75, side: 0.55, front: 0.72 },
                toe: Toe { width: 0.06, roundness: 0.6, cap: false },
                sole: 0.009,
                welt: 0.0022,
                heel: 0.02,
                taper: 0.1,
                upper: ColourRef::rgb(0x5a, 0x34, 0x24),
                sole_colour: ColourRef::rgb(0x2a, 0x1c, 0x16),
                trim: ColourRef::rgb(0x46, 0x28, 0x1a),
                lining,
            },
            Self::Heel => Preset {
                collar: Collar { back: 0.62, side: 0.4, front: 0.38 },
                toe: Toe { width: 0.03, roundness: 0.9, cap: false },
                sole: 0.006,
                welt: 0.0006,
                heel: 0.05,
                taper: 0.6,
                upper: ColourRef::rgb(0x1e, 0x1c, 0x24),
                sole_colour: ColourRef::rgb(0x16, 0x14, 0x18),
                trim: ColourRef::rgb(0x14, 0x12, 0x18),
                lining,
            },
            Self::Sandal => Preset {
                collar: Collar { back: 0.0, side: 0.0, front: 0.0 },
                toe: Toe { width: 0.05, roundness: 0.75, cap: false },
                sole: 0.012,
                welt: 0.0,
                heel: 0.012,
                taper: 0.0,
                upper: ColourRef::rgb(0x8a, 0x5a, 0x3a),
                sole_colour: ColourRef::rgb(0xd4, 0xc0, 0x9c),
                trim: ColourRef::rgb(0x7a, 0x4c, 0x30),
                lining,
            },
        }
    }
}

/// The parameters with every default filled in from the style.
struct Resolved {
    style: ShoeStyle,
    ease: f64,
    collar: Collar,
    toe: Toe,
    sole: f64,
    welt: f64,
    heel: f64,
    taper: f64,
    laces: u8,
    upper: ColourRef,
    sole_colour: ColourRef,
    trim: ColourRef,
    lining: ColourRef,
    lace_colour: ColourRef,
}

impl ShoesParams {
    fn resolve(&self) -> Resolved {
        let p = self.style.preset();
        let sole = self.sole.unwrap_or(p.sole);
        let c = &self.colours;
        Resolved {
            style: self.style,
            ease: self.common.ease,
            collar: self.collar.clone().unwrap_or(p.collar),
            toe: self.toe.clone().unwrap_or(p.toe),
            sole,
            welt: self.welt.unwrap_or(p.welt),
            heel: self.heel_height.unwrap_or(p.heel).max(sole),
            taper: self.heel_taper.unwrap_or(p.taper),
            laces: self.laces.unwrap_or(5),
            upper: c.upper.clone().unwrap_or(p.upper),
            sole_colour: c.sole.clone().unwrap_or(p.sole_colour),
            trim: c.trim.clone().unwrap_or(p.trim),
            lining: c.lining.clone().unwrap_or(p.lining),
            lace_colour: c.laces.clone().unwrap_or(ColourRef::rgb(0xf4, 0xf2, 0xec)),
        }
    }
}

impl Resolved {
    /// The boot the shoe is cut from: the sole an ease under the foot, its top [`SHOE_TOP`] up the shin, no heel drop
    /// (a heel hangs below the sole: [`Resolved::lift`]).
    fn boot(&self, fit: &dyn BodyFit) -> Result<Boot, GarmentError> {
        let mut boots = BootsParams::default();
        boots.common.ease = self.ease;
        boots.toe = self.toe.clone();
        boots.sole = Sole { thickness: self.sole, welt: self.welt, ..Sole::default() };
        boots.heel = Heel { height: self.sole, ..Heel::default() };
        Boot::fitted(&boots, fit, SHOE_TOP)
    }

    /// How far the heel and the platform hang below the sole, metres.
    fn lift(&self) -> f64 {
        (self.heel - self.sole).max(0.0)
    }

    fn has_upper(&self) -> bool {
        self.style != ShoeStyle::Sandal
    }

    /// The open borders of one shoe: the upper and its lining are open at the collar and the sole; a tongue and a
    /// saddle are patches. Every other piece is closed.
    fn borders_of_one(&self) -> usize {
        let upper = if self.has_upper() { 4 } else { 0 };
        let patches = match self.style {
            ShoeStyle::Sneaker | ShoeStyle::Loafer => 2,
            _ => 0,
        };
        upper + patches
    }
}

// --------------------------------------------------------------------------------------------------------- the cut

/// The boot the shoe is cut from reaches this far from the ankle joint to the knee joint: room for the collar and a
/// sneaker's tongue above it.
const SHOE_TOP: f64 = 0.15;
/// The highest the collar reaches in boot space: room above it, under the boot's top, for a tongue.
const COLLAR_TOP: f64 = BOOT_TOP - 0.022;
/// How thick the upper's leather is: the lining lies this far inside it, metres, or less on a thin ease.
const LEATHER: f64 = 0.0012;
/// The share of the ease the lining may take.
const LINING_SHARE: f64 = 0.3;
/// The collar's rolled edge: its radius, metres, a sneaker's padded one's, and how far clear of half the ease it keeps.
const ROLL: f64 = 0.0011;
const PADDED: f64 = 0.0026;
const ROLL_MARGIN: f64 = 0.00065;
/// The contact shadow above the sole: darkest, and the height it fades out by (metres above the sole's top).
const CONTACT: (f64, f64) = (0.82, 0.02);
/// How much darker the inner side (facing the other shoe) is.
const INNER_SIDE: f64 = 0.08;
/// A sneaker's heel counter: from where (the cosine of the angle round from the toe) it starts.
const COUNTER: f64 = -0.55;
/// A sneaker's foxing: its height and how far it stands off the upper, metres.
const FOXING: (f64, f64) = (0.0045, 0.0009);
/// A sneaker's tongue: half its width (radians round the outline), how far it reaches below and above the collar's
/// front (metres), its thickness.
const TONGUE: (f64, f64, f64, f64) = (0.42, 0.006, 0.014, 0.0016);
/// A sneaker's laces: half their span (radians), their radius, and the share of the throat's height they cover.
const LACES: (f64, f64, (f64, f64)) = (0.5, 0.0011, (0.45, 0.92));
/// A loafer's saddle: half its span (radians), half its height (metres), its thickness, and its height as a share of
/// the collar's front.
const SADDLE: (f64, f64, f64, f64) = (1.15, 0.006, 0.0014, 0.62);
/// A sandal's straps: half their width, half their thickness, metres; where along the foot (heel 0, toe 1) the two
/// over the foot cross it; the ankle strap's height as a share of the ankle joint's.
const STRAP: (f64, f64) = (0.0055, 0.0011);
/// How far past the ease a strap over the foot keeps, metres: the section's ellipse is only near the foot.
const STRAP_MARGIN: f64 = 0.0008;
const STRAPS_OVER: [f64; 2] = [0.62, 0.84];
const ANKLE_STRAP: f64 = 1.25;
/// Rows up the back strap: fine enough that no chord cuts the heel's round.
const BACK_ROWS: usize = 28;
/// Where the ball of the foot starts, as a share of the sole from the heel; how far forward of the heel's back the
/// heel block reaches, metres.
const BALL: f64 = 0.58;
const HEEL_LENGTH: f64 = 0.05;

/// A shoe to build: the boot it is cut from and the resolved parameters.
struct Shoe<'a> {
    boot: Boot,
    r: &'a Resolved,
    /// The body heights of the foot's lowest point and of the ankle joint.
    foot: (f64, f64),
    seg: usize,
    rows: usize,
}

impl Shoe<'_> {
    /// The boot-space height of the collar at `phi`, never above [`COLLAR_TOP`] (on a short-legged template the
    /// ankle is close to the boot's top).
    fn collar(&self, phi: f64) -> f64 {
        let (floor, ankle) = self.foot;
        self.boot.space().down(floor + self.r.collar.at(phi) * (ankle - floor)).min(COLLAR_TOP)
    }

    /// The boot-space height of a share `f` of the ankle joint's height.
    fn height(&self, f: f64) -> f64 {
        let (floor, ankle) = self.foot;
        self.boot.space().down(floor + f * (ankle - floor))
    }

    /// How far inside the upper the lining lies, metres.
    fn lining(&self) -> f64 {
        LEATHER.min(LINING_SHARE * self.r.ease)
    }

    /// The boot-space height of row `i` of the column at `phi`: denser toward the sole.
    fn row(&self, i: usize, phi: f64) -> f64 {
        self.collar(phi) * fraction(i, self.rows).powf(1.15)
    }

    /// The upper and its lining.
    fn upper(&self, colours: &Colours, builder: &mut MeshBuilder) -> Result<(), GarmentError> {
        let (seg, space) = (self.seg, self.boot.space());
        let sole_top = space.flat(self.r.sole);
        for (off, lining) in [(0.0, false), (-self.lining(), true)] {
            let grid = Grid::from_fn(self.rows + 1, seg + 1, |i, j| {
                let phi = TAU * fraction(j, seg);
                self.boot.plan(self.row(i, phi), phi, off).p
            })?;
            let inside = |i: usize, j: usize| {
                let y = self.row(i, TAU * fraction(j, seg));
                let s = self.boot.secs(y);
                DVec3::new(s.cx, space.up(y), s.cz)
            };
            let skin = |p: DVec3, _: usize, _: usize| self.boot.skin(p);
            let paint = |_: usize, j: usize, p: DVec3| {
                if lining {
                    return colours.lining;
                }
                let phi = TAU * fraction(j, seg);
                let base = if self.r.style == ShoeStyle::Sneaker && phi.cos() < COUNTER {
                    colours.trim
                } else {
                    colours.upper
                };
                let mut k = CONTACT.0 + (1.0 - CONTACT.0) * sstep(0.0, CONTACT.1, p.y - sole_top);
                k *= 1.0 - INNER_SIDE * (-phi.sin()).max(0.0);
                let k = k as f32;
                [base[0] * k, base[1] * k, base[2] * k, 1.0]
            };
            let options = GridOptions {
                wrap_j: true,
                inside: Some(&inside),
                invert: lining,
                skin: Skin::Per(&skin),
                colour: Some(&paint),
                ..GridOptions::default()
            };
            builder.add_grid(&grid, &options)?;
        }
        // The rolled edge over the two at the collar, clear of half the ease inside.
        let radius = if self.r.style == ShoeStyle::Sneaker { PADDED } else { ROLL };
        let off = radius - 0.5 * self.r.ease + ROLL_MARGIN;
        let (points, skins): (Vec<DVec3>, Vec<_>) = (0..seg)
            .map(|j| {
                let phi = TAU * fraction(j, seg);
                let y = self.collar(phi);
                // Where the body swells out below the collar (the heel's round), the roll's lower side stands clear of
                // it too.
                let at = self.boot.plan(y, phi, 0.0);
                let below = self.boot.plan_at(y, -radius, phi, 0.0).p - at.p;
                let swell = DVec3::new(below.x, 0.0, below.z).dot(at.n).max(0.0);
                let p = self.boot.plan(y, phi, off + swell).p;
                (p, self.boot.skin(p))
            })
            .unzip();
        let axis = |p: DVec3| {
            let s = self.boot.secs(space.down(p.y));
            DVec3::new(s.cx, p.y, s.cz)
        };
        let tube = Tube { points: &points, skins: &skins, radius, colour: rgb(colours.trim) };
        loop_tube(builder, &tube, &axis)
    }

    /// A sneaker's foxing, tongue and laces.
    fn sneaker(&self, colours: &Colours, builder: &mut MeshBuilder) -> Result<(), GarmentError> {
        let (height, stand) = FOXING;
        let foxing = Band {
            yc: self.r.sole,
            half_height: height,
            half_thickness: 0.5 * stand,
            stand_off: 0.5 * stand,
            exponent: 3.0,
            segs: (self.seg, 8),
        };
        foxing.add_to(&self.boot, builder, &|_| colours.sole)?;
        // The tongue: a patch over the throat, from under the collar's front to above it, clear of the padded edge.
        let (half, below, above, thickness) = TONGUE;
        let front = self.collar(0.0);
        let above = above.min((BOOT_TOP - 0.002 - front) * self.boot.space().slope(front));
        let off = 2.0 * PADDED - 0.5 * self.r.ease + ROLL_MARGIN + 0.0004;
        let (rows, cols) = (6, 9);
        let mut piece = Piece::default();
        for i in 0..rows {
            for j in 0..cols {
                let dy = lerp(-below, above, fraction(i, rows - 1));
                let phi = lerp(-half, half, fraction(j, cols - 1));
                // Rounded at the top.
                let dy = dy - above * 0.25 * (phi / half).powi(2) * fraction(i, rows - 1);
                let plan = self.boot.plan_at(front, dy, phi, off);
                piece.push(plan.p, plan.n, self.boot.skin(plan.p));
            }
        }
        piece.lay(builder, rows, cols, thickness, (colours.upper, colours.lining))?;
        // The laces: bars across the throat, lying on the upper.
        let (span, radius, (from, to)) = LACES;
        let n = usize::from(self.r.laces);
        let top = front - 1.5 * PADDED / self.boot.space().slope(front);
        let (low, high) = (self.height(from * self.r.collar.front), self.height(to * self.r.collar.front));
        let high = high.min(top);
        let low = low.min(high - (to - from) * (high - self.height(0.0)));
        for k in 0..n {
            let t = if n > 1 { fraction(k, n - 1) } else { 0.5 };
            let y = lerp(low, high, t);
            let (points, skins): (Vec<DVec3>, Vec<_>) = (0..=12)
                .map(|m| {
                    let phi = lerp(-span, span, fraction(m, 12));
                    let p = self.boot.plan(y, phi, radius + 0.0005).p;
                    (p, self.boot.skin(p))
                })
                .unzip();
            cord(builder, &Tube { points: &points, skins: &skins, radius, colour: rgb(colours.laces) })?;
        }
        Ok(())
    }

    /// A loafer's saddle: a strap across the vamp.
    fn saddle(&self, colours: &Colours, builder: &mut MeshBuilder) -> Result<(), GarmentError> {
        let (span, half, thickness, at) = SADDLE;
        let yc = self.height(at * self.r.collar.front);
        let (rows, cols) = (4, 25);
        let mut piece = Piece::default();
        for i in 0..rows {
            for j in 0..cols {
                let plan = self.boot.plan_at(
                    yc,
                    lerp(-half, half, fraction(i, rows - 1)),
                    lerp(-span, span, fraction(j, cols - 1)),
                    0.0004,
                );
                piece.push(plan.p, plan.n, self.boot.skin(plan.p));
            }
        }
        piece.lay(builder, rows, cols, thickness, (colours.trim, colours.lining))
    }

    /// A sandal's straps: two over the foot from one side of the sole to the other, one round the ankle and one up the
    /// back from the sole to it.
    fn sandal(&self, fit: &dyn BodyFit, colours: &Colours, builder: &mut MeshBuilder) -> Result<(), GarmentError> {
        let space = self.boot.space();
        let sole_top = space.flat(self.r.sole);
        let (half, thick) = STRAP;
        let colour = colours.upper;
        for t in STRAPS_OVER {
            let s = fit
                .section(BodyRegion::FootL, t as f32)
                .ok_or(GarmentError::NoSection { region: BodyRegion::FootL, t })?;
            let (centre, axis) = (s.centre.as_dvec3(), s.axis.as_dvec3().normalize_or_zero());
            let up = (DVec3::Y - axis * axis.y).normalize_or_zero();
            let across = axis.cross(up).normalize_or_zero();
            let grow = self.r.ease + thick + STRAP_MARGIN;
            let (rx, ry) = (f64::from(s.radii.x) + grow, f64::from(s.radii.y) + grow);
            let mut path = Vec::new();
            let side = |a: f64| centre + across * (rx * a.cos()) + up * (ry * a.sin());
            path.push((side(0.0) - DVec3::Y * (side(0.0).y - sole_top + thick), across));
            for m in 0..=16 {
                let a = PI * fraction(m, 16);
                let n = (across * (a.cos() / rx) + up * (a.sin() / ry)).normalize_or_zero();
                path.push((side(a), n));
            }
            path.push((side(PI) - DVec3::Y * (side(PI).y - sole_top + thick), -across));
            ribbon(&self.boot, builder, &path, axis, (half, thick), colour)?;
        }
        // Round the ankle.
        let ankle = self.height(ANKLE_STRAP);
        let band = Band {
            yc: ankle,
            half_height: half,
            half_thickness: thick,
            stand_off: thick,
            exponent: 3.0,
            segs: (self.seg, 8),
        };
        band.add_to(&self.boot, builder, &|_| colour)?;
        // Up the back, from inside the sole to inside the ankle strap.
        let path: Vec<(DVec3, DVec3)> = (0..=BACK_ROWS)
            .map(|m| {
                let y = lerp(self.r.sole * 0.9, ankle, fraction(m, BACK_ROWS));
                let plan = self.boot.plan(y, PI, thick);
                (plan.p, plan.n)
            })
            .collect();
        ribbon(&self.boot, builder, &path, DVec3::X, (half * 0.8, thick), colour)
    }

    /// The heel block and the platform under the ball of the foot, hung `lift` below the sole.
    fn heel(&self, tess: &Tess, builder: &mut MeshBuilder) -> Result<(), GarmentError> {
        let lift = self.r.lift();
        if lift <= 0.0 {
            return Ok(());
        }
        let space = self.boot.space();
        let (bottom, top) = (space.floor() - lift, space.floor() + 0.5 * self.r.sole);
        let s = self.boot.secs(self.r.sole);
        let back = s.zb;
        let ball = lerp(s.zb, s.zf, BALL);
        block(self, tess, (bottom, top), |z| z.min(back + HEEL_LENGTH), self.r.taper, builder)?;
        block(self, tess, (bottom, top), |z| z.max(ball), 0.0, builder)
    }
}

/// A closed slab under the sole: the sole's outline (with the welt) cut by `clip` (on `z`), from `bottom` to `top`
/// (body heights), narrowing toward the bottom by `taper`.
fn block(
    shoe: &Shoe<'_>,
    tess: &Tess,
    (bottom, top): (f64, f64),
    clip: impl Fn(f64) -> f64,
    taper: f64,
    builder: &mut MeshBuilder,
) -> Result<(), GarmentError> {
    // `(height share, scale toward the centre)`, bottom to top: a flat bottom, the walls, a flat top.
    const RINGS: [(f64, f64); 9] =
        [(0.0, 0.0), (0.0, 0.6), (0.0, 1.0), (0.03, 1.0), (0.5, 1.0), (0.97, 1.0), (1.0, 1.0), (1.0, 0.6), (1.0, 0.0)];
    let seg = tess.sole_seg();
    let outline: Vec<DVec3> = (0..=seg)
        .map(|j| {
            let p = shoe.boot.plan(shoe.r.sole, TAU * fraction(j, seg), shoe.r.welt).p;
            DVec3::new(p.x, 0.0, clip(p.z))
        })
        .collect();
    let n = fraction(seg, 1);
    let centre = outline[..seg].iter().fold(DVec3::ZERO, |a, p| a + *p) / n;
    let grid = Grid::from_fn(RINGS.len(), seg + 1, |i, j| {
        let (h, sc) = RINGS[i];
        let sc = sc * (1.0 - taper * (1.0 - h));
        let p = centre + (outline[j] - centre) * sc;
        DVec3::new(p.x, lerp(bottom, top, h), p.z)
    })?;
    let mid = DVec3::new(centre.x, f64::midpoint(bottom, top), centre.z);
    let inside = |_: usize, _: usize| mid;
    let skin = |p: DVec3, _: usize, _: usize| shoe.boot.skin(p);
    let options = GridOptions { wrap_j: true, inside: Some(&inside), skin: Skin::Per(&skin), ..GridOptions::default() };
    Ok(builder.add_grid(&grid, &options)?)
}

/// A flat strap along `path` (points with the strap's outward normal), `width` along `along`, closed at both ends.
fn ribbon(
    boot: &Boot,
    builder: &mut MeshBuilder,
    path: &[(DVec3, DVec3)],
    along: DVec3,
    (half, thick): (f64, f64),
    colour: [f32; 4],
) -> Result<(), GarmentError> {
    const SEG: usize = 8;
    let last = path.len() - 1;
    let grid = Grid::from_fn(path.len(), SEG + 1, |i, j| {
        let (p, n) = path[i];
        if i == 0 || i == last {
            return p;
        }
        let a = TAU * fraction(j % SEG, SEG);
        let side = (along - n * n.dot(along)).try_normalize().unwrap_or(along);
        p + side * (half * a.cos()) + n * (thick * a.sin())
    })?;
    let inside = |i: usize, _: usize| path[i].0;
    let skin = |p: DVec3, _: usize, _: usize| boot.skin(p);
    let paint = |_: usize, _: usize, _: DVec3| colour;
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        skin: Skin::Per(&skin),
        colour: Some(&paint),
        ..GridOptions::default()
    };
    Ok(builder.add_grid(&grid, &options)?)
}

/// The points of a patch being laid out, row by row.
#[derive(Default)]
struct Piece {
    points: Vec<DVec3>,
    normals: Vec<DVec3>,
    skins: Vec<Vec<(omuzaki_char_api::JointRef, f32)>>,
}

impl Piece {
    fn push(&mut self, p: DVec3, n: DVec3, skin: Vec<(omuzaki_char_api::JointRef, f32)>) {
        self.points.push(p);
        self.normals.push(n);
        self.skins.push(skin);
    }

    fn lay(
        self,
        builder: &mut MeshBuilder,
        rows: usize,
        cols: usize,
        thickness: f64,
        (face, under): ([f32; 4], [f32; 4]),
    ) -> Result<(), GarmentError> {
        let face = rgb(face);
        let piece = Patch {
            rows,
            cols,
            points: self.points,
            normals: self.normals,
            skins: self.skins,
            thickness,
            colours: (face, rgb(under)),
            edge: face.map(|c| c * 0.85),
            uvs: None,
        };
        patch(builder, &piece)
    }
}

fn rgb(c: [f32; 4]) -> [f32; 3] {
    [c[0], c[1], c[2]]
}

/// The resolved colours, linear RGBA.
struct Colours {
    upper: [f32; 4],
    sole: [f32; 4],
    trim: [f32; 4],
    lining: [f32; 4],
    laces: [f32; 4],
}

// ------------------------------------------------------------------------------------------------------ the kind

/// The `shoes` part kind.
pub struct Shoes;

impl Garment for Shoes {
    fn common(p: &ShoesParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &ShoesParams) -> Vec<BodyRegion> {
        vec![BodyRegion::LegL, BodyRegion::LegR, BodyRegion::FootL, BodyRegion::FootR]
    }

    fn open_borders(p: &ShoesParams) -> usize {
        2 * p.resolve().borders_of_one()
    }
}

impl PartKind for Shoes {
    type Params = ShoesParams;

    fn layer() -> Option<i32> {
        Some(LAYER_FOOTWEAR)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "shoes",
            summary: "build shoes: sneakers with laces, flats, loafers with a saddle, heels with a platform, or sandals",
            category: PartCategory::Footwear,
            requires: &["body"],
        }
    }

    fn provenance() -> KindProvenance {
        KindProvenance::ProjectOwned
    }

    fn build(p: &ShoesParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        p.validate().map_err(|e| ctx.fail(format!("the parameters are invalid: {e}")))?;
        let r = p.resolve();
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let fit = ctx.fit()?;
        let boot = r.boot(fit.as_ref()).map_err(fail)?;
        let foot = boot.foot().ok_or_else(|| ctx.fail("the shoe has no foot to fit"))?;
        let tess = Tess::of(ctx.quality);
        let (seg, rows) = match ctx.quality {
            Quality::Live => (48, 16),
            Quality::Offline => (72, 28),
        };
        let shoe = Shoe { boot, r: &r, foot, seg, rows };
        let linear = |c: &ColourRef| -> Result<[f32; 4], BuildError> {
            let [r, g, b] = ctx.colour(c)?.to_linear().to_array();
            Ok([r, g, b, 1.0])
        };
        let colours = Colours {
            upper: linear(&r.upper)?,
            sole: linear(&r.sole_colour)?,
            trim: linear(&r.trim)?,
            lining: linear(&r.lining)?,
            laces: linear(&r.lace_colour)?,
        };

        let mut upper = MeshBuilder::with_colours();
        let mut sole = MeshBuilder::new();
        let build = |upper: &mut MeshBuilder, sole: &mut MeshBuilder| -> Result<(), GarmentError> {
            if r.has_upper() {
                shoe.upper(&colours, upper)?;
            }
            match r.style {
                ShoeStyle::Sneaker => shoe.sneaker(&colours, upper)?,
                ShoeStyle::Loafer => shoe.saddle(&colours, upper)?,
                ShoeStyle::Sandal => shoe.sandal(fit.as_ref(), &colours, upper)?,
                ShoeStyle::Flat | ShoeStyle::Heel => {}
            }
            let sole_spec = Sole { thickness: r.sole, welt: r.welt, ..Sole::default() };
            add_sole(&shoe.boot, &tess, &sole_spec, sole)?;
            shoe.heel(&tess, sole)?;
            for b in [upper, sole] {
                let mirror = b.mirrored();
                b.merge(&mirror)?;
            }
            Ok(())
        };
        build(&mut upper, &mut sole).map_err(fail)?;

        let mut out = PartOutput::new();
        let part = ctx.part.as_str().to_owned();
        let line = Colour::hex(0x0014_1012);
        let white = Colour::hex(0x00ff_ffff);
        let upper_spec = MaterialSpec {
            vertex_colours: true,
            outline: Some(outline(line, 0.0014)),
            ..toon(white, Colour::hex(0x0084_7a86), -0.08, 0.16, (Colour::hex(0x00c0_a090), 0.10))
        };
        let sole_spec = MaterialSpec {
            outline: Some(outline(line, 0.0012)),
            ..toon(ctx.colour(&r.sole_colour)?, Colour::hex(0x008a_8088), -0.05, 0.14, (Colour::hex(0), 0.0))
        };
        for (name, builder, spec) in [("upper", upper, upper_spec), ("sole", sole, sole_spec)] {
            if builder.triangle_count() == 0 {
                continue;
            }
            let id: MaterialName =
                format!("{part}-{name}").parse().map_err(|e| ctx.fail(format!("`{name}` isn't a valid name: {e}")))?;
            let mut mesh = builder
                .finish(format!("{part}-{name}"), id.clone())
                .map_err(|e| ctx.fail(format!("the `{name}` surface: {e}")))?;
            mesh.casts_shadow = true;
            out.meshes.push(mesh);
            out.materials.push((id, spec));
        }
        out.publishes = publish_envelope(ctx, &Self::regions(p), p.common.ease, 2.0)?;
        Ok(out)
    }
}

omuzaki_char_api::register_part_kind!(Shoes);

#[cfg(test)]
mod tests {
    use std::f64::consts::FRAC_PI_2;

    use super::*;

    #[test]
    fn the_collar_runs_through_its_three_heights() {
        let c = Collar { back: 0.9, side: 0.5, front: 0.7 };
        assert!((c.at(0.0) - 0.7).abs() < 1e-12);
        assert!((c.at(PI) - 0.9).abs() < 1e-12);
        assert!((c.at(FRAC_PI_2) - 0.5).abs() < 1e-12 && (c.at(-FRAC_PI_2) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn a_heel_is_never_lower_than_the_sole_and_hangs_the_rest_below_it() {
        let p = ShoesParams {
            style: ShoeStyle::Heel,
            heel_height: Some(0.07),
            sole: Some(0.008),
            ..ShoesParams::default()
        };
        assert!((p.resolve().lift() - 0.062).abs() < 1e-12);
        let flat = ShoesParams { style: ShoeStyle::Flat, heel_height: Some(0.0), ..ShoesParams::default() };
        assert_eq!(flat.resolve().lift(), 0.0);
    }
}
