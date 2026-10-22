//! The `vest` garment: a fitted V-neck waistcoat over the shirt (#323), the prototype's `buildVest`.
//!
//! One mesh in the prototype's order: the outer shell and the lining (the same grid pushed in), piping round the rim
//! and the hem, thin raised seams; and a second mesh of gold buttons down the centre front. Every height is a fraction
//! of the torso region ([`profile`]), so the vest follows whatever torso the fit has, and it sits `ease` outside the
//! layer below (the shirt, or the body) with its outer shell [`thickness`](VestParams::thickness) further out.
//!
//! The vest is layer [`LAYER_VEST`], over the shirt and the tie. It publishes the envelope of its outer shell, so a
//! blazer fits over it.

mod pieces;
pub mod profile;

use omuzaki_char_api::{
    BodyRegion, BuildCtx, BuildError, MaterialClass, MaterialSpec, PartCategory, PartKind, PartMeta, PartOutput,
    TextureName,
};
use omuzaki_core::colour::Colour;
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::{Outline, Rim, Shading, Toon};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use self::pieces::{SEAM_LIFT, Surfaces, add_piping, add_seams, add_shell, buttons};
use self::profile::{ARMHOLE_DEPTH, HEM_BACK, HEM_FRONT, HEM_SIDE, MIN_PANEL, NECK_DEPTH, Profile, RIM_HEIGHT};
use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, LAYER_VEST, MAX_EASE};
use super::trim::buttons::{ButtonShape, ButtonStyle, GOLD_RADIUS, MAX_BUTTONS};
use super::trim::{Finish, feature};
use crate::kit::grid::MeshBuilder;

/// The cut of the torso sections: slightly squarer than an ellipse (the prototype's `TE`).
pub const VEST_EXPONENT: f64 = 2.15;
/// The default ease: the lining sits this far outside the layer below (the prototype's inner offset, 4.2 mm).
pub const VEST_EASE: f64 = 0.0042;
/// How thick the cloth is by default, metres: the outer shell stands this far outside the lining (2.8 mm).
pub const VEST_THICKNESS: f64 = 0.0028;
/// The thinnest cloth, metres: under this the shell and the lining weld into one sheet.
pub const MIN_THICKNESS: f64 = 0.0005;
/// The thickest cloth, metres.
pub const MAX_THICKNESS: f64 = 0.02;
/// The default buttons' spacing along the torso, metres.
pub const BUTTON_SPACING: f64 = 0.026;
/// The first button's place on the centre front, as a fraction of the way from the hem tip to the V tip (the
/// prototype's `0.408` m on the soft-sd body).
pub const BUTTON_FIRST: f64 = 0.75;
/// How many buttons by default.
pub const BUTTON_COUNT: u32 = 3;
/// The cloth's default colour (the prototype's near-black).
const CLOTH: ColourRef = ColourRef::rgb(0x23, 0x20, 0x28);
/// The cloth's shadow colour.
const SHADE: u32 = 0x007d_7390;
/// The cloth's outline colour and width (metres at 2 m).
const LINE: (u32, f32) = (0x000f_0d12, 0.0017);
/// The cloth's terminator shift and softness.
const TOON_SHIFT_SOFTNESS: (f32, f32) = (-0.08, 0.22);
/// The cloth's rim light colour.
const RIM: u32 = 0x005a_6699;
/// The cloth's rim light strength and power.
const RIM_STRENGTH_POWER: (f32, f32) = (0.20, 2.4);
/// The buttons' outline colour and width.
const GOLD_LINE: (u32, f32) = (0x005a_4020, 0.0009);
/// Open borders of the shell: the rim and the hem, on the outside and on the lining.
const SHELL_BORDERS: usize = 4;

/// The hem's heights, as fractions of the torso region.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct HemSpec {
    /// At the centre front, the point of the hem.
    pub front: f64,
    /// At the sides, where the front and back curves meet.
    pub side: f64,
    /// At the centre back.
    pub back: f64,
}

impl Default for HemSpec {
    fn default() -> Self {
        Self { front: HEM_FRONT, side: HEM_SIDE, back: HEM_BACK }
    }
}

/// The gold buttons down the centre front.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ButtonsSpec {
    /// How many, 0 for none to [`MAX_BUTTONS`].
    pub count: u32,
    /// Each one's radius at its foot, metres.
    pub radius: f64,
    /// From one to the next along the torso, metres.
    pub spacing: f64,
    /// Where the first stands: the fraction of the way from the hem's point (0) up to the V's tip (1).
    pub first: f64,
    /// What they are made of (gold unless set).
    pub finish: Finish,
}

