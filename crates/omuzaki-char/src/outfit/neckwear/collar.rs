//! The pointed collar: a double-skinned stand that rises from the shirt's neck opening and folds out over the shirt, with
//! ink lines along its free edges (`uniform.js` `buildCollar`, 148-198).
//!
//! The collar is built for the left side, as a grid of 15 rows by 45 columns, and mirrored into one grid of 89 columns. A column `a` is an angle
//! round the neck, from the back (`a = 0`, at `ph = pi`) to the front point; a row `v` runs from the stand's free edge
//! (`v = 0`, a circle round the neck at the stand's height) down to the collar's lower edge laid on the shirt (`v = 1`,
//! 3 mm over its surface). The collar ends at an angle `phEnd(v)` that opens from `spread` at the free edge to
//! `spread + points` at the shirt, so the two halves leave a V in front. A swell along the middle rows rolls the collar
//! outwards. The skin is two shells, one a millimetre either side of that surface, the inner one turned inside out.
//!
//! Where the prototype's numbers were fitted to its own neck they are expressed against the shirt it hangs on: the ring's
//! radius is the shirt's opening plus a flare, the stand rises from the opening, and the lower edge follows the shirt's
//! surface at fractions of its torso's length.

use omuzaki_char_api::{
    BuildCtx, BuildError, JointRef, MaterialName, MaterialSpec, PartCategory, PartColours, PartKind, PartMeta,
    PartOutput, RecolourCtx, Recoloured,
};
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::Shading;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Hang, hang, mesh_of, name_of};
use crate::kit::curve::{Pchip, fraction, lerp, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin, grid_offset};
use crate::kit::sweep::tube;
use crate::outfit::common::{GarmentError, recolour_named};
use crate::outfit::shirt::cloth_look;
use crate::outfit::shirt::torso::along;

/// Rows of the collar's grid, from the free edge to the shirt, and columns, from the back to the front point.
pub const ROWS: usize = 14;
/// See [`ROWS`].
pub const COLUMNS: usize = 44;
/// How far the stand's circle is wider than the shirt's opening, metres.
pub const FLARE: f64 = 0.0045;
/// How far the stand dips towards the front: the circle is this much lower at the front than at the back, metres.
pub const STAND_DIP: f64 = 0.0075;
/// How far the lower edge lies over the shirt's surface, metres, and how much higher than that surface in `y`.
pub const LOWER_LIFT: f64 = 0.0030;
/// See [`LOWER_LIFT`].
pub const LOWER_RAISE: f64 = 0.0006;
/// The swell that rolls the collar outwards: its height in metres, the exponent of its fall-off, and how much of it is
/// lost towards the front.
pub const BULGE: f64 = 0.0030;
/// See [`BULGE`].
pub const BULGE_POWER: f64 = 0.9;
/// See [`BULGE`].
pub const BULGE_FRONT_LOSS: f64 = 0.5;
/// The swell reaches its peak at `v = 1 / BULGE_REACH` and is over by `v = 1`.
pub const BULGE_REACH: f64 = 1.1;
/// The exponent of `v` in how fast the V widens towards the shirt.
pub const OPENING_POWER: f64 = 1.3;
/// The collar's lower edge on the shirt: the angle `ph` round the neck (`pi` at the back) and the prototype's height there
/// on the soft-sd torso, metres. The lower edge rises from the front to the back of the neck.
pub const LOWER_EDGE: [(f64, f64); 6] =
    [(0.45, 0.5100), (0.8, 0.5150), (1.2, 0.5225), (1.57, 0.5295), (2.3, 0.5365), (std::f64::consts::PI, 0.5405)];
/// How much the collar darkens towards the shirt (outer shell), and the inner shell's colour.
pub const OUTER_SHADE: f64 = 0.05;
/// See [`OUTER_SHADE`].
pub const INNER_COLOUR: [f32; 4] = [0.86, 0.85, 0.90, 1.0];
/// Where the collar's skin starts to hand over from the chest to the neck bone: the prototype's height on the soft-sd
/// torso, metres, and how far over the torso's top it is complete.
pub const NECK_BLEND_START: f64 = 0.53;
/// See [`NECK_BLEND_START`].
pub const NECK_BLEND_RISE: f64 = 0.023;
/// The neck bone's greatest share of the collar.
pub const NECK_SHARE: f64 = 0.55;
/// The ink lines: radius, sides.
pub const LINE: (f64, usize) = (0.00058, 5);
/// How far the collar's surface stays off the shirt at least, beyond its half thickness, metres.
pub const MIN_GAP: f64 = 0.0015;
/// The most the collar's skin may be told to be thick: a shell is this many metres either side of the surface at most.
const MAX_THICKNESS: f64 = 0.003;

/// The shape of the collar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct CollarStyle {
    /// How high the stand's free edge rises above the shirt's neck opening, metres.
    pub stand: f64,
    /// How far apart the collar's points sit at the stand's free edge, as the angle in radians each stops short of the
    /// front of the neck.
    pub spread: f64,
    /// How much further apart they sit at the shirt: the V that opens from the free edge to the lower edge, radians.
    pub points: f64,
    /// Half the collar's thickness, metres: each of its two shells is this far from its surface.
    pub thickness: f64,
}

