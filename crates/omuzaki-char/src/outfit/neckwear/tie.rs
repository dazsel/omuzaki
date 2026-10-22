//! The striped tie: a blade that hangs down the shirt's front to a point, and the knot at its top (`uniform.js`
//! `buildTie`, 199-232, and `tieTexture`, 402-416).
//!
//! The blade is a ribbon: a ring of 27 points at each of 12 heights whose half-width follows the prototype's table down
//! to the shoulder and then tapers straight to the point, bowed back at its sides (`-4.4 x^2`) so it wraps the shirt, and
//! 3.2 mm thick. The knot is 16 rings of a rounded trapezoid with a soft dimple, deeper in front than behind. Both lie on
//! the shirt's front and are textured by position: the stripe tile is 36 mm wide and 28 mm high wherever the tie hangs.
//!
//! The prototype's heights are the soft-sd torso's; they are fitted to the shirt's torso as fractions of its length
//! ([`along`]). The tie's length and width are parameters that stretch the whole tie from its top and its middle.

use omuzaki_char_api::{
    BuildCtx, BuildError, JointRef, MaterialName, MaterialSpec, PartCategory, PartKind, PartMeta, PartOutput,
    TextureData, TextureName,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DVec2, DVec3, UVec2, Vec2};
use omuzaki_face_api::{Check, Cmd, ColourRef, DrawList, FaceRect, Paint, Validate};
use omuzaki_render_api::{Filter, Outline, Rim, Sampler, Shading, TextureDesc, Toon, Wrap};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Hang, hang, mesh_of, name_of};
use crate::kit::curve::{Pchip, TAU, fraction, rows_of, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::outfit::common::GarmentError;
use crate::outfit::shirt::torso::{along, to_reference};

/// Segments round the blade and the knot: columns are one more.
pub const SEGMENTS: usize = 26;
/// The blade's rows from its top to its point: the prototype's heights on the soft-sd torso, metres.
pub const BLADE_ROWS: [f64; 12] = [0.502, 0.497, 0.49, 0.48, 0.47, 0.455, 0.44, 0.43, 0.424, 0.418, 0.414, 0.4125];
/// The blade's half-width above the shoulder: knots of `(height, half-width)`, metres.
pub const BLADE_WIDTH: [(f64, f64); 5] =
    [(0.43, 0.0150), (0.45, 0.0153), (0.47, 0.0140), (0.485, 0.0110), (0.500, 0.0075)];
/// The height of the blade's shoulder (where it is widest and the taper begins) and of its point.
pub const BLADE_SHOULDER: f64 = 0.43;
/// See [`BLADE_SHOULDER`].
pub const BLADE_TIP: f64 = 0.4125;
/// The blade's half-thickness, and how far its back lies off the shirt, metres.
pub const BLADE_THICKNESS: f64 = 0.0016;
/// See [`BLADE_THICKNESS`].
pub const BLADE_STANDOFF: f64 = 0.0006;
/// How fast the blade bows back from its middle: `z` falls by this times `x^2`.
pub const BLADE_BOW: f64 = 4.4;
/// The half-width below which the blade's thickness thins away, metres.
pub const BLADE_THIN: f64 = 0.004;
/// The knot's row spans `(from, to, rows)`: the prototype's heights, metres.
pub const KNOT_ROWS: [(f64, f64, usize); 3] = [(0.489, 0.497, 4), (0.497, 0.520, 8), (0.520, 0.527, 3)];
/// The knot's radius: knots of `(height, radius)`, metres.
pub const KNOT_RADIUS: [(f64, f64); 6] =
    [(0.489, 0.0070), (0.495, 0.0125), (0.503, 0.0150), (0.512, 0.0134), (0.520, 0.0114), (0.527, 0.0080)];
/// The knot's dimple: its height, depth as a fraction of the radius, and width, metres.
pub const KNOT_DIMPLE: (f64, f64, f64) = (0.515, 0.07, 0.003);
/// The knot's depth in front of the shirt and behind its middle, and how far it stands off the shirt, metres.
pub const KNOT_DEPTH: f64 = 0.0085;
/// See [`KNOT_DEPTH`].
pub const KNOT_BACK: f64 = 0.55;
/// See [`KNOT_DEPTH`].
pub const KNOT_STANDOFF: f64 = 0.0048;
/// See [`KNOT_DEPTH`].
pub const KNOT_SINK: f64 = 0.0005;
/// The tile of the stripe texture on the model, metres: its width and its height.
pub const TILE: (f64, f64) = (0.036, 0.028);
/// How far inside the shirt the rings' interior points sit (for orienting the normals), metres.
pub const BLADE_INSIDE: f64 = -0.01;
/// See [`BLADE_INSIDE`].
pub const KNOT_INSIDE: f64 = -0.002;
/// The blade's length from its top to its point in the prototype, and the blade's full width at its widest, metres.
pub const REFERENCE_LENGTH: f64 = BLADE_ROWS[0] - BLADE_TIP;
/// See [`REFERENCE_LENGTH`].
pub const REFERENCE_WIDTH: f64 = 2.0 * BLADE_WIDTH[1].1;
/// The tie's skin: the chest's share, and the spine's and neck's shares at the ends of the tie, where the tie's heights
/// are the prototype's on the soft-sd torso (the spine's fades in below `0.46` to `0.40`, the neck's above `0.50` to
/// `0.53`).
pub const CHEST_SHARE: f64 = 0.8;
/// See [`CHEST_SHARE`].
pub const END_SHARE: f64 = 0.2;
/// See [`CHEST_SHARE`].
pub const SPINE_FADE: (f64, f64) = (0.46, 0.40);
/// See [`CHEST_SHARE`].
pub const NECK_FADE: (f64, f64) = (0.50, 0.53);

/// The stripe texture is drawn on a canvas of this many pixels a side, as the prototype's is; it repeats.
pub const TEXTURE_PX: u32 = 128;
/// The pixels from one stripe to the next across the canvas.
pub const STRIPE_PERIOD: f64 = 64.0;
/// The stripes, in drawing order: where each starts along the top edge and how wide it is, in pixels; the colour is
/// `main`, `edge`, `edge` and `thin` of [`TieStripes`].
pub const STRIPES: [(f64, f64, Which); 4] =
    [(6.0, 15.0, Which::Main), (2.0, 3.2, Which::Edge), (21.0, 3.2, Which::Edge), (36.0, 4.0, Which::Thin)];
/// The faint weave: a one pixel vertical line every this many pixels, at this opacity.
pub const WEAVE: (f64, f32) = (3.0, 0.10);
/// The copies of a stripe drawn either side so the slanted bands cover the whole canvas: `-2..=3` periods.
const COPIES: std::ops::RangeInclusive<i32> = -2..=3;

/// Which of the tie's stripe colours a stripe takes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Which {
    /// The broad stripe.
    Main,
    /// The thin edging either side of it.
    Edge,
    /// The thin second stripe.
    Thin,
}

