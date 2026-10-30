//! The material layer stack (#2609): layers on every part's surface, each a parameterised generator with masks,
//! evaluated after the part is built (and again whenever the palette, the age or a layer changes), so nothing is ever
//! repainted by hand.
//!
//! A part's layers run bottom first over its surface colour. Each has an `opacity` and a list of [`Mask`]s whose
//! product says where it shows. They write:
//! - the **vertex colours** of the part's meshes (the material's base colour times them is the surface colour, so a
//!   layer that sets a colour divides by the base, and the base itself is never touched: a recolour still recolours);
//! - for **hair**, whose colours are the hair program's parameters (`root`, `mid`, `tip`, `line`), those parameters,
//!   by the mean of the layer's masks over the material;
//! - for **skin**, the skin program's `skin-info` attribute: wetness, the cavity term and flush, and the surface's
//!   curvature for the scattering, so a photoreal look has what it needs.
//!
//! | Kind | What it does |
//! |---|---|
//! | `tint` | a colour, blended |
//! | `detail` | mottled variation toward a colour (fine skin and fabric irregularity), seeded, at a scale per metre |
//! | `wear` | a lighter, worn colour on the edges that stand out, broken up by noise |
//! | `wet` | darker, wet-looking surface; on skin, the wet term the skin program reads |
//! | `makeup` | blush, eyeshadow, lips, contour or highlight, placed by the face's landmarks on the head |
//! | `paint` | dabs of colour at points on the part (the base the painting tool, #2575, writes to) |
//! | `polish` | nail polish on the fingertips' backs |
//! | `decal` | a shape projected onto the surface (a tattoo, a scar, a birthmark), optionally across every skin part |
//! | `grey` | greying, toward silver at the same lightness |
//! | `cavity` | darkens the folds (and writes the cavity and curvature terms) |
//! | `spots` | scattered spots (freckles, age spots) |
//!
//! Points (a dab, a decal) are [`PartPoint`]s, so they stay where they were put after any shape change. Exports that
//! carry no vertex colours bake the stack into a texture (#2575).

#![allow(clippy::many_single_char_names)] // vector geometry: points, normals, weights and fractions

use std::collections::{BTreeMap, HashMap};

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_core::id::Slug;
use omuzaki_core::mesh::ExtraAttribute;
use omuzaki_face_api::{ColourRef, Palette};
use omuzaki_render_api::{ParamValue, Shading};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::age::AgeLayers;
use super::deform::{BodyFrameFact, HeadFrameFact};
use super::frame::{BodyFrame, HeadFrame, Landmark, Membership, PartFrame, RegionGroup};
use super::sculpt::PartPoint;
use super::{bump, smoothstep};
use crate::error::BuildError;
use crate::geometry::PartMesh;
use crate::hair::HAIR_PROGRAM;
use crate::ids::{JointRef, LayerId, PartId};
use crate::kind::{FitFact, HeadFact, PartCategory, PartOutput, Published};

/// The skin program's attribute (`omuzaki-render`'s `skin` program): wet, cavity, flush, curvature in 1/m.
pub const SKIN_INFO: &str = "skin-info";

/// The hair program's colour parameters a layer recolours.
const HAIR_COLOURS: [&str; 4] = ["root", "mid", "tip", "line"];

/// How a layer's colour meets what is under it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Blend {
    /// Replaces it.
    #[default]
    Normal,
    /// Multiplies it (darkens).
    Multiply,
    /// Screens it (lightens).
    Screen,
    /// Overlays it (contrast).
    Overlay,
}

impl Blend {
    fn apply(self, a: Vec3, c: Vec3, w: f32) -> Vec3 {
        let target = match self {
            Self::Normal => c,
            Self::Multiply => a * c,
            Self::Screen => Vec3::ONE - (Vec3::ONE - a) * (Vec3::ONE - c),
            Self::Overlay => {
                let ch = |a: f32, c: f32| if a < 0.5 { 2.0 * a * c } else { 1.0 - 2.0 * (1.0 - a) * (1.0 - c) };
                Vec3::new(ch(a.x, c.x), ch(a.y, c.y), ch(a.z, c.z))
            }
        };
        a.lerp(target, w.clamp(0.0, 1.0))
    }
}

/// Where makeup goes on the head.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MakeupZone {
    /// The cheeks.
    Blush,
    /// The lids and above the eyes.
    Eyeshadow,
    /// The mouth.
    Lips,
    /// Under the cheekbones.
    Contour,
    /// The bridge of the nose and the tops of the cheekbones.
    Highlight,
}

impl MakeupZone {
    /// The landmarks and nudges (in the head's proportions) a zone centres on, and its reach in degrees.
    fn aims(self) -> (&'static [(Landmark, [f32; 3])], f32) {
        match self {
            Self::Blush => (&[(Landmark::CheekLeft, [0.0; 3]), (Landmark::CheekRight, [0.0; 3])], 20.0),
            Self::Eyeshadow => (&[(Landmark::EyeLeft, [0.0, 0.08, 0.0]), (Landmark::EyeRight, [0.0, 0.08, 0.0])], 11.0),
            Self::Lips => (&[(Landmark::Mouth, [0.0; 3])], 9.0),
            Self::Contour => {
                (&[(Landmark::CheekboneLeft, [0.0, -0.12, 0.0]), (Landmark::CheekboneRight, [0.0, -0.12, 0.0])], 16.0)
            }
            Self::Highlight => (
                &[
                    (Landmark::NoseBridge, [0.0; 3]),
                    (Landmark::CheekboneLeft, [0.0, 0.03, 0.0]),
                    (Landmark::CheekboneRight, [0.0, 0.03, 0.0]),
                ],
                9.0,
            ),
        }
    }
}

/// A decal's shape, in a unit square centred on its point.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecalShape {
    /// A disc (a birthmark, a beauty mark).
    Disc,
    /// A ring.
    Ring,
    /// A heart.
    Heart,
    /// A five-pointed star.
    Star,
    /// A diamond.
    Diamond,
    /// A cross.
    Cross,
    /// A bar (a stripe of body paint).
    Bar,
    /// A thin, slightly wavy scar line.
    Scar,
}

