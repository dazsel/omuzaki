//! The `hoodie` garment (#392): a pullover hoodie, closed all round, with set-in sleeves, knitted rib cuffs and a rib
//! band at the hem, a kangaroo pocket across the front, drawstrings with aglets at the neck and a hood folded down or
//! raised.
//!
//! It is cut with the [coat framework](crate::outfit::coat): a roomy bodice above a low waist seam and a short skirt
//! that hangs straight to the hips, lined sleeves welded to it. The rib bands are closed [bands](crate::outfit::coat::edge::band)
//! raised in ribs; the pocket is a [patch](crate::outfit::coat::trims::pocket) whose open sides are where the hands go in.

use omuzaki_char_api::{BodyRegion, BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::coat::hang::{Arms, Fall};
use super::coat::look::{self, CoatColours};
use super::coat::shell::{self, Frame};
use super::coat::sleeves::{self, SleeveCut};
use super::coat::trims::{self, Drawstrings, Pocket};
use super::coat::{CoatCut, Front, NeckShape, Neckline};
use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, LAYER_OUTER, MAX_EASE};
use super::hood::{self, Hood};
use super::shirt::sleeve::SleeveShape;

/// The layer a hoodie is worn on: over a shirt, under a jacket.
pub const LAYER_HOODIE: i32 = LAYER_OUTER - 4;
/// The cut of its rings: rounder than a tailored coat's.
pub const EXPONENT: f64 = 2.3;
/// The knit's thickness by default, metres.
pub const THICKNESS: f64 = 0.0028;
/// The rolled edge's lip, metres.
const LIP: f64 = 0.0008;
/// The waist seam, as a fraction of the torso: low, so the body hangs straight from the chest.
const WAIST: f64 = 0.16;
/// The shortest a hoodie is cut (just above the hips: shorter, its waist would pass between the arms and the body of a
/// small character) and the longest (a long hoodie to mid thigh), as [`HoodieParams::length`].
const SHORTEST: f64 = -0.08;
const LONGEST: f64 = 0.4;
/// The kangaroo pocket: its top as a fraction of the torso, its half width round the front (radians), and how far it
/// stands off at its bottom (metres).
const POCKET: (f64, f64, f64) = (0.3, 0.62, 0.003);
/// How far above the hem's rib the pocket ends, and the drawstrings at the latest above the pocket, metres.
const POCKET_GAP: f64 = 0.006;
/// The pocket's least height, metres: on a cropped hoodie it rises over the chest rather than vanish.
const POCKET_LEAST: f64 = 0.03;

/// The knitted ribs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Ribbing {
    /// The hem band's height, metres; 0 for none.
    pub hem: f64,
    /// The cuffs' length up the arm, metres; 0 for none.
    pub cuffs: f64,
}

impl Default for Ribbing {
    fn default() -> Self {
        Self { hem: 0.04, cuffs: 0.045 }
    }
}

/// The parameters of a hoodie.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct HoodieParams {
    /// The common settings: `ease` is how far the lining stands off what is under it, `fabric` the knit, `cloth` its
    /// simulation.
    pub common: GarmentCommon,
    /// How thick the knit is, metres.
    pub thickness: f64,
    /// Where the hem is: 0 at the hips' joints, 1 at the ankles (negative: cropped above the hips).
    pub length: f64,
    /// How much of the arm the sleeves cover, 0.2 to 1.
    pub sleeve_length: f64,
    /// The rib cuffs and hem band.
    pub ribbing: Ribbing,
    /// A kangaroo pocket across the front.
    pub pocket: bool,
    /// The drawstrings at the neck.
    pub drawstrings: Drawstrings,
    /// The hood.
    pub hood: Hood,
    /// The lining, the rolled edges and the toon.
    pub colours: CoatColours,
}

impl Default for HoodieParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: 0.005,
                fabric: FabricStyle::Plain { colour: ColourRef::rgb(0x8c, 0x8f, 0x96) },
                ..GarmentCommon::default()
            },
            thickness: THICKNESS,
            length: 0.02,
            sleeve_length: 1.0,
            ribbing: Ribbing::default(),
            pocket: true,
            drawstrings: Drawstrings::default(),
            hood: Hood::default(),
            colours: CoatColours {
                lining: ColourRef::rgb(0xa4, 0xa6, 0xab),
                trim: ColourRef::rgb(0x76, 0x79, 0x80),
                ..CoatColours::default()
            },
        }
    }
}

impl Validate for HoodieParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.range("thickness", self.thickness, 0.0005, 0.008);
        if self.common.ease + self.thickness > MAX_EASE {
            c.fail(
                "thickness",
                format!("ease and thickness stand out {} m: at most {MAX_EASE}", self.common.ease + self.thickness),
            );
        }
        c.range("length", self.length, SHORTEST, LONGEST);
        c.range("sleeve_length", self.sleeve_length, 0.2, 1.0);
        c.range("ribbing/hem", self.ribbing.hem, 0.0, 0.1);
        c.range("ribbing/cuffs", self.ribbing.cuffs, 0.0, 0.1);
        self.drawstrings.check(c);
        self.hood.check(c);
    }
}