/// The four colours of the tie's stripes, each a palette slot or a fixed colour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct TieStripes {
    /// The ground.
    pub base: ColourRef,
    /// The broad stripe.
    pub main: ColourRef,
    /// The edging either side of the broad stripe.
    pub edge: ColourRef,
    /// The thin second stripe.
    pub thin: ColourRef,
}

impl Default for TieStripes {
    fn default() -> Self {
        Self {
            base: ColourRef::rgb(0x2e, 0x3a, 0x5c),
            main: ColourRef::rgb(0xe9, 0xe3, 0xd6),
            edge: ColourRef::rgb(0x1f, 0x28, 0x42),
            thin: ColourRef::rgb(0xb7, 0xa6, 0xc9),
        }
    }
}

impl Validate for TieStripes {
    fn check(&self, _: &mut Check) {}
}

/// The parameters of a tie.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct TieParams {
    /// How long the blade is from the knot to its point, metres.
    pub length: f64,
    /// How wide the blade is at its widest, metres.
    pub width: f64,
    /// The stripes' colours.
    pub stripes: TieStripes,
    /// The tie in shadow.
    pub shade: ColourRef,
    /// The tie's outline.
    pub line: ColourRef,
}

impl Default for TieParams {
    fn default() -> Self {
        Self {
            length: REFERENCE_LENGTH,
            width: REFERENCE_WIDTH,
            stripes: TieStripes::default(),
            shade: ColourRef::rgb(0x9a, 0x9a, 0xac),
            line: ColourRef::rgb(0x1b, 0x20, 0x33),
        }
    }
}

impl Validate for TieParams {
    fn check(&self, c: &mut Check) {
        c.range("length", self.length, 0.04, 0.2);
        c.range("width", self.width, 0.012, 0.06);
        c.nested("stripes", &self.stripes);
    }
}

/// The `tie` part kind.
pub struct Tie;