impl DecalShape {
    /// The signed distance (in the decal's unit square, `±0.5` the edges) from the shape's edge: negative inside.
    fn sdf(self, u: f32, v: f32) -> f32 {
        let r = (u * u + v * v).sqrt();
        match self {
            Self::Disc => r - 0.45,
            Self::Ring => (r - 0.36).abs() - 0.08,
            Self::Diamond => (u.abs() + v.abs()) - 0.45,
            Self::Cross => (u.abs().max(v.abs()) - 0.45).max(u.abs().min(v.abs()) - 0.12),
            Self::Bar => (u.abs() - 0.48).max(v.abs() - 0.14),
            Self::Scar => {
                let wave = 0.03 * (u * 18.0).sin();
                (u.abs() - 0.46).max((v - wave).abs() - 0.025 * (1.0 - (2.0 * u).powi(2)).max(0.2))
            }
            Self::Heart => {
                // Two discs over a diamond's lower half.
                let lobes = ((u.abs() - 0.18).powi(2) + (v - 0.12).powi(2)).sqrt() - 0.21;
                let point = (u.abs() + (-v + 0.12) * 0.95) - 0.40;
                lobes.min(point.max(0.12 - v - 0.42).max(-(0.12 - v)).min(point.max(v - 0.12)))
            }
            Self::Star => {
                let a = v.atan2(u) + std::f32::consts::FRAC_PI_2;
                let k = 2.0 * std::f32::consts::PI / 5.0;
                let m = (a.rem_euclid(k) - 0.5 * k).abs() / (0.5 * k);
                r - (0.20 + 0.27 * m.powf(1.6))
            }
        }
    }
}

/// One dab of paint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dab {
    /// Where.
    pub at: PartPoint,
    /// Its radius, as a fraction of the local scale there.
    pub radius: f32,
    /// Its colour.
    pub colour: ColourRef,
    /// `0` soft to `1` hard-edged.
    #[serde(default)]
    pub hardness: f32,
}

/// What a layer does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LayerKind {
    /// A colour, blended.
    Tint {
        /// The colour.
        colour: ColourRef,
        /// How it blends.
        #[serde(default)]
        blend: Blend,
    },
    /// Mottled variation toward a colour.
    Detail {
        /// The colour the variation leans toward.
        colour: ColourRef,
        /// Features per metre.
        scale: f32,
        /// How strongly it varies, `0..=1`.
        contrast: f32,
        /// The noise's seed.
        #[serde(default)]
        seed: u32,
    },
    /// A worn colour on the edges that stand out.
    Wear {
        /// The worn colour.
        colour: ColourRef,
        /// How worn, `0..=1`.
        amount: f32,
    },
    /// A wet surface.
    Wet {
        /// How wet, `0..=1`.
        amount: f32,
    },
    /// Makeup on the head.
    Makeup {
        /// Where.
        zone: MakeupZone,
        /// Its colour.
        colour: ColourRef,
        /// How strong, `0..=1`.
        amount: f32,
    },
    /// Dabs of paint.
    Paint {
        /// The dabs, in order.
        dabs: Vec<Dab>,
    },
    /// Nail polish.
    Polish {
        /// Its colour.
        colour: ColourRef,
    },
    /// A shape projected onto the surface.
    Decal {
        /// The shape.
        shape: DecalShape,
        /// Where its centre is.
        at: PartPoint,
        /// Its size, metres across.
        size: f32,
        /// Its rotation about the surface's normal, degrees.
        #[serde(default)]
        rotation: f32,
        /// Its colour.
        colour: ColourRef,
        /// Whether it also lands on the character's other skin parts (a tattoo from the neck onto the head).
        #[serde(default)]
        across_parts: bool,
    },
    /// Greying.
    Grey {
        /// How grey, `0..=1`.
        amount: f32,
    },
    /// Darkened folds.
    Cavity {
        /// How dark, `0..=1`.
        amount: f32,
    },
    /// Scattered spots.
    Spots {
        /// Their colour.
        colour: ColourRef,
        /// Spots per metre.
        density: f32,
        /// How strong, `0..=1`.
        amount: f32,
        /// The seed.
        #[serde(default)]
        seed: u32,
    },
}

/// Where a layer shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MaskKind {
    /// A stretch of a body region.
    Region {
        /// The region.
        group: RegionGroup,
        /// From this fraction along it.
        #[serde(default)]
        from: f32,
        /// To this one.
        #[serde(default = "one")]
        to: f32,
    },
    /// Round a landmark of the face.
    Landmark {
        /// The landmark.
        landmark: Landmark,
        /// How far round the head, degrees.
        reach: f32,
    },
    /// A band of the part's height (`0` its bottom, `1` its top).
    Height {
        /// From.
        from: f32,
        /// To.
        to: f32,
    },
    /// A ramp over the part's height (`0` its bottom, `1` its top): 0 at `from`, 1 at `to`, smooth between, so a tint
    /// fades along the part (a stocking from teal at the thigh to indigo at the foot: `from = 1, to = 0`).
    #[non_exhaustive]
    Gradient {
        /// Where the ramp is 0.
        from: f32,
        /// Where it is 1.
        to: f32,
    },
    /// Surfaces facing a direction: 0 at `from` (the cosine to the direction), 1 at `to`.
    Facing {
        /// The direction, bind space.
        dir: [f32; 3],
        /// The cosine where it starts.
        from: f32,
        /// The cosine where it is full.
        to: f32,
    },
    /// Noise.
    Noise {
        /// Features per metre.
        scale: f32,
        /// Where it turns on, `0..=1`.
        threshold: f32,
        /// How soft its edge is.
        #[serde(default = "soft")]
        softness: f32,
        /// The seed.
        #[serde(default)]
        seed: u32,
    },
    /// Edges that stand out (positive curvature): 0 at `from` (1/m), 1 at `to`.
    Convex {
        /// Where it starts.
        from: f32,
        /// Where it is full.
        to: f32,
    },
    /// Folds (negative curvature): 0 at `from` (1/m of concavity), 1 at `to`.
    Concave {
        /// Where it starts.
        from: f32,
        /// Where it is full.
        to: f32,
    },
}

