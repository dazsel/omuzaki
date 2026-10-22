//! The hood (#325) the coats share (the cloak, the robe, the hoodie): folded down, a soft pouch lying on the upper back
//! with a rolled edge round it, or raised over the head, clear of the head and the hair.
//!
//! - **Down** is a [`patch`] on the coat's outside: from just under the collar line round the back between the
//!   shoulders, deepest at the centre back, standing off the cloth by its pouch.
//! - **Up** is a two-layer shell over the head built in two mirrored halves. Each half runs from the face opening's rim
//!   (a circle round the face's forward axis) back to the centre-back seam (in the middle plane, from the nape over the
//!   crown to the forehead's top, where the rims meet), and below the head down to the coat's collar. Every point
//!   stands off the head by the clearance and the hair the hair part published ([`HairReachFact`]), smoothed so the
//!   cloth doesn't follow single locks, and off the neck by the coat's own thickness. The seam's points lie exactly on
//!   the middle plane, so the halves close there; the rims, the bottom and the top form one border, rolled.

use std::f64::consts::PI;

use omuzaki_char_api::{BuildCtx, BuildError, HeadSurface, JointRef};
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::Check;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::coat::Front;
use super::coat::edge::{Patch, Tube, loop_tube, patch};
use super::coat::shell::{Frame, Skinning, Tones};
use super::common::GarmentError;
use crate::hair::reach::{HairReach, HairReachFact};
use crate::kit::curve::{fraction, lerp, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin, grid_offset};
use crate::outfit::trousers::rows::mix_skin;

/// The folded hood's points across the back and down it.
const DOWN_GRID: (usize, usize) = (10, 33);
/// The folded hood: how far round from the centre back it reaches either side (radians), its depth at the centre back
/// and at its ends (fractions of the torso), and how far under the collar line its top edge lies (metres).
const DOWN: (f64, f64, f64, f64) = (1.45, 0.3, 0.06, 0.004);
/// How far round from the centre front the folded hood stays clear of an open front's edge, radians.
const DOWN_FRONT: f64 = 0.3;
/// The raised hood's rows over the head, rows down the neck, and points from the rim to the seam.
const UP_GRID: (usize, usize, usize) = (24, 6, 18);
/// The face opening: its angular radius round the forward axis, how far the axis tilts up, and how far down round the
/// face the rim reaches (radians from the top).
const OPENING: (f64, f64, f64) = (1.08, 0.15, 2.25);
/// The seam's angle in the middle plane at the bottom (below the back of the head), radians from the horizontal.
const NAPE: f64 = -0.45;
/// How far round from the centre front the raised hood meets the collar, radians, at least.
const UP_FRONT: f64 = 0.55;
/// The smoothing of the raised hood's stand-off: the window of the largest and of the mean, in grid points.
const SMOOTH: (usize, usize) = (2, 2);
/// The raised hood's extra fullness at the back of the head, metres per unit of size above 1.
const FULLNESS: f64 = 0.02;
/// The most hair the raised hood clears, metres: below the head, and over its cap.
const HAIR_CAP: (f64, f64) = (0.012, 0.06);
/// The rolled edge's lip over the cloth, metres.
const ROLL_LIP: f64 = 0.0008;

/// How a coat's hood is worn.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HoodStyle {
    /// None.
    None,
    /// Folded down on the upper back.
    #[default]
    Down,
    /// Raised over the head.
    Up,
}

/// A hood.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Hood {
    /// How it is worn.
    pub style: HoodStyle,
    /// Its size: 1 the usual, larger for a deep, roomy hood.
    pub size: f64,
    /// How far the folded hood's pouch stands off the back, metres.
    pub pouch: f64,
    /// How far the raised hood stands clear of the head and the hair, metres.
    pub clearance: f64,
}

impl Default for Hood {
    fn default() -> Self {
        Self { style: HoodStyle::Down, size: 1.0, pouch: 0.012, clearance: 0.008 }
    }
}

impl Hood {
    /// Checks the hood, its fields under `c`.
    pub fn check(&self, c: &mut Check) {
        c.range("size", self.size, 0.5, 2.0);
        c.range("pouch", self.pouch, 0.0, 0.06);
        c.range("clearance", self.clearance, 0.006, 0.05);
    }