impl Default for CollarStyle {
    fn default() -> Self {
        Self { stand: 0.0065, spread: 0.15, points: 0.42, thickness: 0.0010 }
    }
}

impl Validate for CollarStyle {
    fn check(&self, c: &mut Check) {
        c.range("stand", self.stand, 0.002, 0.03);
        c.range("spread", self.spread, 0.0, 1.0);
        c.range("points", self.points, 0.0, 1.0);
        c.range("thickness", self.thickness, 0.0003, MAX_THICKNESS);
    }
}

/// The collar's colours.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct CollarColours {
    /// The cloth: the shirt's colour, so the collar reads as part of the shirt.
    pub cloth: ColourRef,
    /// The cloth in shadow.
    pub shade: ColourRef,
    /// The outline and the ink lines on the free edges.
    pub line: ColourRef,
}

impl Default for CollarColours {
    fn default() -> Self {
        Self {
            cloth: ColourRef::rgb(0xf5, 0xf2, 0xed),
            shade: ColourRef::rgb(0xc6, 0xc2, 0xd6),
            line: ColourRef::rgb(0x8f, 0x8a, 0x9c),
        }
    }
}

impl Validate for CollarColours {
    fn check(&self, _: &mut Check) {}
}

/// The parameters of a collar.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct CollarParams {
    /// Its shape.
    pub style: CollarStyle,
    /// Its colours.
    pub colours: CollarColours,
}

impl Validate for CollarParams {
    fn check(&self, c: &mut Check) {
        c.nested("style", &self.style);
        c.nested("colours", &self.colours);
    }
}

/// The `collar` part kind.
pub struct Collar;

impl PartKind for Collar {
    type Params = CollarParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "collar",
            summary: "Build a pointed collar on a shirt's neck opening.",
            category: PartCategory::Garment,
            requires: &["shirt"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &CollarParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let on = hang(ctx)?;
        let (shell, mut lines) = half(&on, &p.style).map_err(fail)?;
        lines.merge(&lines.mirrored()).map_err(|e| fail(e.into()))?;

        let cloth_name: MaterialName = name_of(ctx, "cloth")?;
        let lines_name: MaterialName = name_of(ctx, "lines")?;
        let (cloth, ink) = materials(p, ctx)?;
        let mut line_mesh = mesh_of(ctx, "lines", lines, lines_name.clone())?;
        line_mesh.casts_shadow = false;
        Ok(PartOutput::new()
            .with_meshes(vec![mesh_of(ctx, "shell", shell, cloth_name.clone())?, line_mesh])
            .with_materials(vec![(cloth_name, cloth), (lines_name, ink)]))
    }

    fn recolour(p: &CollarParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        let (cloth, ink) = materials(p, ctx)?;
        Ok(recolour_named(output, ctx.part, vec![("cloth", cloth), ("lines", ink)], Vec::new()))
    }
}

omuzaki_char_api::register_part_kind!(Collar);

/// The collar's cloth (the shirt's cloth look, so the two read as one) and its ink lines, for the build and the
/// recolour.
fn materials<C: PartColours>(p: &CollarParams, ctx: &C) -> Result<(MaterialSpec, MaterialSpec), BuildError> {
    let mut cloth = MaterialSpec::toon(ctx.colour(&p.colours.cloth)?);
    cloth_look(&mut cloth, ctx.colour(&p.colours.shade)?, ctx.colour(&p.colours.line)?);
    let mut ink = MaterialSpec::toon(ctx.colour(&p.colours.line)?);
    ink.shading = Shading::Unlit;
    Ok((cloth, ink))
}

/// The number of open borders the collar's meshes have: the outer and inner shell's outlines, and the ends of the ink
/// lines (a tube tapers to a hair's width, not to a point; the two lower lines meet at the back and share an end).
pub const OPEN_BORDERS: usize = 2 + 6;

/// The angle the collar stops at, `ph`, at row `v`: how far the two points sit from the front of the neck.
fn ph_end(style: &CollarStyle, v: f64) -> f64 {
    style.spread + style.points * v.powf(OPENING_POWER)
}

/// The skin of the collar at height `y`: chest below, handing over to the neck bone towards the top of the stand.
#[allow(clippy::cast_possible_truncation)] // weights are in 0..=1; the mesh stores f32
fn skin_at(on: &Hang, y: f64) -> Vec<(JointRef, f32)> {
    let height = |reference: f64| on.shirt.bottom() + along(reference) * on.shirt.length();
    let neck = NECK_SHARE * sstep(height(NECK_BLEND_START), on.shirt.top() + NECK_BLEND_RISE, y);
    vec![(JointRef::CHEST, (1.0 - neck) as f32), (JointRef::NECK, neck as f32)]
}