impl MaskKind {
    /// A [`MaskKind::Gradient`]: 0 at `from` up the part's height, 1 at `to`.
    #[must_use]
    pub const fn gradient(from: f32, to: f32) -> Self {
        Self::Gradient { from, to }
    }
}

fn one() -> f32 {
    1.0
}

fn soft() -> f32 {
    0.1
}

fn enabled() -> bool {
    true
}

/// A mask, inverted or not.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mask {
    /// The mask.
    pub mask: MaskKind,
    /// Shows the layer where the mask is 0 instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invert: bool,
}

/// One layer of a part's stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaterialLayer {
    /// The layer's id, unique within the part.
    pub id: LayerId,
    /// What it does.
    pub kind: LayerKind,
    /// How much of it shows, `0..=1`.
    #[serde(default = "one")]
    pub opacity: f32,
    /// Where it shows: the product of the masks (everywhere, with none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<Mask>,
    /// Off: kept but not drawn.
    #[serde(default = "enabled")]
    pub enabled: bool,
}

impl MaterialLayer {
    /// A layer of `kind`, fully opaque, everywhere.
    pub fn new(id: LayerId, kind: LayerKind) -> Self {
        Self { id, kind, opacity: 1.0, masks: Vec::new(), enabled: true }
    }

    /// Checks the numbers.
    ///
    /// # Errors
    /// The field that is wrong.
    pub fn check(&self) -> Result<(), String> {
        let unit = |name: &str, v: f32| {
            if (0.0..=1.0).contains(&v) { Ok(()) } else { Err(format!("{name}: {v} is outside 0 to 1")) }
        };
        unit("opacity", self.opacity)?;
        match &self.kind {
            LayerKind::Tint { .. } | LayerKind::Polish { .. } => {}
            LayerKind::Detail { scale, contrast, .. } => {
                unit("kind/contrast", *contrast)?;
                positive("kind/scale", *scale)?;
            }
            LayerKind::Wear { amount, .. }
            | LayerKind::Wet { amount }
            | LayerKind::Makeup { amount, .. }
            | LayerKind::Grey { amount }
            | LayerKind::Cavity { amount } => unit("kind/amount", *amount)?,
            LayerKind::Paint { dabs } => {
                for (i, dab) in dabs.iter().enumerate() {
                    dab.at.check().map_err(|e| format!("kind/dabs/{i}/at/{e}"))?;
                    positive(&format!("kind/dabs/{i}/radius"), dab.radius)?;
                    unit(&format!("kind/dabs/{i}/hardness"), dab.hardness)?;
                }
            }
            LayerKind::Decal { at, size, rotation, .. } => {
                at.check().map_err(|e| format!("kind/at/{e}"))?;
                positive("kind/size", *size)?;
                if !rotation.is_finite() {
                    return Err("kind/rotation: not a number".to_owned());
                }
            }
            LayerKind::Spots { density, amount, .. } => {
                positive("kind/density", *density)?;
                unit("kind/amount", *amount)?;
            }
        }
        for (i, mask) in self.masks.iter().enumerate() {
            let ok = match &mask.mask {
                MaskKind::Region { from, to, .. } | MaskKind::Height { from, to } => from <= to,
                MaskKind::Gradient { from, to } => {
                    (from - to).abs() > f32::EPSILON && (0.0..=1.0).contains(from) && (0.0..=1.0).contains(to)
                }
                MaskKind::Landmark { reach, .. } => *reach > 0.0 && *reach <= 180.0,
                MaskKind::Facing { dir, .. } => Vec3::from(*dir).length() > 1e-6,
                MaskKind::Noise { scale, softness, .. } => *scale > 0.0 && *softness >= 0.0,
                MaskKind::Convex { from, to } | MaskKind::Concave { from, to } => from < to,
            };
            if !ok {
                return Err(format!("masks/{i}: its numbers are out of order or out of range"));
            }
        }
        Ok(())
    }
}

fn positive(name: &str, v: f32) -> Result<(), String> {
    if v > 0.0 && v.is_finite() { Ok(()) } else { Err(format!("{name}: {v} must be above 0")) }
}

// ------------------------------------------------------------------------------------------------ noise

fn hash(p: [i32; 3], seed: u32) -> f32 {
    #[allow(clippy::cast_sign_loss)] // bit mixing
    let mut h = seed.wrapping_mul(0x9e37_79b9)
        ^ (p[0] as u32).wrapping_mul(0x85eb_ca6b)
        ^ (p[1] as u32).wrapping_mul(0xc2b2_ae35)
        ^ (p[2] as u32).wrapping_mul(0x27d4_eb2f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    #[allow(clippy::cast_precision_loss)]
    let v = (h >> 8) as f32 / (1u32 << 24) as f32;
    v
}

/// Value noise in `0..=1` at `p` (cells of size 1).
#[allow(clippy::cast_possible_truncation)] // floor of a bounded coordinate
fn value_noise(p: Vec3, seed: u32) -> f32 {
    let i = p.floor();
    let f = p - i;
    let u = f * f * (Vec3::splat(3.0) - 2.0 * f);
    let c = [i.x as i32, i.y as i32, i.z as i32];
    let h = |x: i32, y: i32, z: i32| hash([c[0] + x, c[1] + y, c[2] + z], seed);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(h(0, 0, 0), h(1, 0, 0), u.x);
    let x10 = lerp(h(0, 1, 0), h(1, 1, 0), u.x);
    let x01 = lerp(h(0, 0, 1), h(1, 0, 1), u.x);
    let x11 = lerp(h(0, 1, 1), h(1, 1, 1), u.x);
    lerp(lerp(x00, x10, u.y), lerp(x01, x11, u.y), u.z)
}

/// Three octaves of value noise, `0..=1`.
fn fbm(p: Vec3, seed: u32) -> f32 {
    (0.57 * value_noise(p, seed) + 0.29 * value_noise(p * 2.03, seed + 1) + 0.14 * value_noise(p * 4.11, seed + 2))
        .clamp(0.0, 1.0)
}

/// The distance from `p` to the nearest of one random point per cell (cells of size 1), `0..`.
#[allow(clippy::cast_possible_truncation)]
fn cells(p: Vec3, seed: u32) -> f32 {
    let i = p.floor();
    let base = [i.x as i32, i.y as i32, i.z as i32];
    let mut best = f32::INFINITY;
    for z in -1..=1 {
        for y in -1..=1 {
            for x in -1..=1 {
                let c = [base[0] + x, base[1] + y, base[2] + z];
                #[allow(clippy::cast_precision_loss)]
                let cell = Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32);
                let jitter = Vec3::new(hash(c, seed), hash(c, seed + 7), hash(c, seed + 13));
                best = best.min((cell + jitter).distance(p));
            }
        }
    }
    best
}

