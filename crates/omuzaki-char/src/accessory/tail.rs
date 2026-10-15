//! Tails (CB-43): `tail`, four presets (fox, cat, dragon, bunny) on an N-bone spring chain, rooted at the tailbone.
//!
//! **Build.** The tail's rest curve starts inside the back at the tailbone (the hips bone's frame, `attach` moves it),
//! leaves backwards and up, and curls up through `curl_deg`. Each preset is a closed tube along it: a fox's fluffy and
//! fullest in the middle with a `tuft` colour at the tip, a cat's slim and even, a dragon's thick at the root and
//! tapering to a point with a spade, a bunny's a ball. Lengths are for a standard-height body and scale with the body
//! (and the attach scale).
//!
//! **Rig.** `bones` bones lie along the curve, the first at the tailbone and the last at the tip, each the child of the one
//! before; the first hangs from the attach joint. A spring chain over them ([`SpringChain::tapered`]) runs from
//! `stiffness` at the root to `stiffness_tip` at the tip, so the root follows the hips and the tip trails and settles.
//! Every vertex is skinned to the two bones either side of it, so the tube bends smoothly. The chain is simulated in
//! world space, not in the hips' space: a turn of the hips is exactly what makes the tail lag. It declares the colliders
//! earlier parts published. The chain exports as a VRM spring chain.
//!
//! **Hair and clothes** may cover the root: the first rings are inside the back.

use omuzaki_anim_api::SpringParams;
use omuzaki_char_api::{
    BuildCtx, BuildError, JointRef, PartCategory, PartKind, PartMeta, PartOutput, RecolourCtx, Recoloured,
    RigContribution,
};
use omuzaki_core::glam::{DQuat, DVec3, Vec3};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::attach::{ResolvedAttach, attachment, on_bone};
use super::rigid::{Detail, kit_error};
use super::shells::{
    Chain, Shells, Slot, body_scale, chain_weights, closed_tube, ellipsoid, recolour, spring_chain, torso_back,
};
use crate::kit::KitError;
use crate::kit::curve::{clamp, fraction, lerp};
use crate::kit::grid::Skin;
use crate::kit::sweep::Sweep;

/// The kind of tail.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TailKind {
    /// Long and fluffy, with a pale tip.
    #[default]
    Fox,
    /// Slim and even, curling at the end.
    Cat,
    /// Thick at the root, tapering to a spade.
    Dragon,
    /// A short ball.
    Bunny,
}

/// What a kind is when the parameters leave it open (metres at a standard body).
struct Preset {
    length: f64,
    thickness: f64,
    curl_deg: f64,
    tuft: f64,
    bones: u8,
}

impl TailKind {
    fn preset(self) -> Preset {
        match self {
            Self::Fox => Preset { length: 0.4, thickness: 0.05, curl_deg: 55.0, tuft: 0.3, bones: 5 },
            Self::Cat => Preset { length: 0.45, thickness: 0.015, curl_deg: 80.0, tuft: 0.0, bones: 6 },
            Self::Dragon => Preset { length: 0.5, thickness: 0.035, curl_deg: 25.0, tuft: 0.0, bones: 6 },
            Self::Bunny => Preset { length: 0.08, thickness: 0.04, curl_deg: 0.0, tuft: 0.0, bones: 3 },
        }
    }
}

/// A tail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TailParams {
    /// The preset.
    pub kind: TailKind,
    /// Length of the rest curve, metres at a standard body. Left out: the preset's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    /// Radius at its widest, metres at a standard body. Left out: the preset's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thickness: Option<f64>,
    /// How far the curve turns upward over its length, degrees. Left out: the preset's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curl_deg: Option<f64>,
    /// The share of the length the tip colour covers, `0..=1`. Left out: the preset's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tuft: Option<f64>,
    /// Bones in the spring chain, `3..=8`. Left out: the preset's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bones: Option<u8>,
    /// How firmly the root is pulled back to its rest, VRM's `stiffness`.
    pub stiffness: f64,
    /// The same at the tip.
    pub stiffness_tip: f64,
    /// How much of its speed each bone sheds each step, `0..=1` (VRM's `dragForce`).
    pub drag: f64,
    /// The pull of gravity on the bones (VRM's `gravityPower`).
    pub gravity: f64,
    /// The body's colour.
    pub tail: ColourRef,
    /// The tip's colour.
    pub tuft_colour: ColourRef,
    /// The dragon's spade.
    pub accent: ColourRef,
}