/// The prototype's gold: its colour and the library's metal band highlight.
fn gold() -> Finish {
    Finish { colour: ColourRef::rgb(0xdc, 0xb6, 0x6c), ..Finish::default() }
}

impl Default for ButtonsSpec {
    fn default() -> Self {
        Self { count: BUTTON_COUNT, radius: GOLD_RADIUS, spacing: BUTTON_SPACING, first: BUTTON_FIRST, finish: gold() }
    }
}

impl Validate for ButtonsSpec {
    fn check(&self, c: &mut Check) {
        if self.count > MAX_BUTTONS {
            c.fail("count", format!("{}: a vest has 0 to {MAX_BUTTONS} buttons", self.count));
        }
        feature(c, "radius", self.radius);
        feature(c, "spacing", self.spacing);
        c.range("first", self.first, 0.0, 1.0);
        c.nested("finish", &self.finish);
    }
}

/// The parameters of a vest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct VestParams {
    /// The common garment settings. `ease` is how far the lining sits outside the layer below.
    pub common: GarmentCommon,
    /// How thick the cloth is, metres: the outer shell stands this far outside the lining.
    pub thickness: f64,
    /// The rim's height at the shoulders, as a fraction of the torso region.
    pub rim: f64,
    /// How far the V's tip is below the rim, as a fraction of the torso region.
    pub neck_depth: f64,
    /// The hem.
    pub hem: HemSpec,
    /// How far the armholes cut down from the rim at the sides, as a fraction of the torso region; 0 for none. The arm
    /// hangs against the side of the torso, so a vest without armholes goes through it.
    pub armhole: f64,
    /// Piping round the rim and the hem.
    pub piping: bool,
    /// Raised seams: the centre front, two darts, the sides and the back.
    pub seams: bool,
    /// The buttons.
    pub buttons: ButtonsSpec,
}

impl Default for VestParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: VEST_EASE,
                fabric: FabricStyle::Plain { colour: CLOTH },
                ..GarmentCommon::default()
            },
            thickness: VEST_THICKNESS,
            rim: RIM_HEIGHT,
            neck_depth: NECK_DEPTH,
            hem: HemSpec::default(),
            armhole: ARMHOLE_DEPTH,
            piping: true,
            seams: true,
            buttons: ButtonsSpec::default(),
        }
    }
}

impl VestParams {
    /// The outline these parameters cut.
    pub fn profile(&self) -> Profile {
        Profile {
            rim: self.rim,
            neck_depth: self.neck_depth,
            hem_front: self.hem.front,
            hem_side: self.hem.side,
            hem_back: self.hem.back,
            armhole: self.armhole,
        }
    }

    /// How far the outer shell stands outside the layer below, metres.
    pub fn outer_ease(&self) -> f64 {
        self.common.ease + self.thickness
    }
}

impl Validate for VestParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.range("thickness", self.thickness, MIN_THICKNESS, MAX_THICKNESS);
        if self.outer_ease().is_finite() && self.outer_ease() + SEAM_LIFT > MAX_EASE {
            c.fail(
                "thickness",
                format!(
                    "ease and thickness together stand out {} m: at most {}",
                    self.outer_ease(),
                    MAX_EASE - SEAM_LIFT
                ),
            );
        }
        c.range("rim", self.rim, 0.3, 1.0);
        c.range("neck_depth", self.neck_depth, 0.0, 0.6);
        for (name, hem) in [("hem/front", self.hem.front), ("hem/side", self.hem.side), ("hem/back", self.hem.back)] {
            c.range(name, hem, 0.0, 0.6);
        }
        c.range("armhole", self.armhole, 0.0, 0.7);
        c.nested("buttons", &self.buttons);
        let panel = self.profile().panel();
        if panel.is_finite() && panel < MIN_PANEL {
            c.fail(
                "outline",
                format!(
                    "the V's tip, the back of the neck and the armholes leave only {panel} of the torso above the hem: at \
                     least {MIN_PANEL}"
                ),
            );
        }
    }
}

/// The vest part kind.
pub struct Vest;

impl PartKind for Vest {
    type Params = VestParams;