/// The collar's two shells in one builder, and the ink lines of its left half in another (the caller mirrors them).
///
/// The shells are one grid of `2 * COLUMNS + 1` columns, the right half the mirror image of the left, so the collar's
/// back is one smooth surface and its normals there are the same for both halves.
fn half(on: &Hang, style: &CollarStyle) -> Result<(MeshBuilder, MeshBuilder), GarmentError> {
    let stand_radius = on.opening_radius + FLARE;
    let centre_z = on.ring.z;
    let lower_edge = Pchip::new(
        &LOWER_EDGE.iter().map(|&(ph, y)| (ph, on.shirt.bottom() + along(y) * on.shirt.length())).collect::<Vec<_>>(),
    )?;
    // The left half's point at row `v` and column `a` (0 at the back, 1 at the front point).
    let point = |v: f64, a: f64| -> Result<DVec3, GarmentError> {
        let ph = lerp(std::f64::consts::PI, ph_end(style, v), a);
        let top = on.ring.y + style.stand - STAND_DIP * (1.0 - ph / std::f64::consts::PI).powi(2);
        let free = DVec3::new(stand_radius * ph.sin(), top, centre_z + stand_radius * ph.cos());
        let on_shirt = on.shirt.point(lower_edge.eval(ph), ph, LOWER_LIFT)?;
        let lower = DVec3::new(on_shirt.x, on_shirt.y + LOWER_RAISE, on_shirt.z);
        let swell = BULGE
            * (std::f64::consts::PI * (BULGE_REACH * v).min(1.0)).sin().powf(BULGE_POWER)
            * (1.0 - BULGE_FRONT_LOSS * a);
        let mut p = free.lerp(lower, v);
        let outward = DVec3::new(p.x, 0.0, p.z - centre_z).try_normalize().unwrap_or(DVec3::Z);
        p += outward * swell;
        // A straight line from the stand to the shirt can dip into a shirt that bulges between them (a short, wide
        // neck): such a point is lifted to leave room for the collar's skin.
        let mut p = on.shirt.lift(p, style.thickness + MIN_GAP)?;
        // The back of the collar is on the middle exactly, so the two halves are one surface.
        if a <= 0.0 {
            p.x = 0.0;
        }
        Ok(p)
    };
    let mut left: Vec<Vec<DVec3>> = Vec::with_capacity(ROWS + 1);
    for iv in 0..=ROWS {
        let v = fraction(iv, ROWS);
        left.push((0..=COLUMNS).map(|ia| point(v, fraction(ia, COLUMNS))).collect::<Result<_, _>>()?);
    }
    let rows: Vec<Vec<DVec3>> = left
        .iter()
        .map(|row| {
            let right = row.iter().skip(1).rev().map(|p| DVec3::new(-p.x, p.y, p.z));
            right.chain(row.iter().copied()).collect()
        })
        .collect();
    let surface = Grid::from_rows(&rows)?;
    let inside = |i: usize, j: usize| DVec3::new(0.0, surface.at(i, j).y, centre_z);
    let skin = |p: DVec3, _: usize, _: usize| skin_at(on, p.y);
    let place = GridOptions { inside: Some(&inside), skin: Skin::Per(&skin), ..GridOptions::default() };
    // Each shell is lifted off the shirt on its own: the skin's offset along the surface's normal can go into a shirt that
    // slopes steeply (a neck that widens towards the chin), by more than the surface's own clearance allows.
    let shell_of = |distance: f64| -> Result<Grid, GarmentError> {
        let grid = grid_offset(&surface, distance, &place)?;
        let points = grid.points().iter().map(|&p| on.shirt.lift(p, MIN_GAP)).collect::<Result<Vec<_>, _>>()?;
        Ok(Grid::new(grid.rows(), grid.cols(), points)?)
    };
    let outer = shell_of(style.thickness)?;
    let inner = shell_of(-style.thickness)?;
    let outer_shade = |i: usize, _: usize, _: DVec3| {
        #[allow(clippy::cast_possible_truncation)] // a shade in 0..=1
        let k = (1.0 - OUTER_SHADE * fraction(i, ROWS)) as f32;
        [k, k, k, 1.0]
    };
    let inner_shade = |_: usize, _: usize, _: DVec3| INNER_COLOUR;
    let mut shell = MeshBuilder::with_colours();
    shell.add_grid(&outer, &GridOptions { colour: Some(&outer_shade), ..place.clone() })?;
    shell.add_grid(&inner, &GridOptions { colour: Some(&inner_shade), invert: true, ..place })?;

    // Ink lines along the lower edge and then the front edge, on the mid surface.
    let lower: Vec<DVec3> = left[ROWS].clone();
    let front: Vec<DVec3> = left.iter().map(|r| r[COLUMNS]).collect();
    let mut lines = MeshBuilder::new();
    for path in [lower, front] {
        let swept = tube(&path, |_| LINE.0, LINE.1)?;
        let centre = |i: usize, _: usize| swept.centres[i];
        let skin = |p: DVec3, _: usize, _: usize| skin_at(on, p.y);
        let options =
            GridOptions { wrap_j: true, inside: Some(&centre), skin: Skin::Per(&skin), ..GridOptions::default() };
        lines.add_grid(&swept.grid, &options)?;
    }
    Ok((shell, lines))
}