impl Default for TailParams {
    fn default() -> Self {
        Self {
            kind: TailKind::Fox,
            length: None,
            thickness: None,
            curl_deg: None,
            tuft: None,
            bones: None,
            stiffness: 0.8,
            stiffness_tip: 0.35,
            drag: 0.35,
            gravity: 0.1,
            tail: ColourRef::rgb(0xd8, 0x7a, 0x38),
            tuft_colour: ColourRef::rgb(0xf8, 0xf0, 0xe0),
            accent: ColourRef::rgb(0x8a, 0x2a, 0x3a),
        }
    }
}

impl Validate for TailParams {
    fn check(&self, c: &mut Check) {
        if let Some(length) = self.length {
            c.range("length", length, 0.03, 1.5);
        }
        if let Some(thickness) = self.thickness {
            c.range("thickness", thickness, 0.005, 0.15);
        }
        if let Some(curl) = self.curl_deg {
            c.range("curl_deg", curl, -30.0, 180.0);
        }
        if let Some(tuft) = self.tuft {
            c.range("tuft", tuft, 0.0, 1.0);
        }
        if let Some(bones) = self.bones {
            c.range("bones", f64::from(bones), 3.0, 8.0);
        }
        c.range("stiffness", self.stiffness, 0.0, 4.0);
        c.range("stiffness_tip", self.stiffness_tip, 0.0, 4.0);
        c.range("drag", self.drag, 0.0, 1.0);
        c.range("gravity", self.gravity, 0.0, 2.0);
    }
}

/// The parameters with the preset's own filled in.
struct Resolved {
    length: f64,
    thickness: f64,
    curl: f64,
    tuft: f64,
    bones: usize,
}

impl TailParams {
    fn resolved(&self, scale: f64) -> Resolved {
        let d = self.kind.preset();
        Resolved {
            length: self.length.unwrap_or(d.length) * scale,
            thickness: self.thickness.unwrap_or(d.thickness) * scale,
            curl: self.curl_deg.unwrap_or(d.curl_deg).to_radians(),
            tuft: self.tuft.unwrap_or(d.tuft),
            bones: usize::from(self.bones.unwrap_or(d.bones)),
        }
    }

    fn slots(&self) -> [Slot; 3] {
        [
            Slot::opaque("tail", &self.tail),
            Slot::opaque("tuft", &self.tuft_colour),
            Slot::opaque("accent", &self.accent),
        ]
    }
}

/// How deep the tail's root starts inside the back, metres at a standard body.
const ROOT_DEPTH: f64 = 0.012;
/// The share of the length over which the root closes (inside the body).
const ROOT_CAP: f64 = 0.04;
/// How much wider than the tail the tuft is, metres at a standard body.
const SLEEVE: f64 = 6e-4;

/// The `tail` kind.
pub struct Tail;

/// The rest curve and the radius of a tail, in the accessory's local frame.
struct Shape<'a> {
    p: &'a TailParams,
    r: &'a Resolved,
}