// ------------------------------------------------------------------------------------------------ evaluation

/// What a layer reads about one vertex.
struct Vertex {
    p: Vec3,
    n: Vec3,
    m: Membership,
    /// Its height in the part's box, `0..=1`.
    height: f32,
    /// Mean curvature, 1/m (positive: convex).
    curvature: f32,
    /// The weight of the fingertips' bones.
    distal: f32,
}

/// The frames layers read, from the part itself or from what was published before it.
struct Frames {
    body: Option<std::sync::Arc<BodyFrame>>,
    head: Option<std::sync::Arc<HeadFrame>>,
    part: Option<PartFrame>,
}

impl Frames {
    fn point(&self, at: &PartPoint) -> Option<(Vec3, Vec3, f32)> {
        match *at {
            PartPoint::Body { region, t, angle, lift } => {
                let row = self.body.as_ref()?.path(region)?.at(t);
                let d = row.direction(angle);
                Some((row.centre + d * row.radius_at(angle) * lift, d, row.scale()))
            }
            PartPoint::Head { dir, lift } => {
                let head = self.head.as_ref()?;
                let d = head.to_dir(Vec3::from(dir));
                Some((head.centre + d * head.radius(d) * lift, d, head.scale()))
            }
            PartPoint::Box { at } => {
                let part = self.part.as_ref()?;
                let p = part.point(Vec3::from(at));
                Some((p, (p - 0.5 * (part.min + part.max)).normalize_or(Vec3::Z), part.scale()))
            }
        }
    }

    /// The head frame's angle from `aim` to the vertex, for a head vertex.
    fn head_bump(&self, v: &Vertex, aim: Vec3, reach_deg: f32) -> f32 {
        let Some(head) = &self.head else { return 0.0 };
        let w = v.m.of(RegionGroup::Head);
        if w <= 0.0 {
            return 0.0;
        }
        let d = (v.p - head.centre).normalize_or(Vec3::Z);
        w * bump(d.dot(aim).clamp(-1.0, 1.0).acos() / reach_deg.to_radians())
    }
}

/// A layer with its colours resolved.
struct Resolved<'a> {
    layer: &'a MaterialLayer,
    colour: Vec3,
    /// A paint layer's dab colours, in order.
    dabs: Vec<Vec3>,
}

fn mask_value(mask: &Mask, v: &Vertex, frames: &Frames) -> f32 {
    let value = match &mask.mask {
        MaskKind::Region { group, from, to } => {
            let w = v.m.of(*group);
            match frames.body.as_ref().and_then(|b| b.path(*group)) {
                Some(path) if w > 0.0 => {
                    let t = path.locate(v.p).row.t;
                    w * smoothstep(from - 0.03, from + 0.03, t) * (1.0 - smoothstep(to - 0.03, to + 0.03, t))
                }
                _ => 0.0,
            }
        }
        MaskKind::Landmark { landmark, reach } => match &frames.head {
            Some(head) => frames.head_bump(v, head.landmark(*landmark), *reach),
            None => 0.0,
        },
        MaskKind::Height { from, to } => {
            smoothstep(from - 0.02, from + 0.02, v.height) * (1.0 - smoothstep(to - 0.02, to + 0.02, v.height))
        }
        MaskKind::Gradient { from, to } => {
            if to > from {
                smoothstep(*from, *to, v.height)
            } else {
                1.0 - smoothstep(*to, *from, v.height)
            }
        }
        MaskKind::Facing { dir, from, to } => {
            let c = v.n.dot(Vec3::from(*dir).normalize_or(Vec3::Y));
            if to > from { smoothstep(*from, *to, c) } else { 1.0 - smoothstep(*to, *from, c) }
        }
        MaskKind::Noise { scale, threshold, softness, seed } => {
            let n = fbm(v.p * *scale, *seed);
            smoothstep(threshold - softness, threshold + softness, n)
        }
        MaskKind::Convex { from, to } => smoothstep(*from, *to, v.curvature),
        MaskKind::Concave { from, to } => smoothstep(*from, *to, -v.curvature),
    };
    if mask.invert { 1.0 - value } else { value }
}

