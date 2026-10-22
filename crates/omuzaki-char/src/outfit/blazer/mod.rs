//! The `blazer` garment (#329): a tailored jacket over the shirt and the vest. Fitted through the body to the hips,
//! open at the front or buttoned with the left panel over the right, a V that opens into notched or shawl lapels,
//! hip pocket flaps and a breast welt, lined sleeves with rolled cuffs, a vent at the back, and long tails if asked.
//!
//! It is cut with the [coat framework](crate::outfit::coat): its waist seam low on the hips, so the short skirt below
//! it only carries the hem, the vent and the tails.

use omuzaki_char_api::{BodyRegion, BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::coat::hang::{Arms, Fall};
use super::coat::look::{self, CoatColours};
use super::coat::shell::{self, Frame};
use super::coat::sleeves::{self, SleeveCut};
use super::coat::trims::{self, BACK, Lapel, Pocket};
use super::coat::{CoatCut, Front, MAX_LENGTH, MIN_LENGTH, NeckShape, Neckline};
use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, LAYER_OUTER, MAX_EASE};
use super::shirt::sleeve::SleeveShape;
use super::trim::Finish;
use super::trim::buttons::{ButtonShape, ButtonStyle, GOLD_RADIUS};
use crate::kit::curve::TAU;

/// The layer a blazer is worn on: over the shirt, the vest and the dress.
pub const LAYER_BLAZER: i32 = LAYER_OUTER - 2;
/// The cut of its rings: squarer than the dress, a tailored shoulder line.
pub const EXPONENT: f64 = 2.3;
/// The cloth's thickness by default, metres.
pub const THICKNESS: f64 = 0.0026;
/// The rolled edge's lip, metres.
const LIP: f64 = 0.0006;
/// The waist seam, as a fraction of the torso: low on the hips, so the pockets sit on the fitted body.
const WAIST: f64 = 0.12;
/// The hip pockets: the top edge's height (fraction of the torso), angle from the centre front (radians), width and
/// depth (fractions of the torso), and the flap's sag (metres).
const HIP_POCKET: (f64, f64, f64, f64, f64) = (0.26, 1.0, 0.26, 0.09, 0.0015);
/// How far above the hem a hip pocket's bottom stays, metres.
const POCKET_ABOVE_HEM: f64 = 0.015;
/// The breast welt: height, the least angle from the centre front, width and depth, as the hip pockets'.
const WELT: (f64, f64, f64, f64) = (0.66, 0.8, 0.18, 0.035);
/// The farthest round from the centre front the welt's far end goes, radians: on a narrow chest it overlaps the lapel
/// rather than reach the arm.
const WELT_REACH: f64 = 1.0;
/// The vent: its length up from the hem, as a fraction of the torso.
const VENT: f64 = 0.16;
/// The tails' extra length at the back, metres.
const TAILS: f64 = 0.24;
/// The buttons: how far below the break the first is and the spacing between them, fractions of the torso.
const BUTTON_ROW: (f64, f64) = (0.06, 0.13);
/// The most buttons.
const MAX_BUTTONS: u32 = 6;

/// How the front is worn.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Closure {
    /// Buttoned: the left panel over the right by [`BlazerParams::overlap`].
    #[default]
    Buttoned,
    /// Open: the panels [`BlazerParams::opening`] apart.
    Open,
}

/// The pockets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Pockets {
    /// Flaps over the hips.
    pub hip: bool,
    /// A welt on the left breast.
    pub breast: bool,
}

impl Default for Pockets {
    fn default() -> Self {
        Self { hip: true, breast: true }
    }
}

/// The buttons down the front.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Buttons {
    /// How many, 0 to 6.
    pub count: u32,
    /// Each one's radius, metres.
    pub radius: f64,
    /// What they are made of.
    pub finish: Finish,
}

impl Default for Buttons {
    fn default() -> Self {
        Self {
            count: 2,
            radius: GOLD_RADIUS * 1.1,
            finish: Finish { colour: ColourRef::rgb(0x2a, 0x26, 0x24), metal: None },
        }
    }
}

