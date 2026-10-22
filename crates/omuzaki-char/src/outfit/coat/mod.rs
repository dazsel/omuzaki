//! The coat framework: the garments that run from the shoulders or the waist down over the hips (the dress, the blazer,
//! the cloak, the robe, the hoodie, the kimono), cut the same way.
//!
//! A coat is a **bodice** fitted over the torso from the waist seam up to the neckline (the rings of the upright
//! [`Column`](crate::outfit::shirt::frame::Column), with the ease), and a **skirt** hanging from the seam to the hem
//! ([`hang`]): enclosing the body, the earlier garments' skirts ([`reach`]) and, for a cape, the arms, never narrower
//! going down, flared toward the hem and kept clear of the hands. Both are one sheet of cloth with a lining a
//! thickness inside it ([`shell`]), its open edges covered by a rolled edge ([`edge`]). The front is closed, or open
//! with an edge that may cross over (a wrap) and open into a V toward the neck ([`Front`]). Sleeves are the shirt's,
//! wider, belled or lined as the kind asks, welded to the bodice ([`sleeves`]).
//!
//! The skirt is a grid whose first row is the waist seam: it is what [`drape`](crate::outfit::drape) simulates when
//! the garment's cloth is on, so a dress, a robe or a cloak swings while its bodice stays on the body. Every coat
//! publishes its reach below the waist ([`reach::HangReachFact`]), so the next layer clears it.

pub mod cloud;
pub mod edge;
pub mod hang;
pub mod look;
pub mod reach;
pub mod shell;
pub mod sleeves;

use omuzaki_face_api::Check;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use hang::{Arms, Fall};

/// The shape of a closed neckline at the front.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NeckShape {
    /// A round neck: a circular arc.
    #[default]
    Round,
    /// A V.
    V,
    /// A scoop: round, flatter at the bottom.
    Scoop,
    /// A square neck with soft corners.
    Square,
    /// No dip: the neck stands up round the throat (a crew or a high collar).
    High,
}

impl NeckShape {
    /// How deep the neckline is at `s`, the share of its half width from the middle (0) to its side (1): 1 at the
    /// bottom, 0 at the sides.
    pub fn dip(self, s: f64) -> f64 {
        if s >= 1.0 {
            return 0.0;
        }
        let s = s.abs();
        match self {
            Self::Round => (1.0 - s * s).sqrt(),
            Self::V => 1.0 - s,
            Self::Scoop => (1.0 - s.powi(4)).sqrt(),
            Self::Square => 1.0 - crate::kit::curve::sstep(0.8, 1.0, s),
            Self::High => 0.0,
        }
    }
}

/// A neckline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Neckline {
    /// Its shape at the front (a closed garment's).
    pub shape: NeckShape,
    /// How far it dips at the front, as a fraction of the torso's length.
    pub depth: f64,
    /// Its half width round the body at the front, radians.
    pub width: f64,
    /// How far it dips at the back, as a fraction of the torso's length.
    pub back: f64,
    /// How far up the neck the collar line stands above the torso's top, as a fraction of the torso's length (below it
    /// when negative: a strap's top on the shoulder).
    pub rise: f64,
}

impl Default for Neckline {
    fn default() -> Self {
        Self { shape: NeckShape::Round, depth: 0.08, width: 0.9, back: 0.02, rise: 0.02 }
    }
}

impl Neckline {
    /// Checks the neckline, its fields under `c`.
    pub fn check(&self, c: &mut Check) {
        c.range("depth", self.depth, 0.0, 0.6);
        c.range("width", self.width, 0.2, 1.5);
        c.range("back", self.back, 0.0, 0.4);
        c.range("rise", self.rise, -0.2, 0.25);
    }
}

/// The front of a coat.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Front {
    /// Closed all round (a seam at the back).
    Closed,
    /// Open at the front: the left panel's edge is `gap` radians to the left of the centre front (negative: past it
    /// to the right, over the right panel: a wrap), and the right panel's mirrors it. From `point` (a fraction of the
    /// torso) up, the edges open into a V that reaches `neck` radians either side at the collar.
    Open {
        /// The edges' angle below the V, radians.
        gap: f64,
        /// Where the V starts, as a fraction of the torso.
        point: f64,
        /// The edges' angle at the collar, radians.
        neck: f64,
    },
}