/// What a layer does to one vertex's colour (linear) and skin terms.
fn apply_layer(r: &Resolved<'_>, v: &Vertex, frames: &Frames, colour: &mut Vec3, skin: &mut [f32; 4]) {
    let layer = r.layer;
    let mut w = layer.opacity;
    for mask in &layer.masks {
        if w <= 0.0 {
            return;
        }
        w *= mask_value(mask, v, frames);
    }
    if w <= 0.0 {
        return;
    }
    match &layer.kind {
        LayerKind::Tint { blend, .. } => *colour = blend.apply(*colour, r.colour, w),
        LayerKind::Detail { scale, contrast, seed, .. } => {
            let n = fbm(v.p * *scale, *seed);
            *colour = Blend::Normal.apply(*colour, r.colour, w * contrast * n);
        }
        LayerKind::Wear { amount, .. } => {
            let edge = smoothstep(2.0, 40.0, v.curvature) * (0.4 + 0.6 * fbm(v.p * 60.0, 11));
            *colour = Blend::Normal.apply(*colour, r.colour, w * amount * edge);
        }
        LayerKind::Wet { amount } => {
            let k = w * amount;
            let luma = colour.dot(Vec3::new(0.2126, 0.7152, 0.0722));
            let saturated = Vec3::splat(luma) + (*colour - Vec3::splat(luma)) * 1.25;
            *colour = colour.lerp(saturated * 0.72, k);
            skin[0] = skin[0].max(k);
        }
        LayerKind::Makeup { zone, amount, .. } => {
            let Some(head) = &frames.head else { return };
            let (aims, reach) = zone.aims();
            let mut m = 0.0f32;
            for (landmark, nudge) in aims {
                let aim = head.to_dir(head.from_dir(head.landmark(*landmark)) + Vec3::from(*nudge));
                m = m.max(frames.head_bump(v, aim, reach));
            }
            let blend = if *zone == MakeupZone::Highlight { Blend::Screen } else { Blend::Multiply };
            let k = w * amount * m;
            *colour = blend.apply(*colour, r.colour, k);
            if *zone == MakeupZone::Blush {
                skin[2] = skin[2].max(k);
            }
        }
        LayerKind::Paint { dabs } => {
            for (dab, c) in dabs.iter().zip(&r.dabs) {
                let Some((at, _, scale)) = frames.point(&dab.at) else { continue };
                let x = v.p.distance(at) / (dab.radius * scale).max(1e-6);
                let soft = bump(x);
                let hard = 1.0 - smoothstep(0.92, 1.0, x);
                *colour = colour.lerp(*c, (w * (soft + (hard - soft) * dab.hardness)).clamp(0.0, 1.0));
            }
        }
        LayerKind::Polish { .. } => {
            let back = smoothstep(-0.1, 0.5, v.n.y);
            *colour = colour.lerp(r.colour, (w * smoothstep(0.35, 0.8, v.distal) * back).clamp(0.0, 1.0));
        }
        LayerKind::Decal { shape, at, size, rotation, .. } => {
            let Some((anchor, normal, _)) = frames.point(at) else { return };
            let k = decal_coverage(*shape, anchor, normal, *size, *rotation, v);
            *colour = colour.lerp(r.colour, (w * k).clamp(0.0, 1.0));
        }
        LayerKind::Grey { amount } => {
            let luma = colour.dot(Vec3::new(0.2126, 0.7152, 0.0722));
            let silver = Vec3::new(0.80, 0.80, 0.83) * (0.55 + 0.45 * luma.sqrt());
            *colour = colour.lerp(silver, w * amount);
        }
        LayerKind::Cavity { amount } => {
            let fold = smoothstep(4.0, 60.0, -v.curvature);
            *colour *= 1.0 - 0.55 * w * amount * fold;
            skin[1] = skin[1].min(1.0 - w * amount * fold);
        }
        LayerKind::Spots { density, amount, seed, .. } => {
            let c = cells(v.p * *density, *seed);
            let spot = 1.0 - smoothstep(0.10, 0.22, c);
            #[allow(clippy::cast_possible_truncation)] // a hash key: the cell distance, quantised
            let key = (c * 1000.0) as i32;
            let keep = smoothstep(0.35, 0.6, hash([key, 0, 0], *seed));
            *colour = Blend::Multiply.apply(*colour, r.colour, w * amount * spot * keep);
        }
    }
}

/// How much of a decal covers vertex `v`: its shape in the plane through `anchor` facing `normal`, projected along
/// the normal no deeper than its size, on surfaces facing it.
fn decal_coverage(shape: DecalShape, anchor: Vec3, normal: Vec3, size: f32, rotation: f32, v: &Vertex) -> f32 {
    let n = normal.normalize_or(Vec3::Z);
    let up0 = Vec3::Y - n * n.y;
    let up = if up0.length() > 0.2 { up0.normalize() } else { (Vec3::Z - n * n.z).normalize_or(Vec3::X) };
    let right = up.cross(n);
    let (s, c) = rotation.to_radians().sin_cos();
    let (t, b) = (right * c + up * s, up * c - right * s);
    let d = v.p - anchor;
    if d.dot(n).abs() > size {
        return 0.0;
    }
    let facing = smoothstep(0.05, 0.35, v.n.dot(n));
    let (u, w) = (d.dot(t) / size, d.dot(b) / size);
    if u.abs() > 0.6 || w.abs() > 0.6 {
        return 0.0;
    }
    let sdf = shape.sdf(u, w);
    facing * (1.0 - smoothstep(-0.015, 0.015, sdf))
}

// ------------------------------------------------------------------------------------------------ the stack on a part

/// The implicit layers the age puts on a part of `category`.
fn age_layers(age: &AgeLayers, category: PartCategory) -> Vec<MaterialLayer> {
    let id = |s: &str| LayerId::new(s).ok();
    let mut out = Vec::new();
    let mut push = |name: &str, kind: LayerKind, masks: Vec<Mask>, opacity: f32| {
        if let Some(id) = id(name) {
            out.push(MaterialLayer { opacity, masks, ..MaterialLayer::new(id, kind) });
        }
    };
    match category {
        PartCategory::Hair if age.grey > 0.0 => push("age-grey", LayerKind::Grey { amount: age.grey }, Vec::new(), 1.0),
        PartCategory::Body | PartCategory::Head => {
            if age.wrinkles > 0.0 {
                push("age-folds", LayerKind::Cavity { amount: age.wrinkles }, Vec::new(), 1.0);
                for (name, landmark, reach) in [
                    ("age-crow-left", Landmark::EyeLeft, 9.0),
                    ("age-crow-right", Landmark::EyeRight, 9.0),
                    ("age-smile-fold", Landmark::Mouth, 16.0),
                    ("age-forehead", Landmark::Forehead, 18.0),
                ] {
                    let colour = ColourRef::Fixed(Colour::hex(0x00b0_8c80));
                    let masks = vec![
                        Mask { mask: MaskKind::Landmark { landmark, reach }, invert: false },
                        Mask {
                            mask: MaskKind::Noise { scale: 160.0, threshold: 0.55, softness: 0.06, seed: 31 },
                            invert: false,
                        },
                    ];
                    push(name, LayerKind::Tint { colour, blend: Blend::Multiply }, masks, 0.45 * age.wrinkles);
                }
            }
            if age.spots > 0.0 {
                let colour = ColourRef::Fixed(Colour::hex(0x00b8_8a64));
                push(
                    "age-spots",
                    LayerKind::Spots { colour, density: 90.0, amount: age.spots, seed: 5 },
                    Vec::new(),
                    1.0,
                );
            }
            if age.flush > 0.0 && category == PartCategory::Head {
                let colour = ColourRef::Fixed(Colour::hex(0x00ff_b0a8));
                push(
                    "age-flush",
                    LayerKind::Makeup { zone: MakeupZone::Blush, colour, amount: age.flush },
                    Vec::new(),
                    1.0,
                );
            }
        }
        _ => {}
    }
    out
}