    /// The open borders the hood adds: two (its outside's and its lining's, under the roll), none without one.
    pub fn borders(&self) -> usize {
        if self.style == HoodStyle::None { 0 } else { 2 }
    }
}

/// Builds `hood` on the coat of `frame`, in `tones`: the mesh to add as the coat's `hood`, or nothing without one.
///
/// # Errors
/// [`BuildError::Missing`] for a raised hood on a character without a head; the kit's.
pub fn build(ctx: &BuildCtx<'_>, frame: &Frame, hood: &Hood, tones: &Tones) -> Result<Option<MeshBuilder>, BuildError> {
    let fail = |e: GarmentError| ctx.fail(format!("hood: {e}"));
    let mut mesh = MeshBuilder::with_colours();
    match hood.style {
        HoodStyle::None => return Ok(None),
        HoodStyle::Down => down(&mut mesh, frame, hood, tones).map_err(fail)?,
        HoodStyle::Up => {
            let head = ctx.head()?;
            let hair = ctx.published.get::<HairReachFact>().clone();
            up(&mut mesh, frame, hood, tones, head.as_ref(), hair.as_ref()).map_err(fail)?;
        }
    }
    Ok(Some(mesh))
}

/// How far round from the centre front the hood may come at the collar: clear of an open front's edges.
fn front_limit(frame: &Frame, least: f64, clear: f64) -> f64 {
    match frame.cut.front {
        Front::Closed => least,
        Front::Open { .. } => least.max(frame.edge(frame.top) + clear),
    }
}

/// The folded hood: a pouch on the back under the collar line.
#[allow(clippy::many_single_char_names)] // the grid's textbook names
fn down(mesh: &mut MeshBuilder, frame: &Frame, hood: &Hood, tones: &Tones) -> Result<(), GarmentError> {
    let (rows, cols) = DOWN_GRID;
    let (reach, deep, shallow, under) = DOWN;
    let len = frame.column.length();
    // Either side of the centre back, short of an open front's edges.
    let reach = (reach * hood.size.sqrt()).min(PI - front_limit(frame, 0.0, DOWN_FRONT));
    let lift = frame.cut.lip + 0.0005;
    let (mut points, mut normals, mut skins) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..rows {
        let s = fraction(i, rows - 1);
        for j in 0..cols {
            let t = fraction(j, cols - 1) * 2.0 - 1.0;
            let phi = PI + t * reach;
            let top = frame.neck(phi) - under;
            let depth = len * hood.size * lerp(deep, shallow, t.abs().powf(1.4));
            let y = (top - s * depth).max(frame.hem + 0.02);
            let pouch = hood.pouch * (PI * s).sin().powf(0.8) * (1.0 - t * t);
            let (p, n, skin) = frame.on(y, phi, lift + pouch)?;
            points.push(p);
            normals.push(n);
            skins.push(skin);
        }
    }
    let piece = Patch {
        rows,
        cols,
        points,
        normals,
        skins,
        thickness: frame.cut.thickness * 1.5,
        colours: (tones.outer, tones.lining),
        edge: tones.edge,
    };
    patch(mesh, &piece)
}

/// The raised hood's directions from the head's centre, rows (bottom to top) by points (rim to seam), for the left
/// half: rows run from the bottom of the head up to where the rims meet the seam.
fn directions() -> Vec<Vec<DVec3>> {
    let (rows, _, cols) = UP_GRID;
    let (radius, tilt, down) = OPENING;
    let forward = DVec3::new(0.0, tilt.sin(), tilt.cos());
    let up = (DVec3::Y - forward * forward.y).normalize();
    let left = DVec3::X;
    let rim = |w: f64| forward * radius.cos() + (up * w.cos() + left * w.sin()) * radius.sin();
    let crown = rim(0.0);
    // The seam's angle where the rims meet it, in the middle plane: up from the back's horizontal.
    let meet = crown.y.atan2(-crown.z);
    let seam = |b: f64| {
        let a = lerp(NAPE, meet, b);
        DVec3::new(0.0, a.sin(), -a.cos())
    };
    (0..rows)
        .map(|i| {
            let b = fraction(i, rows - 1);
            let (from, to) = (rim(down * (1.0 - b)), seam(b));
            (0..cols)
                .map(|j| {
                    let a = fraction(j, cols - 1);
                    // The middle plane exactly at the seam, so the mirrored half meets it.
                    let d = from.lerp(to, a).try_normalize().unwrap_or(to);
                    if j == cols - 1 { DVec3::new(0.0, to.y, to.z) } else { d }
                })
                .collect()
        })
        .collect()
}

