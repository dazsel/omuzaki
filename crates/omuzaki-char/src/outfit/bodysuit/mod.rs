//! The `bodysuit` garment (#2447): a thin, close top that follows the body from the neck to the hips, the layer many
//! designs wear under armour, jewellery or a jacket. Sleeveless (held by straps, or by a collar band with a high or
//! halter neck) or with sleeves; worn over tights it reads as one suit.
//!
//! It is the [coat framework](crate::outfit::coat)'s bodice with a pencil hem at the hips: cut as the dress is, at a
//! small ease, so its inside lies within 4 mm of the body over the bust and the waist; thin, with a fine roll at its
//! edges, no folds and no sash.

use omuzaki_char_api::{BodyRegion, BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::coat::hang::{Arms, Fall};
use super::coat::look::CoatColours;
use super::coat::shell::{self, Frame};
use super::coat::sleeves;
use super::coat::{CoatCut, Front, MAX_LENGTH, MIN_LENGTH, NeckShape, Neckline};
use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, LAYER_UNDERWEAR, MAX_EASE};
use super::dress::{ARMHOLE, DressSleeves, MAX_STRAP_WIDTH, STRAP_WIDTH, hold_up, standing};

/// The layer a bodysuit is worn on: over underwear and tights, under a shirt.
pub const LAYER_BODYSUIT: i32 = LAYER_UNDERWEAR + 4;
/// The cut of its rings: the dress's, a little squarer than an ellipse. On the body's own ellipses the flats between
/// its rows cut into the bust at this ease.
pub const EXPONENT: f64 = 2.15;
/// The cloth's thickness by default, metres.
pub const THICKNESS: f64 = 0.0012;
/// How far it stands off the layer under it by default, metres.
pub const EASE: f64 = 0.0025;
/// The rolled edge's lip, metres.
const LIP: f64 = 0.0004;
/// The waist seam, as a fraction of the torso: low, so the bodice runs down over the waist.
const SEAM: f64 = 0.2;

/// The parameters of a bodysuit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct BodysuitParams {
    /// The common settings: `ease` is how far it stands off the layer below, `fabric` the cloth.
    pub common: GarmentCommon,
    /// How thick the cloth is, metres.
    pub thickness: f64,
    /// The neckline: a high neck by default; a halter leaves the back bare.
    pub neckline: Neckline,
    /// The sleeves (long by default), or none (`straps`: straps, or a collar band with a high or halter neck).
    pub sleeves: DressSleeves,
    /// How far down from the torso's top the armholes' floor is without sleeves, as a fraction of the torso.
    pub armhole: f64,
    /// How wide each strap over the shoulder is without sleeves, metres; 0 for none.
    pub strap_width: f64,
    /// Where the hem is: 0 at the hips' joints, negative above them (a cropped top), positive below.
    pub length: f64,
    /// The lining, the rolled edges and the toon.
    pub colours: CoatColours,
}

impl Default for BodysuitParams {
    fn default() -> Self {
        let fabric = ColourRef::rgb(0x24, 0x22, 0x2c);
        Self {
            common: GarmentCommon {
                ease: EASE,
                fabric: FabricStyle::Plain { colour: fabric.clone() },
                ..GarmentCommon::default()
            },
            thickness: THICKNESS,
            neckline: Neckline { shape: NeckShape::High, depth: 0.0, width: 0.8, back: 0.0, rise: 0.04 },
            sleeves: DressSleeves::Long,
            armhole: ARMHOLE,
            strap_width: STRAP_WIDTH,
            length: 0.02,
            colours: CoatColours { lining: ColourRef::rgb(0x1a, 0x18, 0x20), trim: fabric, ..CoatColours::default() },
        }
    }
}

impl Validate for BodysuitParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.range("thickness", self.thickness, 0.0003, 0.005);
        if self.common.ease + self.thickness > MAX_EASE {
            c.fail(
                "thickness",
                format!("ease and thickness stand out {} m: at most {MAX_EASE}", self.common.ease + self.thickness),
            );
        }
        self.neckline.check(c);
        c.range("armhole", self.armhole, 0.0, 0.6);
        c.range("strap_width", self.strap_width, 0.0, MAX_STRAP_WIDTH);
        c.range("length", self.length, MIN_LENGTH, 0.2_f64.min(MAX_LENGTH));
        if self.neckline.shape == NeckShape::Halter && self.sleeves != DressSleeves::Straps {
            c.fail("neckline/shape", "a halter neck leaves the shoulders bare: set sleeves to straps");
        }
    }
}

impl BodysuitParams {
    /// The coat cut these parameters make.
    pub fn cut(&self) -> CoatCut {
        let sleeves = self.sleeves.cut();
        CoatCut {
            exponent: EXPONENT,
            ease: self.common.ease,
            thickness: self.thickness,
            waist: SEAM,
            front: Front::Closed,
            neckline: standing(&self.neckline),
            armhole: if sleeves.is_some() { 0.0 } else { self.armhole },
            length: self.length,
            back_drop: 0.0,
            fall: Fall::Pencil,
            flare: 0.0,
            folds: (0, 0.0),
            arms: Arms::Clear {
                sleeve: sleeves
                    .map_or((0.0, 0.0), |s| (s.width + self.thickness, s.width + s.shape.flare + self.thickness)),
                margin: 0.004,
            },
            lip: LIP,
        }
    }
}

/// The bodysuit part kind.
pub struct Bodysuit;

impl PartKind for Bodysuit {
    type Params = BodysuitParams;

    fn layer() -> Option<i32> {
        Some(LAYER_BODYSUIT)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "bodysuit",
            summary: "a thin, close top that follows the body from the neck to the hips: a high, halter or scoop neck, \
                      sleeveless or with sleeves; under armour, jewellery or a jacket",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &BodysuitParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let cut = p.cut();
        let frame = Frame::new(ctx.fit()?, &cut, super::coat::under(ctx)).map_err(fail)?;
        let out = GarmentOutput::new(ctx, &p.common)?;
        let tones = super::coat::look::tones(ctx, &p.common, &p.colours)?;
        let mut shell = shell::build(&frame, &tones, out.tile()).map_err(fail)?;
        let sleeves = if let Some(cut) = p.sleeves.cut() {
            Some(sleeves::build(&frame, &cut, &tones, out.tile()).map_err(fail)?)
        } else {
            hold_up(&mut shell.mesh, &frame, p.strap_width, &tones).map_err(fail)?;
            None
        };
        super::coat::finish(ctx, out, &p.common, &p.colours, &frame, shell, sleeves, &Self::regions(p))
    }
}

impl Garment for Bodysuit {
    fn common(p: &BodysuitParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(p: &BodysuitParams) -> Vec<BodyRegion> {
        match p.sleeves {
            DressSleeves::Straps => vec![BodyRegion::Torso],
            _ => vec![BodyRegion::Torso, BodyRegion::ArmL, BodyRegion::ArmR],
        }
    }

    /// The hem and the neckline, outside and in; with sleeves, an armhole in each and two ends to each sleeve; with
    /// straps, two for each strap.
    fn open_borders(p: &BodysuitParams) -> usize {
        4 + if p.sleeves.cut().is_some() {
            4 + 4
        } else if p.strap_width > 0.0 && !p.neckline.shape.stands() {
            4
        } else {
            0
        }
    }
}

omuzaki_char_api::register_part_kind!(Bodysuit);