impl Shape<'_> {
    /// The rest curve: `rows + 1` points from the root, evenly spaced along its length, in the plane of the body's
    /// middle. It leaves backwards (-Z) and up, then turns up through `curl`.
    fn curve(&self, root: DVec3, rows: usize) -> Vec<DVec3> {
        let ds = self.r.length / usize_f(rows);
        let mut at = root;
        let mut points = vec![root];
        for k in 0..rows {
            let t = (usize_f(k) + 0.5) / usize_f(rows);
            // The angle above straight back: starts a little up and turns by `curl`, a dragon in an S.
            let lead = 0.3 + self.r.curl * t.powf(1.4) - self.sway(t);
            let d = DQuat::from_rotation_x(lead) * -DVec3::Z;
            at += d * ds;
            points.push(at);
        }
        points
    }

    /// A dragon's tail rises and then falls back: the turn at `t` that the S takes off the curl.
    fn sway(&self, t: f64) -> f64 {
        if self.p.kind == TailKind::Dragon { 0.9 * (std::f64::consts::PI * t).sin() * (1.0 - t) } else { 0.0 }
    }

    /// The tube's radius at fraction `t` of the length.
    fn radius(&self, t: f64) -> f64 {
        let thick = self.r.thickness;
        let body = match self.p.kind {
            // Fluffy: widest a third of the way along, closing in a rounded tip.
            TailKind::Fox => 0.35 + 0.65 * (std::f64::consts::PI * t.powf(0.75)).sin(),
            TailKind::Cat => lerp(1.0, 0.7, t),
            TailKind::Dragon => (1.0 - t).powf(0.9).max(0.08),
            TailKind::Bunny => (1.0 - (2.0 * t - 1.0).powi(2)).max(0.0).sqrt(),
        };
        let start = {
            let x = clamp(t / ROOT_CAP, 0.0, 1.0);
            (1.0 - (1.0 - x) * (1.0 - x)).sqrt()
        };
        let end = {
            let x = clamp((t - 0.94) / 0.06, 0.0, 1.0);
            (1.0 - x * x).sqrt()
        };
        thick * body * start * end
    }
}

#[allow(clippy::cast_precision_loss)] // a row count
fn usize_f(n: usize) -> f64 {
    n as f64
}

impl Tail {
    /// The tailbone: where the curve starts, local frame. It sits `ROOT_DEPTH` inside the back at the height of the hips.
    fn root(ctx: &BuildCtx<'_>, attach: &ResolvedAttach, scale: f64) -> Result<DVec3, BuildError> {
        let hips =
            ctx.joint(&JointRef::HIPS).ok_or_else(|| ctx.fail("the body has no hips joint to root the tail on"))?;
        let back = torso_back(ctx)?;
        // The back at the hips: the nearest sample to the hips' height.
        let at = back
            .iter()
            .min_by(|a, b| (a.0 - f64::from(hips.y)).abs().total_cmp(&(b.0 - f64::from(hips.y)).abs()))
            .ok_or_else(|| ctx.fail("the body has no torso to root the tail on"))?;
        let bind = DVec3::new(f64::from(hips.x), f64::from(hips.y) - 0.03 * scale, at.1 + ROOT_DEPTH * scale);
        Ok(attach.to_bind.inverse().transform_point3(bind))
    }

