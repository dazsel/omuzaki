//! Headphones (CB-43): `headphones`, a band over the head and a cup at each ear, with optional cat-ear shells on the band
//! and a mic boom.
//!
//! **Build.** The band is a flat tube in the plane of the ears: its centre line stands `clearance` metres above the head's
//! surface (the head, because the hair publishes no envelope yet: the default clears the hair of the styles on `main`
//! by more than 2 mm) from one cup over the crown to the other, and ends inside the cups. Each cup is a rounded disc
//! whose axis is the head's across axis, centred level with its ear and `cup_gap` outside the ear's root, where the
//! ear anchor (`ear_l`, `ear_r`, as the head publishes them) says. A head without a side ear (animal ears, none)
//! has no such anchor, so the cup sits at `ear_height` below the head centre on the side of the head.
//!
//! **Both sides.** Each side is generated with its own sign `s` (`+1` the character's left, `-1` the right), so the pair
//! is the exact mirror image of one another.
//!
//! Skinned rigidly to the head joint. With an `attach` the whole piece moves as one.

use omuzaki_char_api::{
    AnchorsFact, BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput, RecolourCtx, Recoloured,
    RigContribution,
};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::rigid::{Detail, kit_error};
use super::shells::{Shells, Slot, closed_tube, head_frames, recolour};
use crate::kit::KitError;
use crate::kit::curve::{fraction, lerp};
use crate::kit::grid::Grid;
use crate::kit::sweep::Sweep;

/// Headphones.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HeadphonesParams {
    /// Width of the band along the head's front-back axis, metres.
    pub band_width: f64,
    /// Thickness of the band, metres.
    pub band_thickness: f64,
    /// How far the band's centre line stands above the head's surface, metres: more than the hair is thick.
    pub clearance: f64,
    /// Radius of a cup, metres.
    pub cup_radius: f64,
    /// Depth of a cup, metres.
    pub cup_depth: f64,
    /// The gap between the ear's root and the cup's inner face, metres: the ear sits in it.
    pub cup_gap: f64,
    /// Where the cups sit when the head publishes no ear anchor: their height over the head centre, metres.
    pub ear_height: f64,
    /// Cat-ear shells on the band.
    pub cat_ears: bool,
    /// A mic boom from the left cup.
    pub mic: bool,
    /// The band and the cat ears' outer shell.
    pub band: ColourRef,
    /// The cups.
    pub cup: ColourRef,
    /// The cat ears' inner shell and the mic.
    pub accent: ColourRef,
}

impl Default for HeadphonesParams {
    fn default() -> Self {
        Self {
            band_width: 0.03,
            band_thickness: 0.014,
            clearance: 0.064,
            cup_radius: 0.06,
            cup_depth: 0.036,
            cup_gap: 0.035,
            ear_height: -0.02,
            cat_ears: false,
            mic: false,
            band: ColourRef::rgb(0x3a, 0x38, 0x48),
            cup: ColourRef::rgb(0xe8, 0x6a, 0x92),
            accent: ColourRef::rgb(0xf6, 0xd0, 0xdc),
        }
    }
}

impl Validate for HeadphonesParams {
    fn check(&self, c: &mut Check) {
        c.range("band_width", self.band_width, 0.006, 0.05);
        c.range("band_thickness", self.band_thickness, 0.003, 0.03);
        c.range("clearance", self.clearance, 0.0, 0.15);
        c.range("cup_radius", self.cup_radius, 0.02, 0.1);
        c.range("cup_depth", self.cup_depth, 0.01, 0.06);
        c.range("cup_gap", self.cup_gap, 0.0, 0.08);
        c.range("ear_height", self.ear_height, -0.1, 0.1);
    }
}

/// How round the rim of a cup is: the superellipse exponent of its profile (2 is a lens, large is a flat-topped drum).
const CUP_ROUNDNESS: f64 = 4.0;
/// How far the band's ends reach into a cup, metres, so no open end shows.
const END_INTO_CUP: f64 = 0.006;
/// Where the cat ears stand on the band, degrees from the top, towards each side.
const CAT_EAR_AT_DEG: f64 = 40.0;

/// The `headphones` kind.
pub struct Headphones;

/// Where one side's cup sits and where its ear is.
struct Cup {
    /// The side: `+1` the left, `-1` the right.
    s: f64,
    /// The ear's root.
    ear: DVec3,
    /// The cup's centre.
    centre: DVec3,
}