/// The decals with `across_parts` on every part but `part`, for a skin part.
fn shared_decals<'a>(design: &'a super::DesignDef, part: &PartId) -> Vec<&'a MaterialLayer> {
    design
        .layers
        .iter()
        .filter(|(owner, _)| *owner != part)
        .flat_map(|(_, layers)| layers.iter())
        .filter(|l| matches!(l.kind, LayerKind::Decal { across_parts: true, .. }))
        .collect()
}

/// Runs the layer stack of `part` (and the age's implicit layers, and the decals other skin parts share) over
/// `output`. Does nothing when there are none.
///
/// # Errors
/// [`BuildError::Colour`] for a colour that names a slot the palette doesn't have.
#[allow(clippy::too_many_arguments)] // the build step's own inputs: the design, the age, the palette, the face and the part
pub fn apply(
    design: &super::DesignDef,
    age: Option<AgeLayers>,
    palette: &Palette,
    layout: &omuzaki_face_api::FaceLayout,
    part: &PartId,
    category: PartCategory,
    output: &mut PartOutput,
    published: &Published,
) -> Result<(), BuildError> {
    let skin_part = matches!(category, PartCategory::Body | PartCategory::Head);
    let implicit = age.map(|a| age_layers(&a, category)).unwrap_or_default();
    let shared = if skin_part { shared_decals(design, part) } else { Vec::new() };
    let layers: Vec<&MaterialLayer> =
        implicit.iter().chain(design.layers_of(part)).chain(shared).filter(|l| l.enabled && l.opacity > 0.0).collect();
    if layers.is_empty() {
        return Ok(());
    }
    let colour = |c: &ColourRef| {
        palette.resolve(c).map(Colour::to_linear).map_err(|error| BuildError::Colour { part: part.clone(), error })
    };
    let resolved = layers
        .iter()
        .map(|layer| {
            let main = match &layer.kind {
                LayerKind::Tint { colour: c, .. }
                | LayerKind::Detail { colour: c, .. }
                | LayerKind::Wear { colour: c, .. }
                | LayerKind::Makeup { colour: c, .. }
                | LayerKind::Polish { colour: c }
                | LayerKind::Decal { colour: c, .. }
                | LayerKind::Spots { colour: c, .. } => colour(c)?,
                LayerKind::Paint { .. } | LayerKind::Wet { .. } | LayerKind::Grey { .. } | LayerKind::Cavity { .. } => {
                    Vec3::ONE
                }
            };
            let dabs = match &layer.kind {
                LayerKind::Paint { dabs } => dabs.iter().map(|d| colour(&d.colour)).collect::<Result<_, _>>()?,
                _ => Vec::new(),
            };
            Ok(Resolved { layer, colour: main, dabs })
        })
        .collect::<Result<Vec<_>, BuildError>>()?;
    let frames = frames(output, published, category, layout);
    let skin_classes = skin_materials(output, category);
    let program_materials = hair_materials(output);
    let mut program_weights: BTreeMap<usize, (f32, usize)> = BTreeMap::new();
    let mut needs_colours = std::collections::BTreeSet::new();
    let uses_curvature = resolved.iter().any(|r| {
        matches!(r.layer.kind, LayerKind::Wear { .. } | LayerKind::Cavity { .. })
            || r.layer.masks.iter().any(|m| matches!(m.mask, MaskKind::Convex { .. } | MaskKind::Concave { .. }))
    });
    let writes_skin = resolved.iter().any(|r| {
        matches!(
            r.layer.kind,
            LayerKind::Wet { .. } | LayerKind::Cavity { .. } | LayerKind::Makeup { zone: MakeupZone::Blush, .. }
        )
    });
    for mesh in &mut output.meshes {
        let Some(mi) = output.materials.iter().position(|(name, _)| *name == mesh.material) else { continue };
        let base = output.materials[mi].1.base.to_linear();
        let program = program_materials.contains(&mi);
        let skin = skin_classes.contains(&mi);
        let vertices = vertices(mesh, frames.part.as_ref(), category, uses_curvature || skin);
        let mut colours = if mesh.geometry.colours.is_empty() {
            vec![[1.0f32; 4]; vertices.len()]
        } else {
            mesh.geometry.colours.clone()
        };
        let mut skin_info = skin_info_of(mesh, &vertices);
        let mut sum = 0.0f32;
        for (i, v) in vertices.iter().enumerate() {
            let vc = Vec3::new(colours[i][0], colours[i][1], colours[i][2]);
            let mut albedo = if program { Vec3::ONE } else { base * vc };
            let mut s = skin_info[i];
            for r in &resolved {
                apply_layer(r, v, &frames, &mut albedo, &mut s);
            }
            if program {
                sum += (albedo - Vec3::ONE).length();
                continue;
            }
            let out = Vec3::new(
                divide(albedo.x, base.x, vc.x),
                divide(albedo.y, base.y, vc.y),
                divide(albedo.z, base.z, vc.z),
            );
            colours[i] = [out.x, out.y, out.z, colours[i][3]];
            skin_info[i] = s;
        }
        if program {
            let entry = program_weights.entry(mi).or_insert((0.0, 0));
            entry.0 += sum;
            entry.1 += vertices.len();
            continue;
        }
        mesh.geometry.colours = colours;
        needs_colours.insert(mi);
        if skin && (writes_skin || uses_curvature) {
            write_skin_info(mesh, &skin_info);
        }
    }
    for mi in needs_colours {
        output.materials[mi].1.vertex_colours = true;
    }
    for mi in program_weights.keys() {
        recolour_program(&mut output.materials[*mi].1.shading, &resolved, &frames);
    }
    Ok(())
}