    fn layer() -> Option<i32> {
        Some(LAYER_VEST)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "vest",
            summary: "a fitted V-neck vest with piping, seams and gold buttons, over the shirt",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &VestParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let fit = ctx.fit()?;
        let cut = |offset: f64| super::loft::Surface::new(fit.clone(), BodyRegion::Torso, VEST_EXPONENT, offset);
        let surfaces = Surfaces {
            outer: cut(p.outer_ease()).map_err(fail)?,
            inner: cut(p.common.ease).map_err(fail)?,
            mid: cut(p.common.ease + p.thickness / 2.0).map_err(fail)?,
            seam: cut(p.outer_ease() + SEAM_LIFT).map_err(fail)?,
        };
        let profile = p.profile();
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let mut mesh = MeshBuilder::with_colours();
        add_shell(&mut mesh, &profile, &surfaces, out.tile()).map_err(fail)?;
        if p.piping {
            add_piping(&mut mesh, &profile, &surfaces.mid).map_err(fail)?;
        }
        if p.seams {
            add_seams(&mut mesh, &profile, &surfaces.seam).map_err(fail)?;
        }
        let shell = out.material(ctx, "shell", shell_material(ctx, &p.common, out.tile().is_some())?)?;
        out.mesh(ctx, "vest", mesh, shell)?;
        if p.buttons.count > 0 {
            let length = {
                let (bottom, top) = (surfaces.outer.ring(0.0).map_err(fail)?, surfaces.outer.ring(1.0).map_err(fail)?);
                bottom.centre.distance(top.centre)
            };
            let style = ButtonStyle {
                radius: p.buttons.radius,
                shank: 0.0,
                shape: ButtonShape::Dome,
                finish: p.buttons.finish.clone(),
            };
            let row =
                buttons(&surfaces.outer, &profile, length, p.buttons.count, p.buttons.spacing, p.buttons.first, &style)
                    .map_err(|e| ctx.fail(format!("buttons: {e}")))?;
            let gold = gold_material(ctx, &p.buttons.finish)?;
            let material = out.material(ctx, "gold", gold.0)?;
            out.classify(&material, gold.1);
            out.mesh(ctx, "buttons", row, material)?;
        }
        // The next layer fits over the outside of the shell, not the lining.
        let outer = GarmentCommon { ease: p.outer_ease(), ..p.common.clone() };
        out.finish(ctx, &outer, &[BodyRegion::Torso], VEST_EXPONENT)
    }
}

/// The cloth's material: the fabric's colour (or its texture, which [`GarmentOutput::new`] painted as
/// `<part>-fabric`), with the prototype's toon settings and the vertex colours that shade it.
fn shell_material(ctx: &BuildCtx<'_>, common: &GarmentCommon, textured: bool) -> Result<MaterialSpec, BuildError> {
    let mut spec = match (&common.fabric, textured) {
        (FabricStyle::Plain { colour }, false) => MaterialSpec::toon(ctx.colour(colour)?),
        (_, true) => {
            let name = format!("{}-fabric", ctx.part.as_str());
            let texture =
                name.parse::<TextureName>().map_err(|e| ctx.fail(format!("`{name}` isn't a texture name: {e}")))?;
            MaterialSpec { base_texture: Some(texture), ..MaterialSpec::toon(Colour::rgb(255, 255, 255)) }
        }
        (_, false) => return Err(ctx.fail("a patterned fabric painted no tile")),
    };
    spec.shading = Shading::Toon(Toon {
        shade: Colour::hex(SHADE),
        shift: TOON_SHIFT_SOFTNESS.0,
        softness: TOON_SHIFT_SOFTNESS.1,
        rim: Rim { colour: Colour::hex(RIM), strength: RIM_STRENGTH_POWER.0, power: RIM_STRENGTH_POWER.1 },
        ..Toon::default()
    });
    spec.vertex_colours = true;
    spec.outline = Some(Outline { colour: Colour::hex(LINE.0), width: LINE.1 });
    Ok(spec)
}

/// The buttons' material: metal with the `band-highlight` program when the finish says so, as every trim.
fn gold_material(ctx: &BuildCtx<'_>, finish: &Finish) -> Result<(MaterialSpec, MaterialClass), BuildError> {
    let base = ctx.colour(&finish.colour)?;
    let (mut spec, class) = match &finish.metal {
        Some(metal) => {
            let highlight = ctx.colour(&metal.highlight)?;
            (metal.material(base, highlight).map_err(|e| ctx.fail(format!("buttons: {e}")))?, MaterialClass::Metal)
        }
        None => (MaterialSpec::toon(base), MaterialClass::Cloth),
    };
    spec.outline = Some(Outline { colour: Colour::hex(GOLD_LINE.0), width: GOLD_LINE.1 });
    Ok((spec, class))
}

impl Garment for Vest {
    fn common(params: &VestParams) -> &GarmentCommon {
        &params.common
    }

    fn regions(_: &VestParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso]
    }

    fn open_borders(_: &VestParams) -> usize {
        SHELL_BORDERS
    }
}

omuzaki_char_api::register_part_kind!(Vest);

/// The layer the vest is built on: over the shirt and the tie.
pub const LAYER: i32 = LAYER_VEST;