/// The parameters of a blazer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct BlazerParams {
    /// The common settings: `ease` is how far the lining stands off the layer below, `fabric` the cloth.
    pub common: GarmentCommon,
    /// How thick the cloth is, metres.
    pub thickness: f64,
    /// Buttoned or open.
    pub closure: Closure,
    /// How far the left panel crosses over the right when buttoned, radians round the body.
    pub overlap: f64,
    /// How far apart the panels hang when open, radians round the body (each side of the centre).
    pub opening: f64,
    /// Where the V opens (the lapels' break), as a fraction of the torso.
    pub break_point: f64,
    /// How far either side of the centre the V reaches at the collar, radians.
    pub collar: f64,
    /// How far the collar line stands above the torso's top, as a fraction of the torso.
    pub rise: f64,
    /// The lapels.
    pub lapel: Lapel,
    /// The pockets.
    pub pockets: Pockets,
    /// The buttons.
    pub buttons: Buttons,
    /// A vent up the centre back.
    pub vent: bool,
    /// Long tails at the back.
    pub tails: bool,
    /// Where the hem is: 0 at the hips' joints, 1 at the ankles (negative: cropped above the hips).
    pub length: f64,
    /// How much of the arm the sleeves cover, 0.2 to 1.
    pub sleeve_length: f64,
    /// The lining, the rolled edges and the toon.
    pub colours: CoatColours,
}

impl Default for BlazerParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: 0.005,
                fabric: FabricStyle::Plain { colour: ColourRef::rgb(0x3b, 0x3f, 0x4c) },
                ..GarmentCommon::default()
            },
            thickness: THICKNESS,
            closure: Closure::Buttoned,
            overlap: 0.1,
            opening: 0.22,
            break_point: 0.5,
            collar: 0.62,
            rise: 0.03,
            lapel: Lapel::default(),
            pockets: Pockets::default(),
            buttons: Buttons::default(),
            vent: true,
            tails: false,
            length: 0.1,
            sleeve_length: 1.0,
            colours: CoatColours {
                lining: ColourRef::rgb(0x7a, 0x74, 0x80),
                trim: ColourRef::rgb(0x33, 0x36, 0x42),
                ..CoatColours::default()
            },
        }
    }
}

impl Validate for BlazerParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.range("thickness", self.thickness, 0.0005, 0.01);
        if self.common.ease + self.thickness > MAX_EASE {
            c.fail(
                "thickness",
                format!("ease and thickness stand out {} m: at most {MAX_EASE}", self.common.ease + self.thickness),
            );
        }
        c.range("overlap", self.overlap, 0.0, 0.4);
        c.range("opening", self.opening, 0.0, 1.2);
        c.range("break_point", self.break_point, 0.15, 0.9);
        c.range("collar", self.collar, 0.2, 1.2);
        c.range("rise", self.rise, -0.05, 0.2);
        self.lapel.check(c);
        if self.buttons.count > MAX_BUTTONS {
            c.fail("buttons/count", format!("{}: at most {MAX_BUTTONS}", self.buttons.count));
        }
        c.range("buttons/radius", self.buttons.radius, 0.001, 0.02);
        c.nested("buttons/finish", &self.buttons.finish);
        c.range("length", self.length, MIN_LENGTH, MAX_LENGTH);
        c.range("sleeve_length", self.sleeve_length, 0.2, 1.0);
    }
}

impl BlazerParams {
    /// The front edge's angle below the V: across the centre when buttoned.
    fn gap(&self) -> f64 {
        match self.closure {
            Closure::Buttoned => -self.overlap,
            Closure::Open => self.opening,
        }
    }

    /// The coat cut these parameters make.
    pub fn cut(&self) -> CoatCut {
        let sleeve = self.sleeve();
        CoatCut {
            exponent: EXPONENT,
            ease: self.common.ease,
            thickness: self.thickness,
            waist: WAIST,
            front: Front::Open { gap: self.gap(), point: self.break_point, neck: self.collar.max(self.gap() + 0.05) },
            neckline: Neckline { shape: NeckShape::High, depth: 0.0, width: 0.9, back: 0.0, rise: self.rise },
            armhole: 0.0,
            length: self.length,
            back_drop: if self.tails { TAILS } else { 0.0 },
            fall: Fall::Drape,
            flare: 0.012,
            folds: (0, 0.0),
            arms: Arms::Clear {
                sleeve: (sleeve.width + self.thickness * 2.0, sleeve.width + sleeve.shape.flare + self.thickness * 2.0),
                margin: 0.004,
            },
            lip: LIP,
        }
    }

    /// The sleeves' cut: lined, a little wider than a shirt's, with a rolled cuff.
    fn sleeve(&self) -> SleeveCut {
        SleeveCut {
            width: 0.006,
            ease_at_elbow: 0.006,
            length: self.sleeve_length,
            shape: SleeveShape { flare: 0.004, past: 0.0, lining: 1.0 },
            cuff: 0.0016,
            rib: 0.0,
            pouch: None,
        }
    }
}

/// The blazer part kind.
pub struct Blazer;

impl PartKind for Blazer {
    type Params = BlazerParams;