/// The vertex colour that makes `base × colour` equal `target`, or the old colour where the base is black.
fn divide(target: f32, base: f32, old: f32) -> f32 {
    if base > 1e-4 { (target / base).max(0.0) } else { old }
}

fn frames(
    output: &PartOutput,
    published: &Published,
    category: PartCategory,
    layout: &omuzaki_face_api::FaceLayout,
) -> Frames {
    let body = published.get::<BodyFrameFact>().clone().or_else(|| {
        let own = output.publishes.values::<FitFact>().find(|f| f.envelope()).cloned();
        own.or_else(|| published.outer_fit().cloned()).map(|f| std::sync::Arc::new(BodyFrame::of(f.as_ref())))
    });
    let head = match output.publishes.values::<super::deform::HeadFrameFact>().next() {
        Some(own) => Some(own.clone()),
        None => published.get::<HeadFrameFact>().clone().or_else(|| {
            let surface =
                output.publishes.values::<HeadFact>().next().cloned().or_else(|| published.get::<HeadFact>().clone());
            surface.map(|s| std::sync::Arc::new(HeadFrame::of(s, layout)))
        }),
    };
    let body = match output.publishes.values::<BodyFrameFact>().next() {
        Some(own) if category == PartCategory::Body => Some(own.clone()),
        _ => body,
    };
    Frames { body, head, part: PartFrame::of(&output.meshes) }
}

/// The materials of `output` drawn as skin (a toon material with the skin treatment, or the skin program), for the
/// `skin-info` attribute.
fn skin_materials(output: &PartOutput, category: PartCategory) -> std::collections::BTreeSet<usize> {
    output
        .materials
        .iter()
        .enumerate()
        .filter(|(_, (name, spec))| {
            let declared = output.material_classes.iter().find(|(n, _)| n == name).map(|(_, c)| *c);
            let class = declared.unwrap_or_else(|| crate::look::MaterialClass::infer(category, spec));
            class == crate::look::MaterialClass::Skin
        })
        .map(|(i, _)| i)
        .collect()
}

/// The materials drawn with the hair program, whose colours are its parameters.
fn hair_materials(output: &PartOutput) -> std::collections::BTreeSet<usize> {
    output
        .materials
        .iter()
        .enumerate()
        .filter(
            |(_, (_, spec))| matches!(&spec.shading, Shading::Program { name, .. } if name.as_str() == HAIR_PROGRAM),
        )
        .map(|(i, _)| i)
        .collect()
}

/// Applies the layers to a hair program's colours, each by the share of the material its masks cover (taken at the
/// material's mean: hair has one colour ramp).
fn recolour_program(shading: &mut Shading, resolved: &[Resolved<'_>], frames: &Frames) {
    let Shading::Program { params, .. } = shading else { return };
    for name in HAIR_COLOURS {
        let Ok(key) = Slug::new(name) else { continue };
        let Some(ParamValue::Colour(c)) = params.get(&key).copied() else { continue };
        let mut lin = c.to_linear();
        let neutral =
            Vertex { p: Vec3::ZERO, n: Vec3::Y, m: Membership::default(), height: 0.5, curvature: 0.0, distal: 0.0 };
        let mut skin = [0.0, 1.0, 0.0, 0.0];
        for r in resolved {
            // Spatial masks don't apply to one colour: a hair layer is the whole of the hair's colour.
            let unmasked = MaterialLayer { masks: Vec::new(), ..r.layer.clone() };
            let r2 = Resolved { layer: &unmasked, colour: r.colour, dabs: r.dabs.clone() };
            apply_layer(&r2, &neutral, frames, &mut lin, &mut skin);
        }
        params.insert(key, ParamValue::Colour(Colour::from_linear(lin, c.alpha())));
    }
}

/// The vertices of `mesh` as layers read them.
fn vertices(mesh: &PartMesh, part: Option<&PartFrame>, category: PartCategory, curvature: bool) -> Vec<Vertex> {
    let g = &mesh.geometry;
    let members = Membership::of_mesh(mesh, (category == PartCategory::Head).then_some(RegionGroup::Head));
    let distal: Vec<f32> = if g.is_skinned() {
        let is_distal: Vec<bool> = mesh
            .joint_bones
            .iter()
            .map(|j| matches!(j, JointRef::Humanoid(b) if b.name().ends_with("Distal")))
            .collect();
        g.joints
            .iter()
            .zip(&g.weights)
            .map(|(js, ws)| {
                js.iter().zip(ws).filter(|(j, _)| is_distal.get(usize::from(**j)) == Some(&true)).map(|(_, w)| *w).sum()
            })
            .collect()
    } else {
        vec![0.0; g.positions.len()]
    };
    let k = if curvature { mean_curvature(mesh) } else { vec![0.0; g.positions.len()] };
    g.positions
        .iter()
        .zip(&g.normals)
        .enumerate()
        .map(|(i, (p, n))| Vertex {
            p: *p,
            n: *n,
            m: members[i],
            height: part.map_or(0.5, |f| f.coordinates(*p).y),
            curvature: k[i],
            distal: distal[i],
        })
        .collect()
}

/// Each vertex's mean curvature (1/m, convex positive), from its neighbours across the mesh welded by position (so a
/// UV seam is not an edge): `-2 · mean((q - p)·n) / mean(|q - p|²)`.
fn mean_curvature(mesh: &PartMesh) -> Vec<f32> {
    let g = &mesh.geometry;
    #[allow(clippy::cast_possible_truncation)] // a micrometre grid over a body
    let key = |p: Vec3| ((p.x * 1e6).round() as i64, (p.y * 1e6).round() as i64, (p.z * 1e6).round() as i64);
    let mut welded: HashMap<(i64, i64, i64), usize> = HashMap::new();
    let id: Vec<usize> = g
        .positions
        .iter()
        .map(|p| {
            let n = welded.len();
            *welded.entry(key(*p)).or_insert(n)
        })
        .collect();
    let mut pos = vec![Vec3::ZERO; welded.len()];
    let mut nor = vec![Vec3::ZERO; welded.len()];
    for (i, w) in id.iter().enumerate() {
        pos[*w] = g.positions[i];
        nor[*w] += g.normals[i];
    }
    let mut sums = vec![(0.0f32, 0.0f32); welded.len()];
    for tri in g.indices.as_chunks::<3>().0 {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            let (a, b) = (id[a as usize], id[b as usize]);
            if a == b {
                continue;
            }
            for (p, q) in [(a, b), (b, a)] {
                let d = pos[q] - pos[p];
                let n = nor[p].normalize_or(Vec3::Z);
                sums[p].0 += d.dot(n);
                sums[p].1 += d.length_squared();
            }
        }
    }
    id.iter()
        .map(|w| {
            let (h, l) = sums[*w];
            if l > 1e-12 { -2.0 * h / l } else { 0.0 }
        })
        .collect()
}