impl PartKind for Tie {
    type Params = TieParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "tie",
            summary: "Build a striped tie with its knot on a shirt's neck opening.",
            category: PartCategory::Garment,
            requires: &["shirt"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &TieParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let on = hang(ctx)?;
        let mesh = build_mesh(&on, p).map_err(fail)?;
        let colours = [&p.stripes.base, &p.stripes.main, &p.stripes.edge, &p.stripes.thin].map(|c| ctx.colour(c));
        let [base, main, edge, thin] = colours;
        let list = stripes_list(base?, main?, edge?, thin?);
        let texture = paint(ctx, &list)?;
        let texture_name: TextureName = name_of(ctx, "stripes")?;
        let material_name: MaterialName = name_of(ctx, "tie")?;
        let mut material = MaterialSpec::toon(Colour::rgb(255, 255, 255));
        material.base_texture = Some(texture_name.clone());
        material.shading = Shading::Toon(Toon {
            shade: ctx.colour(&p.shade)?,
            shift: -0.1,
            softness: 0.2,
            rim: Rim { colour: Colour::hex(0x00cf_e0d8), strength: 0.12, ..Rim::default() },
            ..Toon::default()
        });
        material.outline = Some(Outline { colour: ctx.colour(&p.line)?, width: 0.0012 });
        Ok(PartOutput::new()
            .with_meshes(vec![mesh_of(ctx, "tie", mesh, material_name.clone())?])
            .with_materials(vec![(material_name, material)])
            .with_textures(vec![(texture_name, texture)]))
    }
}

omuzaki_char_api::register_part_kind!(Tie);

/// The open borders the tie's mesh has: the blade's top, and the knot's bottom and top (the blade's point is closed).
pub const OPEN_BORDERS: usize = 3;

/// The tie's mesh: the blade and then the knot.
///
/// # Errors
/// [`GarmentError`] if the shirt has no ring at a height the tie needs.
#[allow(clippy::cast_possible_truncation)] // skin weights are in 0..=1; the mesh stores f32
fn build_mesh(on: &Hang, p: &TieParams) -> Result<MeshBuilder, GarmentError> {
    let column = &on.shirt;
    let (bottom, length) = (column.bottom(), column.length());
    let actual = |reference: f64| bottom + along(reference) * length;
    // The tie stretches from the top of its blade: lengthwise by `length`, widthwise by `width`.
    let top = BLADE_ROWS[0];
    let stretch = p.length / REFERENCE_LENGTH;
    let tall = |reference: f64| actual(top - (top - reference) * stretch);
    let wide = p.width / REFERENCE_WIDTH;
    let skin_at = |y: f64| -> Vec<(JointRef, f32)> {
        let reference = to_reference(((y - bottom) / length).clamp(0.0, 1.0));
        vec![
            (JointRef::CHEST, CHEST_SHARE as f32),
            (
                JointRef::Humanoid(omuzaki_anim_api::HumanoidBone::Spine),
                (END_SHARE * sstep(SPINE_FADE.0, SPINE_FADE.1, reference)) as f32,
            ),
            (JointRef::NECK, (END_SHARE * sstep(NECK_FADE.0, NECK_FADE.1, reference)) as f32),
        ]
    };
    let width_above = Pchip::new(&BLADE_WIDTH)?;
    let blade_width = |reference: f64| {
        if reference > BLADE_SHOULDER {
            width_above.eval(reference)
        } else {
            BLADE_WIDTH[0].1 * ((reference - BLADE_TIP) / (BLADE_SHOULDER - BLADE_TIP)).max(0.0)
        }
    };
    let mut mesh = MeshBuilder::new();

    // The blade.
    let ys: Vec<f64> = BLADE_ROWS.iter().map(|&r| tall(r)).collect();
    let mut rows = Vec::with_capacity(ys.len());
    for (&reference, &y) in BLADE_ROWS.iter().zip(&ys) {
        let w = blade_width(reference) * wide;
        let front = column.front_z(y, 0.0)?;
        rows.push(
            (0..=SEGMENTS)
                .map(|j| {
                    let ph = ring_angle(j);
                    let x = ph.sin() * w;
                    let thickness = ph.cos() * BLADE_THICKNESS * (w / BLADE_THIN).min(1.0);
                    DVec3::new(x, y, front + BLADE_STANDOFF + BLADE_THICKNESS - BLADE_BOW * x * x + thickness)
                })
                .collect::<Vec<_>>(),
        );
    }
    let insides = ys
        .iter()
        .map(|&y| column.front_z(y, BLADE_INSIDE).map(|z| DVec3::new(0.0, y, z)))
        .collect::<Result<Vec<_>, _>>()?;
    add_ring_grid(&mut mesh, &Grid::from_rows(&rows)?, &insides, &skin_at)?;

    // The knot.
    let knot_ys: Vec<f64> = rows_of(&KNOT_ROWS.map(|(from, to, n)| (tall(from), tall(to), n)));
    let radius = Pchip::new(&KNOT_RADIUS.map(|(y, r)| (tall(y), r * wide)))?;
    let mut rows = Vec::with_capacity(knot_ys.len());
    for &y in &knot_ys {
        let front = column.front_z(y, 0.0)?;
        let dimple = 1.0 - KNOT_DIMPLE.1 * (-((y - tall(KNOT_DIMPLE.0)) / KNOT_DIMPLE.2).powi(2)).exp();
        rows.push(
            (0..=SEGMENTS)
                .map(|j| {
                    let ph = ring_angle(j);
                    let x = ph.sin() * radius.eval(y) * dimple;
                    let depth = KNOT_DEPTH * if ph.cos() > 0.0 { 1.0 } else { KNOT_BACK };
                    DVec3::new(
                        x,
                        y,
                        front + KNOT_STANDOFF + ph.cos() * depth * (0.8 + 0.2 * ph.sin().abs()) - KNOT_SINK,
                    )
                })
                .collect::<Vec<_>>(),
        );
    }
    let insides = knot_ys
        .iter()
        .map(|&y| column.front_z(y, KNOT_INSIDE).map(|z| DVec3::new(0.0, y, z)))
        .collect::<Result<Vec<_>, _>>()?;
    add_ring_grid(&mut mesh, &Grid::from_rows(&rows)?, &insides, &skin_at)?;
    Ok(mesh)
}