impl HoodieParams {
    /// The coat cut these parameters make.
    pub fn cut(&self) -> CoatCut {
        let sleeve = self.sleeve();
        CoatCut {
            exponent: EXPONENT,
            ease: self.common.ease,
            thickness: self.thickness,
            waist: WAIST,
            front: Front::Closed,
            neckline: Neckline { shape: NeckShape::Round, depth: 0.035, width: 0.6, back: 0.01, rise: 0.03 },
            armhole: 0.0,
            length: self.length,
            back_drop: 0.0,
            fall: Fall::Drape,
            flare: 0.0,
            folds: (0, 0.0),
            arms: Arms::Clear {
                sleeve: (sleeve.width + self.thickness * 2.0, sleeve.width + sleeve.shape.flare + self.thickness * 2.0),
                margin: 0.004,
            },
            lip: LIP,
        }
    }

    /// The sleeves' cut: roomy, lined (the knit's two faces), ending in a rib cuff.
    fn sleeve(&self) -> SleeveCut {
        SleeveCut {
            width: 0.006,
            ease_at_elbow: 0.008,
            length: self.sleeve_length,
            shape: SleeveShape { flare: 0.0, past: 0.0, lining: 1.0 },
            // A rolled cuff only where there is no rib.
            cuff: if self.ribbing.cuffs > 0.0 { 0.0 } else { 0.0016 },
            rib: self.ribbing.cuffs,
            pouch: None,
        }
    }
}

/// The hoodie part kind.
pub struct Hoodie;

impl PartKind for Hoodie {
    type Params = HoodieParams;

    fn layer() -> Option<i32> {
        Some(LAYER_HOODIE)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "hoodie",
            summary: "a pullover hoodie: rib cuffs and hem band, a kangaroo pocket, drawstrings with aglets, a hood down \
                      or up",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &HoodieParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let frame = Frame::new(ctx.fit()?, &p.cut(), super::coat::under(ctx)).map_err(fail)?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let tones = look::tones(ctx, &p.common, &p.colours)?;
        let mut shell = shell::build(&frame, &tones, out.tile()).map_err(fail)?;
        trims::hem_rib(&mut shell.mesh, &frame, p.ribbing.hem, tones.outer).map_err(fail)?;
        let above_rib = frame.hem + p.ribbing.hem + POCKET_GAP;
        let mut lowest = above_rib;
        if let Some(pocket) = pocket(&frame, p) {
            trims::pocket(&mut shell.mesh, &frame, &pocket, &tones).map_err(fail)?;
            lowest = pocket.at.0 + POCKET_GAP;
        }
        let strings = (
            ctx.colour(&p.drawstrings.colour)?.to_linear().to_array(),
            ctx.colour(&p.drawstrings.aglet)?.to_linear().to_array(),
        );
        trims::drawstrings(&mut shell.mesh, &frame, &p.drawstrings, strings, lowest).map_err(fail)?;
        if let Some(hood) = hood::build(ctx, &frame, &p.hood, &tones)? {
            out.surface(ctx, "hood", hood)?;
        }
        let sleeves = sleeves::build(&frame, &p.sleeve(), &tones, out.tile()).map_err(fail)?;
        super::coat::finish(ctx, out, &p.common, &p.colours, &frame, shell, Some(sleeves), &Self::regions(p))
    }
}

/// The kangaroo pocket, if it has one: above the hem's rib.
fn pocket(frame: &Frame, p: &HoodieParams) -> Option<Pocket> {
    if !p.pocket {
        return None;
    }
    let (top, half, sag) = POCKET;
    let bottom = frame.hem + p.ribbing.hem + POCKET_GAP;
    let top = frame.torso(top).max(bottom + POCKET_LEAST);
    let height = top - bottom;
    let width = 2.0 * half * trims::radius(frame, top);
    Some(Pocket { at: (top, 0.0), size: (width, height), sag })
}

impl Garment for Hoodie {
    fn common(p: &HoodieParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &HoodieParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso, BodyRegion::ArmL, BodyRegion::ArmR]
    }

    /// The hem and the neckline, outside and in; an armhole in each; each lined sleeve's two ends, outside and in; the
    /// pocket's two; the hood's. The rib bands and the cords are closed.
    fn open_borders(p: &HoodieParams) -> usize {
        4 + 4 + 8 + if p.pocket { 2 } else { 0 } + p.hood.borders()
    }
}

omuzaki_char_api::register_part_kind!(Hoodie);