impl Headphones {
    /// The ear root of side `s`: the anchor the head published, else a point on the side of the head.
    fn ear(ctx: &BuildCtx<'_>, p: &HeadphonesParams, s: f64, head: &dyn omuzaki_char_api::HeadSurface) -> DVec3 {
        let name = if s > 0.0 { "ear_l" } else { "ear_r" };
        let anchors = ctx.published.get::<AnchorsFact>();
        if let Some((_, anchor)) = anchors.iter().find(|((_, anchor), _)| anchor.as_str() == name) {
            return DVec3::from(anchor.position);
        }
        #[allow(clippy::cast_possible_truncation)] // metres
        let height = p.ear_height as f32;
        let dir = if s > 0.0 { Vec3::X } else { Vec3::NEG_X };
        let on_side = head.point(dir, 0.0);
        DVec3::from(Vec3::new(on_side.x, head.centre().y + height, head.centre().z))
    }

    /// The path of the band: out of the left cup, over the crown, into the right cup.
    fn band_path(
        p: &HeadphonesParams,
        head: &dyn omuzaki_char_api::HeadSurface,
        cups: &[Cup; 2],
        rows: usize,
    ) -> Vec<DVec3> {
        let centre = DVec3::from(head.centre());
        let plane_z = f64::midpoint(cups[0].ear.z, cups[1].ear.z) - centre.z;
        #[allow(clippy::cast_possible_truncation)] // metres
        let on_arc = |alpha: f64| -> DVec3 {
            let along = DVec3::new(alpha.sin(), alpha.cos(), 0.0);
            let dir = (along * f64::from(head.radius(Vec3::Y)) + DVec3::Z * plane_z).normalize_or_zero();
            DVec3::from(head.point(dir.as_vec3(), p.clearance as f32))
        };
        let top_of = |cup: &Cup| cup.centre + DVec3::Y * (0.8 * p.cup_radius);
        // The arc starts where it clears the cup's top: the side the cup is on, a little above its top.
        let reach = {
            let t = top_of(&cups[0]) - centre;
            t.x.atan2(t.y).min(1.35)
        };
        let from = 0.8 * reach;
        let mut path = Vec::with_capacity(rows + 12);
        let left_top = top_of(&cups[0]);
        path.push(left_top - DVec3::Y * END_INTO_CUP);
        path.push(left_top);
        let arc_start = on_arc(from);
        for k in 1..4 {
            path.push(left_top.lerp(arc_start, fraction(k, 4)));
        }
        for k in 0..=rows {
            path.push(on_arc(lerp(from, -from, fraction(k, rows))));
        }
        let right_top = top_of(&cups[1]);
        let arc_end = on_arc(-from);
        for k in 1..4 {
            path.push(arc_end.lerp(right_top, fraction(k, 4)));
        }
        path.push(right_top);
        path.push(right_top - DVec3::Y * END_INTO_CUP);
        path
    }

    /// A cup: a rounded disc about the across axis, as a lathe grid whose inner face looks at the head.
    fn cup(p: &HeadphonesParams, cup: &Cup, rows: usize, seg: usize) -> Result<Grid, KitError> {
        let axis = DVec3::X * cup.s;
        let origin = cup.centre - axis * (0.5 * p.cup_depth);
        let profile: Vec<(f64, f64)> = (0..=rows)
            .map(|k| {
                let u = fraction(k, rows);
                let r = (1.0 - (2.0 * u - 1.0).abs().powf(CUP_ROUNDNESS)).max(0.0).powf(1.0 / CUP_ROUNDNESS);
                (u * p.cup_depth, p.cup_radius * r)
            })
            .collect();
        crate::kit::loft::lathe_axis(origin, axis, &profile, seg)
    }

    /// A cat ear: a flattened cone standing on the band at `angle` from the top, towards side `s`, with `scale` of the
    /// full size (the inner shell is smaller).
    fn cat_ear(
        head: &dyn omuzaki_char_api::HeadSurface,
        p: &HeadphonesParams,
        s: f64,
        scale: f64,
        forward: f64,
        seg: usize,
    ) -> Result<Sweep, KitError> {
        let alpha = s * CAT_EAR_AT_DEG.to_radians();
        let out = DVec3::new(alpha.sin(), alpha.cos(), 0.0);
        #[allow(clippy::cast_possible_truncation)] // metres
        let root = DVec3::from(head.point(out.as_vec3(), (p.clearance + 0.5 * p.band_thickness) as f32));
        let base = root + DVec3::Z * forward;
        let height = 0.055 * scale;
        let path: Vec<DVec3> = (0..=6).map(|k| base + out * (fraction(k, 6) * height - 0.004)).collect();
        let half_across = 0.026 * scale;
        let half_thick = 0.008 * scale;
        // The across axis of the ear is the band's tangent: the sweep's first normal is cut from it.
        let tangent = DVec3::new(alpha.cos(), -alpha.sin(), 0.0) * s;
        closed_tube(
            &path,
            |t| {
                let k = (1.0 - t).powf(0.85);
                (half_across * k, half_thick * k)
            },
            seg,
            tangent,
        )
    }