    fn sleeve(sh: &Shape<'_>, path: &[DVec3], seg: usize) -> Result<Option<Sweep>, KitError> {
        let rows = path.len() - 1;
        let from = ((1.0 - sh.r.tuft) * usize_f(rows)).floor();
        if sh.r.tuft <= 0.0 || sh.p.kind == TailKind::Bunny {
            return Ok(None);
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a row index below `rows`
        let from = (from as usize).min(rows - 2);
        let start = fraction(from, rows);
        let width = SLEEVE * sh.r.thickness / 0.05;
        let radius = |u: f64| {
            let band = clamp(u / 0.12, 0.0, 1.0);
            (sh.radius(lerp(start, 1.0, u)) + width) * (1.0 - (1.0 - band) * (1.0 - band)).sqrt()
        };
        closed_tube(&path[from..], |u| (radius(u), radius(u)), seg, DVec3::Y).map(Some)
    }
}

impl PartKind for Tail {
    type Params = TailParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "tail",
            summary: "Build a tail (fox, cat, dragon or bunny) on a spring-bone chain rooted at the tailbone.",
            category: PartCategory::Accessory,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &TailParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let attach = attachment(ctx, |c| on_bone(c, &JointRef::HIPS))?;
        let scale = body_scale(ctx)? * attach.scale();
        let r = p.resolved(scale);
        let detail = Detail::of(ctx.quality);
        let err = kit_error(ctx);
        let sh = Shape { p, r: &r };
        // The rows come in whole segments between bones, so each bone sits on a ring.
        let per_segment = (detail.rows / (r.bones - 1)).max(2);
        let rows = per_segment * (r.bones - 1);
        let root = Self::root(ctx, &attach, scale)?;
        let path = sh.curve(root, rows);
        // The bones: one per segment boundary, bind space.
        let points: Vec<DVec3> = (0..r.bones).map(|b| attach.point(path[b * per_segment])).collect();
        let hit = SpringParams {
            hit_radius: 0.0,
            stiffness: narrow(p.stiffness),
            gravity_power: narrow(p.gravity),
            gravity_dir: Vec3::NEG_Y,
            drag_force: narrow(p.drag),
        };
        let tip = SpringParams { stiffness: narrow(p.stiffness_tip), ..hit };
        let mut rig = RigContribution::default();
        let chain = Chain { tag: "", parent: attach.joint.clone(), points: &points, root: hit, tip, centre: None };
        let joints = spring_chain(ctx, "tail", &mut rig, &chain)?;
        let mut shells = Shells::new(&attach, &attach, 3);
        let seg = detail.lathe * 3 / 4;
        let body = closed_tube(&path, |t| (sh.radius(t), sh.radius(t)), seg, DVec3::Y).map_err(&err)?;
        let weights = |_: DVec3, i: usize, _: usize| chain_weights(&joints, fraction(i, rows));
        let centres = body.centres.clone();
        shells.local(0, &body.grid, (false, true), &|i, _| centres[i], Skin::Per(&weights)).map_err(&err)?;
        if let Some(tuft) = Self::sleeve(&sh, &path, seg).map_err(&err)? {
            let from = rows - (tuft.centres.len() - 1);
            let weights = |_: DVec3, i: usize, _: usize| chain_weights(&joints, fraction(from + i, rows));
            let centres = tuft.centres.clone();
            shells.local(1, &tuft.grid, (false, true), &|i, _| centres[i], Skin::Per(&weights)).map_err(&err)?;
        }
        if p.kind == TailKind::Dragon {
            // The spade: a flat leaf at the tip, skinned to the last two bones.
            let tip_at = path[rows];
            let back = (path[rows] - path[rows - 1]).normalize_or_zero();
            let centre = tip_at + back * (0.5 * r.thickness * 1.6);
            let spade = ellipsoid(DVec3::ZERO, DVec3::new(0.2, 1.0, 0.5), 10, 12).map_err(&err)?;
            let size = r.thickness * 1.9;
            let frame = DQuat::from_rotation_arc(DVec3::Y, back);
            let grid = crate::kit::grid::Grid::new(
                spade.rows(),
                spade.cols(),
                spade.points().iter().map(|q| centre + frame * (*q * size)).collect(),
            )
            .map_err(&err)?;
            let weights = |_: DVec3, _: usize, _: usize| chain_weights(&joints, 1.0);
            shells.local(2, &grid, (false, true), &|_, _| centre, Skin::Per(&weights)).map_err(&err)?;
        }
        shells.output(ctx, &p.slots(), rig)
    }

    fn recolour(p: &TailParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour(out, ctx, &p.slots())
    }
}

#[allow(clippy::cast_possible_truncation)] // spring parameters are f32
fn narrow(v: f64) -> f32 {
    v as f32
}

omuzaki_char_api::register_part_kind!(Tail);