    fn layer() -> Option<i32> {
        Some(LAYER_BLAZER)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "blazer",
            summary: "a lined tailored jacket: buttoned or open front, notched or shawl lapels, pocket flaps and a \
                      breast welt, a back vent, optional tails",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &BlazerParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let cut = p.cut();
        let frame = Frame::new(ctx.fit()?, &cut, super::coat::under(ctx)).map_err(fail)?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let tones = look::tones(ctx, &p.common, &p.colours)?;
        let mut shell = shell::build(&frame, &tones, out.tile()).map_err(fail)?;
        add_trims(&mut shell.mesh, &frame, p, &tones).map_err(fail)?;
        let places = button_places(&frame, p);
        if !places.is_empty() {
            let style = ButtonStyle {
                radius: p.buttons.radius,
                shank: 0.0,
                shape: ButtonShape::Flat,
                finish: p.buttons.finish.clone(),
            };
            let mut row = crate::kit::grid::MeshBuilder::new();
            trims::buttons(&mut row, &frame, &places, &style).map_err(fail)?;
            let line = ctx.colour(&p.colours.line)?;
            let (spec, class) = look::metal(ctx, &p.buttons.finish, line)?;
            let material = out.material(ctx, "buttons", spec)?;
            out.classify(&material, class);
            out.mesh(ctx, "buttons", row, material)?;
        }
        let sleeves = sleeves::build(&frame, &p.sleeve(), &tones, out.tile()).map_err(fail)?;
        super::coat::finish(ctx, out, &p.common, &p.colours, &frame, shell, Some(sleeves), &Self::regions(p))
    }
}

/// The pockets, the lapels and the vent, on the shell.
#[allow(clippy::many_single_char_names)] // the pockets' textbook names
fn add_trims(
    mesh: &mut crate::kit::grid::MeshBuilder,
    frame: &Frame,
    p: &BlazerParams,
    tones: &shell::Tones,
) -> Result<(), GarmentError> {
    trims::lapels(mesh, frame, &p.lapel, tones)?;
    let len = frame.column.length();
    if p.pockets.hip {
        let (y, phi, w, h, sag) = HIP_POCKET;
        // Above a cropped hem (which stays low on the torso, so they never reach the chest).
        let y = frame.torso(y).max(frame.hem + h * len + POCKET_ABOVE_HEM);
        for side in [phi, TAU - phi] {
            let pocket = Pocket { at: (y, side), size: (w * len, h * len), sag };
            trims::pocket(mesh, frame, &pocket, tones)?;
        }
    }
    if p.pockets.breast {
        let (y, least, w, h) = WELT;
        let y = frame.torso(y);
        // Clear of the left lapel.
        let r = trims::radius(frame, y);
        let lapel = if p.lapel.style == trims::LapelStyle::None { 0.0 } else { p.lapel.width / r };
        // Its far end short of the arm, narrowed rather than moved onto the lapel when the chest has no room.
        let half = (w * len / 2.0 / r).min(WELT_REACH / 4.0);
        let phi = least.max(frame.edge(y) + lapel + 0.1 + half).min(WELT_REACH - half);
        let size = (2.0 * half * r, h * len);
        trims::pocket(mesh, frame, &Pocket { at: (y, phi), size, sag: 0.0 }, tones)?;
    }
    if p.vent {
        let hem = frame.hem_at(BACK);
        let top = (hem + VENT * len).min(frame.waist() - 0.005);
        if top > hem + 0.02 {
            trims::stitch(mesh, frame, BACK, (hem + 0.004, top), tones.edge)?;
        }
    }
    Ok(())
}

/// Where the buttons go: down the front below the break, on the panel on top (the centre when buttoned).
fn button_places(frame: &Frame, p: &BlazerParams) -> Vec<(f64, f64)> {
    let len = frame.column.length();
    let start = trims::break_height(frame).unwrap_or_else(|| frame.torso(p.break_point)) - BUTTON_ROW.0 * len;
    let phi = match p.closure {
        Closure::Buttoned => 0.0,
        Closure::Open => p.opening + 0.08,
    };
    (0..p.buttons.count)
        .map(|k| (start - f64::from(k) * BUTTON_ROW.1 * len, phi))
        .take_while(|(y, _)| *y > frame.waist() + 0.01)
        .collect()
}

impl Garment for Blazer {
    fn common(p: &BlazerParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &BlazerParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso, BodyRegion::ArmL, BodyRegion::ArmR]
    }

    /// The shell's border, outside and in; an armhole in each; each lined sleeve's two ends, outside and in; two for
    /// each lapel and pocket.
    fn open_borders(p: &BlazerParams) -> usize {
        let lapels = if p.lapel.style == trims::LapelStyle::None || p.lapel.width <= 0.0 { 0 } else { 4 };
        let pockets = 4 * usize::from(p.pockets.hip) + 2 * usize::from(p.pockets.breast);
        2 + 4 + 8 + lapels + pockets
    }
}

omuzaki_char_api::register_part_kind!(Blazer);