    /// The mic boom: a thin curved tube from the left cup's outer face to the mouth, and its capsule.
    fn mic(
        head: &dyn omuzaki_char_api::HeadSurface,
        p: &HeadphonesParams,
        cup: &Cup,
        rows: usize,
        seg: usize,
    ) -> Result<(Sweep, Sweep), KitError> {
        let start = cup.centre + DVec3::X * (0.5 * p.cup_depth * cup.s) - DVec3::Y * (0.4 * p.cup_radius);
        #[allow(clippy::cast_possible_truncation)] // metres
        let end = DVec3::from(head.point(Vec3::new(0.32, -0.62, 0.72).normalize(), 0.03));
        let control = start + DVec3::new(0.0, -0.04, 0.5 * (end.z - start.z)) + DVec3::X * 0.015;
        let curve = |u: f64| start * (1.0 - u) * (1.0 - u) + control * (2.0 * (1.0 - u) * u) + end * (u * u);
        let path: Vec<DVec3> = (0..=rows).map(|k| curve(fraction(k, rows))).collect();
        let boom = closed_tube(&path, |_| (0.0028, 0.0028), seg.min(8), DVec3::Z)?;
        // The capsule: a short fat tube along the boom's last direction.
        let tip = path[rows] - path[rows - 1];
        let along = tip.normalize_or_zero();
        let capsule: Vec<DVec3> = (0..=6).map(|k| end + along * (fraction(k, 6) * 0.018 - 0.006)).collect();
        let bulb = closed_tube(
            &capsule,
            |t| {
                let r = 0.008 * (1.0 - (2.0 * t - 1.0).powi(2)).max(0.0).sqrt();
                (r, r)
            },
            seg.min(10),
            DVec3::Y,
        )?;
        Ok((boom, bulb))
    }
}

impl PartKind for Headphones {
    type Params = HeadphonesParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "headphones",
            summary: "Build headphones: a band over the head, a cup at each ear, optional cat ears and a mic boom.",
            category: PartCategory::Accessory,
            requires: &["head"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &HeadphonesParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let (attach, default) = head_frames(ctx)?;
        let detail = Detail::of(ctx.quality);
        let err = kit_error(ctx);
        let mut shells = Shells::new(&attach, &default, 3);
        let seg = detail.lathe;
        let cup_rows = detail.rows / 2;
        let cups: [Cup; 2] = [1.0, -1.0].map(|s| {
            let ear = Self::ear(ctx, p, s, head.as_ref());
            let centre = ear + DVec3::X * (s * (p.cup_gap + 0.5 * p.cup_depth));
            Cup { s, ear, centre }
        });
        let inside = |sw: &Sweep| {
            let centres = sw.centres.clone();
            move |i: usize, _: usize| centres[i]
        };
        let path = Self::band_path(p, head.as_ref(), &cups, detail.rows);
        let band =
            closed_tube(&path, |_| (0.5 * p.band_width, 0.5 * p.band_thickness), seg / 2, DVec3::Z).map_err(&err)?;
        shells.bind(0, &band.grid, (false, true), &inside(&band)).map_err(&err)?;
        for cup in &cups {
            let grid = Self::cup(p, cup, cup_rows, seg).map_err(&err)?;
            // The lathe's rows run from the inner face to the outer one along the axis, so the interior reference of row
            // `i` is the axis point of that row.
            let axis_point =
                |i: usize, _: usize| cup.centre + DVec3::X * (cup.s * (fraction(i, cup_rows) - 0.5) * p.cup_depth);
            shells.bind(1, &grid, (false, true), &axis_point).map_err(&err)?;
        }
        if p.cat_ears {
            for s in [1.0, -1.0] {
                for (slot, scale, forward) in [(0, 1.0, 0.0), (2, 0.62, 0.006)] {
                    let ear = Self::cat_ear(head.as_ref(), p, s, scale, forward, 4).map_err(&err)?;
                    shells.bind(slot, &ear.grid, (false, true), &inside(&ear)).map_err(&err)?;
                }
            }
        }
        if p.mic {
            let (boom, bulb) = Self::mic(head.as_ref(), p, &cups[0], detail.rows, seg).map_err(&err)?;
            shells.bind(2, &boom.grid, (false, true), &inside(&boom)).map_err(&err)?;
            shells.bind(2, &bulb.grid, (false, true), &inside(&bulb)).map_err(&err)?;
        }
        shells.output(ctx, &Self::slots(p), RigContribution::default())
    }

    fn recolour(p: &HeadphonesParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour(out, ctx, &Self::slots(p))
    }
}

impl Headphones {
    fn slots(p: &HeadphonesParams) -> [Slot; 3] {
        [Slot::opaque("band", &p.band), Slot::opaque("cup", &p.cup), Slot::opaque("accent", &p.accent)]
    }
}

omuzaki_char_api::register_part_kind!(Headphones);