/// The angle of column `j`; the last repeats the first exactly so the seam welds.
fn ring_angle(j: usize) -> f64 {
    if j == SEGMENTS { 0.0 } else { TAU * fraction(j, SEGMENTS) }
}

/// Adds `grid` (rings round the tie) to `mesh` with the texture coordinates of its position and the tie's skin.
fn add_ring_grid(
    mesh: &mut MeshBuilder,
    grid: &Grid,
    insides: &[DVec3],
    skin_at: &dyn Fn(f64) -> Vec<(JointRef, f32)>,
) -> Result<(), GarmentError> {
    let inside = |i: usize, _: usize| insides[i];
    let uv = |i: usize, j: usize| {
        let p = grid.at(i, j);
        // The canvas is read top to bottom, so v runs down: the prototype's `y / 0.028` upwards is `1 - ` it here.
        DVec2::new((p.x + 0.5) / TILE.0, 1.0 - p.y / TILE.1)
    };
    let skin = |p: DVec3, _: usize, _: usize| skin_at(p.y);
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        uv: Some(&uv),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    mesh.add_grid(grid, &options)?;
    Ok(())
}

/// The corner `(x, y)` of the canvas (y down) in the drawing plane (y up).
#[allow(clippy::cast_possible_truncation)] // the plane is f32
fn canvas(x: f64, y: f64) -> Vec2 {
    Vec2::new(x as f32, (f64::from(TEXTURE_PX) - y) as f32)
}

/// The drawing of the tie's stripes, in canvas pixels: the ground, the slanted stripes in the prototype's order, and a faint
/// weave of vertical lines.
pub fn stripes_list(base: Colour, main: Colour, edge: Colour, thin: Colour) -> DrawList {
    let size = f64::from(TEXTURE_PX);
    let mut list = DrawList::new();
    let full = [(0.0, 0.0), (size, 0.0), (size, size), (0.0, size)].map(|(x, y)| canvas(x, y));
    let ground = list.path().polyline(&full).close().finish();
    list.fill(ground, Paint::Solid(base));
    for (start, width, which) in STRIPES {
        let colour = match which {
            Which::Main => main,
            Which::Edge => edge,
            Which::Thin => thin,
        };
        for k in COPIES {
            let x = start + STRIPE_PERIOD * f64::from(k);
            // A 45 degree parallelogram from the top edge to the bottom one.
            let corners =
                [(x, 0.0), (x + width, 0.0), (x + width + size, size), (x + size, size)].map(|(x, y)| canvas(x, y));
            let path = list.path().polyline(&corners).close().finish();
            list.fill(path, Paint::Solid(colour));
        }
    }
    // A canvas line one pixel wide centred on integer `x` covers half of the column either side (only the right half
    // of column 0), so it is drawn as the 1 px band it is, for the rasteriser's coverage to weigh.
    let mut x = 0.0;
    while x < size {
        let band = [(x - 0.5).max(0.0), x + 0.5];
        let corners = [(band[0], 0.0), (band[1], 0.0), (band[1], size), (band[0], size)].map(|(x, y)| canvas(x, y));
        let path = list.path().polyline(&corners).close().finish();
        list.push(Cmd::Fill { path, paint: Paint::Solid(Colour::BLACK), opacity: WEAVE.1 });
        x += WEAVE.0;
    }
    list
}

/// Paints the stripes into the tie's texture: repeating, mip-mapped and filtered.
fn paint(ctx: &BuildCtx<'_>, list: &DrawList) -> Result<TextureData, BuildError> {
    let size = f64::from(TEXTURE_PX);
    #[allow(clippy::cast_possible_truncation)] // the plane is f32
    let rect = FaceRect { x0: 0.0, x1: size as f32, y0: 0.0, y1: size as f32 };
    let mut desc = TextureDesc::colour(UVec2::splat(TEXTURE_PX));
    desc.sampler = Sampler { filter: Filter::Linear, wrap: Wrap::Repeat, anisotropy: 8 };
    ctx.paint_texture(UVec2::splat(TEXTURE_PX), rect, list, desc)
}