/// The raised hood over `head` and the `hair` on it.
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)] // one shell, built in order; skin shares
fn up(
    mesh: &mut MeshBuilder,
    frame: &Frame,
    hood: &Hood,
    tones: &Tones,
    head: &dyn HeadSurface,
    hair: Option<&HairReach>,
) -> Result<(), GarmentError> {
    let (head_rows, neck_rows, cols) = UP_GRID;
    let centre = head.centre().as_dvec3();
    let dirs = directions();
    // The stand-off at each point: the clearance and the hair, the largest over a window, then smoothed, never below
    // what the point itself needs.
    #[allow(clippy::cast_possible_truncation)] // a direction for an f32 query
    let raw: Vec<Vec<f64>> = dirs
        .iter()
        .map(|row| {
            row.iter()
                .map(|d| {
                    let v = d.as_vec3();
                    // Over the head's cap the hood clears all the hair; below it only the hair's first few
                    // centimetres, so long locks hang out of the hood instead of spreading it into wings.
                    let cap = lerp(HAIR_CAP.0, HAIR_CAP.1, sstep(-0.4, 0.2, d.y));
                    hood.clearance + f64::from(hair.map_or(0.0, |h| h.above(v))).min(cap)
                })
                .collect()
        })
        .collect();
    let stand = smooth(&raw);
    let fullness = FULLNESS * (hood.size - 1.0).max(0.0);
    let column = &frame.column;
    let least = frame.cut.ease + 2.0 * frame.cut.thickness + frame.cut.lip + 0.001;
    let clear_neck = |p: DVec3| -> Result<DVec3, GarmentError> {
        if p.y < column.neck_top() { column.lift(p, least) } else { Ok(p) }
    };
    let mut points = Vec::with_capacity((head_rows + neck_rows) * cols);
    let mut skins: Vec<Skinning> = Vec::with_capacity(points.capacity());
    // The collar: from the front, where the hood meets it, round to the centre back.
    let front = front_limit(frame, UP_FRONT, 0.15);
    let collar: Vec<(DVec3, Skinning)> = (0..cols)
        .map(|j| {
            let phi = lerp(front, PI, fraction(j, cols - 1));
            let (p, _, skin) = frame.on(frame.neck(phi) - 0.006, phi, frame.cut.lip + 0.001)?;
            Ok((if j == cols - 1 { DVec3::new(0.0, p.y, p.z) } else { p }, skin))
        })
        .collect::<Result<_, GarmentError>>()?;
    let on_head = |i: usize, j: usize| -> DVec3 {
        let d = dirs[i][j];
        #[allow(clippy::cast_possible_truncation)] // the head is f32
        let r = f64::from(head.radius(d.as_vec3()));
        let back = (-d.z).max(0.0) * fullness;
        centre + d * (r + stand[i][j] + back)
    };
    for k in 0..neck_rows {
        let s = sstep(0.0, 1.0, fraction(k, neck_rows));
        for (j, (c, skin)) in collar.iter().enumerate() {
            let p = clear_neck(c.lerp(on_head(0, j), s))?;
            points.push(if j == cols - 1 { DVec3::new(0.0, p.y, p.z) } else { p });
            skins.push(mix_skin(skin, &[(JointRef::NECK, 1.0)], s as f32 * 0.5));
        }
    }
    for i in 0..head_rows {
        let b = fraction(i, head_rows - 1);
        for j in 0..cols {
            let p = clear_neck(on_head(i, j))?;
            points.push(if j == cols - 1 { DVec3::new(0.0, p.y, p.z) } else { p });
            #[allow(clippy::cast_possible_truncation)] // a share
            skins.push(mix_skin(&[(JointRef::NECK, 1.0)], &[(JointRef::HEAD, 1.0)], (0.5 + 0.5 * b) as f32));
        }
    }
    let rows = head_rows + neck_rows;
    // The top row is the rims' meeting point: one point (a pole the grid handles).
    let top = points[(rows - 1) * cols + cols - 1];
    for p in &mut points[(rows - 1) * cols..] {
        *p = top;
    }
    let lining = Grid::new(rows, cols, points)?;
    let inside = |i: usize, j: usize| {
        let p = lining.at(i, j);
        if p.y > centre.y - 0.02 { centre } else { DVec3::new(0.0, p.y, centre.z) }
    };
    let options = GridOptions { inside: Some(&inside), ..GridOptions::default() };
    let offset = grid_offset(&lining, frame.cut.thickness, &options)?;
    // The seam on the middle plane and the top row one point, as the lining's.
    let pole = offset.at(rows - 1, cols - 1);
    let pole = DVec3::new(0.0, pole.y, pole.z);
    let outer = Grid::from_fn(rows, cols, |i, j| {
        let p = offset.at(i, j);
        if i == rows - 1 {
            pole
        } else if j == cols - 1 {
            DVec3::new(0.0, p.y, p.z)
        } else {
            p
        }
    })?;
    let mut half = MeshBuilder::with_colours();
    let skin = |_: DVec3, i: usize, j: usize| skins[i * cols + j].clone();
    for (grid, invert, tone) in [(&outer, false, tones.outer), (&lining, true, tones.lining)] {
        let colour = |_: usize, _: usize, _: DVec3| [tone[0], tone[1], tone[2], 1.0];
        let options = GridOptions {
            invert,
            inside: Some(&inside),
            colour: Some(&colour),
            skin: Skin::Per(&skin),
            ..GridOptions::default()
        };
        half.add_grid(grid, &options)?;
    }
    // The border's roll, half way through the cloth: the bottom from the seam to the rim, up the rim to the top.
    let mid = |i: usize, j: usize| (outer.at(i, j).midpoint(lining.at(i, j)), skins[i * cols + j].clone());
    let mut path: Vec<(DVec3, Skinning)> = (0..cols).rev().map(|j| mid(0, j)).collect();
    path.extend((1..rows).map(|i| mid(i, 0)));
    let mirror = |(p, s): &(DVec3, Skinning)| {
        (DVec3::new(-p.x, p.y, p.z), s.iter().map(|(j, w)| (crate::kit::grid::mirror_joint(j), *w)).collect())
    };
    let mut ring = path.clone();
    ring.extend(path.iter().rev().skip(1).map(mirror));
    let (points, skins): (Vec<DVec3>, Vec<Skinning>) = ring.into_iter().unzip();
    mesh.merge(&half)?;
    mesh.merge(&half.mirrored())?;
    let radius = frame.cut.thickness / 2.0 + ROLL_LIP;
    let axis = |p: DVec3| {
        if p.y > centre.y - 0.02 { centre } else { DVec3::new(0.0, p.y, centre.z) }
    };
    loop_tube(mesh, &Tube { points: &points, skins: &skins, radius, colour: tones.edge }, &axis)
}

/// `raw` smoothed: the largest over a window, then the mean over a window, never below `raw`.
#[allow(clippy::cast_precision_loss)] // a small window
#[allow(clippy::needless_range_loop)] // windows round each point
fn smooth(raw: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let (rows, cols) = (raw.len(), raw.first().map_or(0, Vec::len));
    let window = |grid: &[Vec<f64>], i: usize, j: usize, r: usize| {
        let mut out = Vec::new();
        for a in i.saturating_sub(r)..=(i + r).min(rows - 1) {
            for b in j.saturating_sub(r)..=(j + r).min(cols - 1) {
                out.push(grid[a][b]);
            }
        }
        out
    };
    let most: Vec<Vec<f64>> = (0..rows)
        .map(|i| (0..cols).map(|j| window(raw, i, j, SMOOTH.0).into_iter().fold(0.0, f64::max)).collect())
        .collect();
    (0..rows)
        .map(|i| {
            (0..cols)
                .map(|j| {
                    let w = window(&most, i, j, SMOOTH.1);
                    (w.iter().sum::<f64>() / w.len() as f64).max(raw[i][j])
                })
                .collect()
        })
        .collect()
}