/// How a coat is cut: what each kind fills in from its parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct CoatCut {
    /// The superellipse exponent of the rings.
    pub exponent: f64,
    /// How far the lining stands off the layer below, metres.
    pub ease: f64,
    /// How far the outside stands over the lining, metres.
    pub thickness: f64,
    /// The waist seam, as a fraction of the torso: the bodice above, the skirt below (1 hangs the whole coat from the
    /// neck, a cape).
    pub waist: f64,
    /// The front.
    pub front: Front,
    /// The neckline.
    pub neckline: Neckline,
    /// How far down from the torso's top the armholes' floor is, as a fraction of the torso; 0 for none (sleeves or a
    /// cape).
    pub armhole: f64,
    /// Where the hem is: 0 at the hips' joints, 1 at the ankles (negative: above the hips).
    pub length: f64,
    /// How much longer the back is than the front, metres.
    pub back_drop: f64,
    /// How the skirt falls.
    pub fall: Fall,
    /// How much wider the skirt is at its lowest, metres.
    pub flare: f64,
    /// Soft folds round the skirt: how many, and how far they stand out at the hem, metres.
    pub folds: (usize, f64),
    /// The arms.
    pub arms: Arms,
    /// The rolled edge's lip: how far it stands beyond the cloth's thickness, metres.
    pub lip: f64,
}

/// The lowest a hem may reach: just above the ankles.
pub const MAX_LENGTH: f64 = 0.95;
/// The highest a hem may be: a cropped coat, above the hips' joints.
pub const MIN_LENGTH: f64 = -0.25;

use omuzaki_char_api::{BodyRegion, BuildCtx, BuildError, PartOutput};
use omuzaki_core::glam::DVec2;

use self::reach::{HangReach, HangReachFact};
use self::shell::{Frame, Shell};
use self::sleeves::Sleeves;
use super::common::{GarmentCommon, GarmentOutput};

/// The reach of the garments under the one `ctx` builds.
pub fn under(ctx: &BuildCtx<'_>) -> Option<HangReach> {
    ctx.published.get::<HangReachFact>().clone()
}

/// Finishes a coat: adds the shell (`<part>-shell`, in the cloth) and the sleeves (`<part>-sleeves`) to `out` after
/// whatever the kind added, publishes the envelope over `regions`, tints and welds the sleeves, publishes the coat's
/// reach below the waist and, when its cloth is on, simulates the skirt.
///
/// # Errors
/// As [`GarmentOutput::finish`], [`sleeves::weld`] and [`drape::simulate`](super::drape::simulate).
#[allow(clippy::too_many_arguments)] // the pieces of one garment, each used once
pub fn finish(
    ctx: &BuildCtx<'_>,
    mut out: GarmentOutput,
    common: &GarmentCommon,
    colours: &look::CoatColours,
    frame: &Frame,
    shell: Shell,
    sleeves: Option<Sleeves>,
    regions: &[BodyRegion],
) -> Result<PartOutput, BuildError> {
    let tones = look::tones(ctx, common, colours)?;
    let Shell { skirt, cols, mesh, bodice_vertices, .. } = shell;
    let reach_points: Vec<_> = skirt.outer.iter().map(|p| p.as_vec3()).collect();
    out.surface(ctx, "shell", mesh)?;
    let sleeves = match sleeves {
        Some(sleeves) => {
            let mesh = sleeves.mesh.clone();
            out.surface(ctx, "sleeves", mesh)?;
            Some(sleeves)
        }
        None => None,
    };
    let textured = out.tile().is_some();
    let mut output = out.finish(ctx, common, regions, frame.cut.exponent)?;
    let name = format!("{}-fabric", ctx.part);
    let cloth = look::cloth(ctx, colours, textured)?;
    for (material, spec) in &mut output.materials {
        if material.as_str() == name {
            *spec = cloth.clone();
        }
    }
    if let Some(sleeves) = &sleeves {
        self::sleeves::tint(&mut output, ctx, sleeves, &tones)?;
        self::sleeves::weld(&mut output, ctx, frame, &bodice_vertices, sleeves)?;
    }
    let axis = DVec2::new(frame.table.ring(frame.waist()).centre.x, frame.table.ring(frame.waist()).centre.z);
    if let Some(reach) = HangReach::of(axis, &reach_points, under(ctx).as_ref()) {
        output.publishes.put::<HangReachFact>(reach);
    }
    let shell_name = format!("{}-shell", ctx.part);
    let index =
        output.meshes.iter().position(|m| m.name == shell_name).ok_or_else(|| ctx.fail("the coat has no shell"))?;
    let drape = super::drape::Drape { mesh: index, start: 0, rows: skirt.rows, cols, pinned_first: true, pin_rows: 1 };
    super::drape::simulate(&common.cloth, &drape, ctx, &mut output)?;
    Ok(output)
}