/// The `skin-info` values a mesh starts from: its own, or dry, open, unflushed and the surface's curvature.
fn skin_info_of(mesh: &PartMesh, vertices: &[Vertex]) -> Vec<[f32; 4]> {
    if let Some(a) = mesh.geometry.extra_attribute(SKIN_INFO).filter(|a| a.components == 4) {
        return a.data.as_chunks::<4>().0.iter().map(|c| [c[0], c[1], c[2], c[3]]).collect();
    }
    vertices.iter().map(|v| [0.0, 1.0, 0.0, v.curvature.abs()]).collect()
}

fn write_skin_info(mesh: &mut PartMesh, info: &[[f32; 4]]) {
    let data: Vec<f32> = info.iter().flatten().copied().collect();
    let extra = &mut mesh.geometry.extra;
    if let Some(a) = extra.iter_mut().find(|a| a.name.as_str() == SKIN_INFO) {
        *a = ExtraAttribute { name: a.name.clone(), components: 4, data };
    } else if extra.len() < omuzaki_core::mesh::MeshData::MAX_EXTRA_ATTRIBUTES
        && let Ok(name) = Slug::new(SKIN_INFO)
    {
        extra.push(ExtraAttribute { name, components: 4, data });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decal_shapes_hold_their_centres_and_leave_their_corners() {
        for shape in [
            DecalShape::Disc,
            DecalShape::Heart,
            DecalShape::Star,
            DecalShape::Diamond,
            DecalShape::Cross,
            DecalShape::Bar,
        ] {
            assert!(shape.sdf(0.0, 0.0) < 0.0, "{shape:?} misses its centre");
            assert!(shape.sdf(0.49, 0.49) > 0.0, "{shape:?} fills its corner");
        }
        assert!(DecalShape::Ring.sdf(0.0, 0.0) > 0.0);
        assert!(DecalShape::Ring.sdf(0.36, 0.0) < 0.0);
    }

    #[test]
    fn blends_reach_their_targets() {
        let a = Vec3::splat(0.5);
        assert!((Blend::Normal.apply(a, Vec3::ONE, 1.0) - Vec3::ONE).length() < 1e-6);
        assert!((Blend::Multiply.apply(a, Vec3::splat(0.5), 1.0) - Vec3::splat(0.25)).length() < 1e-6);
        assert!((Blend::Screen.apply(a, Vec3::splat(0.5), 1.0) - Vec3::splat(0.75)).length() < 1e-6);
        assert_eq!(Blend::Overlay.apply(a, Vec3::ONE, 0.0), a);
    }

    #[test]
    fn noise_is_deterministic_and_in_range() {
        for i in 0..50u8 {
            let p = Vec3::new(f32::from(i) * 0.37, 1.3, -0.4 * f32::from(i));
            let n = fbm(p, 3);
            assert!((0.0..=1.0).contains(&n));
            assert_eq!(n, fbm(p, 3));
        }
    }

    #[test]
    fn a_gradient_ramps_along_the_height_either_way() {
        let frames = Frames { body: None, head: None, part: None };
        let at = |mask: MaskKind, height: f32| {
            let v = Vertex { p: Vec3::ZERO, n: Vec3::Y, m: Membership::default(), height, curvature: 0.0, distal: 0.0 };
            mask_value(&Mask { mask, invert: false }, &v, &frames)
        };
        let up = MaskKind::gradient(0.2, 0.8);
        assert!(at(up.clone(), 0.1) < 1e-6 && (at(up.clone(), 0.9) - 1.0).abs() < 1e-6);
        assert!((at(up.clone(), 0.5) - 0.5).abs() < 1e-6);
        let down = MaskKind::gradient(1.0, 0.0);
        assert!(at(down.clone(), 0.9) < at(down.clone(), 0.5) && at(down.clone(), 0.5) < at(down, 0.1));
        let id = LayerId::new("fade").unwrap();
        let layer = |mask| MaterialLayer {
            masks: vec![Mask { mask, invert: false }],
            ..MaterialLayer::new(id.clone(), LayerKind::Wet { amount: 0.5 })
        };
        assert!(layer(MaskKind::gradient(1.0, 0.0)).check().is_ok());
        assert!(layer(MaskKind::gradient(0.5, 0.5)).check().is_err());
        assert!(layer(MaskKind::gradient(0.0, 1.5)).check().is_err());
    }

    #[test]
    fn a_layer_checks_its_numbers() {
        let id = LayerId::new("wet").unwrap();
        assert!(MaterialLayer::new(id.clone(), LayerKind::Wet { amount: 0.5 }).check().is_ok());
        assert!(MaterialLayer::new(id, LayerKind::Wet { amount: 1.5 }).check().unwrap_err().starts_with("kind/amount"));
    }
}
