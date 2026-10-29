//! A coat's shell: the skirt and the bodice as one sheet of cloth and its lining, with the rolled edges.
//!
//! The vertices go into one mesh in this order: the skirt's outside (row 0 the waist seam: the grid
//! [`drape`](crate::outfit::drape) simulates), the bodice's outside (its row 0 the same seam), the skirt's lining, the
//! bodice's lining, then the rolled edges. The bodice rows run from the seam up to the neckline of each column, the
//! skirt rows down to the hem of each column, spaced evenly along the skirt's outline so a shoulder line or a flare is
//! cut as finely as a straight fall.

use std::f64::consts::PI;
use std::ops::Range;
use std::sync::Arc;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, BodyRegion, JointRef};
use omuzaki_core::glam::{DVec2, DVec3};

use super::cloud::Cloud;
use super::edge::{Tube, loop_tube};
use super::hang::{Arms, HangSpec, Table};
use super::reach::HangReach;
use super::{CoatCut, Front, MAX_LENGTH, MIN_LENGTH, NeckShape};
use crate::kit::curve::{TAU, fraction, lerp, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::outfit::common::GarmentError;
use crate::outfit::loft::{RegionWeights, Ring};
use crate::outfit::shirt::frame::Column;
use crate::outfit::shirt::tile_uvs;
use crate::outfit::skirt::ThighPull;
use crate::outfit::skirt::weights::SkirtSkin;
use crate::outfit::trousers::rows::mix_skin;

/// Segments round the body.
pub const SEGMENTS: usize = 96;
/// The skirt's rows are about this far apart along its outline, metres.
const SKIRT_ROW: f64 = 0.012;
/// The bodice's rows are about this far apart, metres.
const BODICE_ROW: f64 = 0.009;
/// The fewest and most rows of either.
const ROWS: (usize, usize) = (6, 44);
/// How far the waist seam's ring stands clear of the arms beyond the cloth, metres.
const SEAM_MARGIN: f64 = 0.004;
/// How far the waist seam comes down at a time to clear the arms, metres.
const SEAM_STEP: f64 = 0.004;
/// The highest a hem goes, as a fraction of the torso.
const HIGHEST_HEM: f64 = 0.3;
/// The least length of skirt below the waist seam, metres.
const MIN_SKIRT: f64 = 0.02;
/// How far the left panel stands over the right where the front crosses, beyond the cloth's thickness, metres.
const OVERLAP_GAP: f64 = 0.0015;
/// Over how many radians past the right panel's edge the left panel comes back down onto the body.
const OVERLAP_RAMP: f64 = 0.4;
/// The armhole's floor is flat within this many radians of the side and climbs back over the next (the vest's).
const ARMHOLE: (f64, f64) = (0.40, 0.20);
/// The most the neckline may climb from one column to the next, metres.
const TOP_STEP: f64 = 0.003;
/// Samples of the neckline across one even column, to find its steepest climb.
const TOP_SAMPLES: usize = 8;
/// The most columns one even column is cut into.
const MAX_SPLIT: usize = 12;
/// With sleeves, the shoulders the bodice covers: fully within this angle of the side, radians, none beyond the second,
/// and how far over the torso's top it rises there, as a fraction of the neck.
const SHOULDER: (f64, f64, f64) = (0.5, 1.0, 0.5);
/// The straps beside an armhole: the highest their top may be, as a fraction of the torso below its top, and over
/// which angles from the armhole's edge they keep to it (all of it, then easing back to the neckline).
const STRAP: (f64, f64, f64) = (0.08, 0.15, 0.35);
/// A sleeveless high or halter neck's cut-in, as angles from the side, radians: it leaves the armhole's floor where the
/// torso turns from its side to its front, and reaches the collar line this far from the middle of the front. The
/// torso's rings are squarish, so most of the angles round to the front lie on its side, at one width.
const CUT_IN: (f64, f64) = (0.8, PI / 2.0 - 0.2);
/// How far below the neck's top a collar line stays, metres.
const NECK_CLEAR: f64 = 0.004;
/// The neckline dips at the back over this many radians either side of the centre back.
const BACK_DIP: f64 = 0.9;
/// Where the skirt's skin hands over from the torso's bones to the hips and thighs, as fractions of the torso: the
/// torso's above the first, the thighs' below the second.
const HIP_HANDOVER: (f64, f64) = (0.45, 0.0);
/// The skirt's thighs: how far their pull reaches and where it hands over (the skirt's defaults).
const THIGH: (f64, [f64; 2]) = (crate::outfit::skirt::DEFAULT_THIGH_REACH, crate::outfit::skirt::DEFAULT_HIP_BLEND);
/// The outside's shading down the skirt: lighter at the waist.
const SHADE: (f32, f32) = (0.9, 1.0);
/// How much darker the folds' valleys are.
const VALLEY: f32 = 0.12;
/// The lining's shading.
const LINING_SHADE: f32 = 0.85;

/// The skin of one vertex.
pub type Skinning = Vec<(JointRef, f32)>;

/// One sheet of the shell: a grid of rows by the shell's columns.
#[derive(Clone, Debug, Default)]
pub struct Sheet {
    /// Rows.
    pub rows: usize,
    /// The outside's points.
    pub outer: Vec<DVec3>,
    /// The lining's points.
    pub lining: Vec<DVec3>,
    /// The outward normals of the rings there.
    pub normals: Vec<DVec3>,
    /// Each vertex's height and angle.
    pub at: Vec<(f64, f64)>,
    /// Each vertex's skin.
    pub skins: Vec<Skinning>,
}

/// Colours of the shell's vertices (linear RGB): the outside, the lining, the rolled edge.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Tones {
    /// The outside.
    pub outer: [f32; 3],
    /// The lining.
    pub lining: [f32; 3],
    /// The rolled edges.
    pub edge: [f32; 3],
}

/// Where a coat's surface is: the bodice's rings above the seam, the skirt's below, and how the front opens.
#[derive(Clone, Debug)]
pub struct Frame {
    /// The cut.
    pub cut: CoatCut,
    /// The torso and neck.
    pub column: Column,
    /// The skirt.
    pub table: Table,
    weights: RegionWeights,
    thighs: SkirtSkin,
    /// The hem's height at the front, metres.
    pub hem: f64,
    /// The collar line's height for an open front.
    pub top: f64,
}

impl Frame {
    /// The frame of `cut` over `fit`, clearing the garments under it (`under`).
    ///
    /// # Errors
    /// [`GarmentError`] for a body the coat can't be cut over, or a hem that isn't below the waist seam.
    pub fn new(fit: Arc<dyn BodyFit>, cut: &CoatCut, under: Option<HangReach>) -> Result<Self, GarmentError> {
        let column = Column::new(fit, cut.exponent)?;
        let fit = column.fit().clone();
        let weights = RegionWeights::of(fit.as_ref(), BodyRegion::Torso)?;
        let thighs = SkirtSkin::new(fit.as_ref(), 1.0, THIGH.0, THIGH.1, ThighPull::Whole)?;
        let joint = |bone| {
            fit.joint(&JointRef::Humanoid(bone))
                .map(|j| f64::from(j.y))
                .ok_or_else(|| GarmentError::invalid("body", format!("the fit has no {bone:?} joint")))
        };
        let (hip, ankle) = (joint(HumanoidBone::LeftUpperLeg)?, joint(HumanoidBone::LeftFoot)?);
        let length = cut.length.clamp(MIN_LENGTH, MAX_LENGTH);
        // A cropped hem stays on the lower torso, below the arms, on every body.
        let hem = (hip - length * (hip - ankle)).min(column.bottom() + HIGHEST_HEM * column.length());
        // On a body whose hips sit high in the torso the seam rises to keep a skirt's length above a short hem.
        let seam = cut.waist.clamp(0.0, 1.0 + cut.neckline.rise.max(0.0));
        let waist = (column.bottom() + seam * column.length()).max(hem + 2.0 * MIN_SKIRT);
        if hem > waist - MIN_SKIRT {
            return Err(GarmentError::invalid(
                "length",
                format!("the hem at {hem:.3} m is not {MIN_SKIRT} m below the waist seam at {waist:.3} m"),
            ));
        }
        let cloud = Cloud::of(fit.as_ref(), under)?;
        let waist = clear_seam(&column, &cloud, cut, waist, hem + 2.0 * MIN_SKIRT)?;
        let ring = column.ring(waist, cut.ease)?;
        let spec = HangSpec {
            waist: ring,
            bottom: hem - cut.back_drop.max(0.0) - 0.01,
            ease: cut.ease,
            outside: cut.thickness + cut.folds.1 + cut.lip,
            fall: cut.fall,
            flare: cut.flare,
            arms: cut.arms,
        };
        let table = Table::new(&cloud, &spec)?;
        // A collar line no higher than the neck: the rings above it are the neck's last, and rows there would meet.
        let top = (column.top() + cut.neckline.rise * column.length()).min(column.neck_top() - NECK_CLEAR);
        Ok(Self { cut: cut.clone(), column, table, weights, thighs, hem, top })
    }

    /// The waist seam's height.
    pub fn waist(&self) -> f64 {
        self.table.top()
    }

    /// The height at fraction `f` of the torso.
    pub fn torso(&self, f: f64) -> f64 {
        self.column.bottom() + f * self.column.length()
    }

    /// The angle of the left panel's front edge at height `y` (the right panel's is `TAU` minus it); `-PI` for a
    /// closed front.
    pub fn edge(&self, y: f64) -> f64 {
        match self.cut.front {
            Front::Closed => -PI,
            Front::Open { gap, point, neck } => {
                let from = self.torso(point).min(self.top - 0.005);
                if y <= from {
                    gap
                } else {
                    let s = ((y - from) / (self.top - from)).clamp(0.0, 1.0);
                    gap + (neck - gap) * s.powf(1.3)
                }
            }
        }
    }

    /// The angle of column `u` (0 to 1 round the shell) at height `y`.
    pub fn phi(&self, u: f64, y: f64) -> f64 {
        match self.cut.front {
            Front::Closed => -PI + TAU * u,
            Front::Open { .. } => {
                let e = self.edge(y);
                e + (TAU - 2.0 * e) * u
            }
        }
    }

    /// Whether the front is closed.
    pub fn closed(&self) -> bool {
        self.cut.front == Front::Closed
    }

    /// The neckline's height at angle `phi` (a closed front's; an open front's collar line is level).
    pub fn neck(&self, phi: f64) -> f64 {
        let n = &self.cut.neckline;
        let len = self.column.length();
        let a = wrap_angle(phi).abs();
        let back = n.back * (1.0 - ((PI - a) / BACK_DIP).min(1.0)).powi(2);
        let front = if self.closed() { n.depth * n.shape.dip(a / n.width) } else { 0.0 };
        let neck = self.top - len * (back + front);
        // The armhole's floor is measured down from the torso's top, wherever the neckline stands.
        let floor = (self.column.top() - len * self.cut.armhole).min(neck);
        let from_side = (a - PI / 2.0).abs();
        if self.cut.armhole <= 0.0 {
            if self.cut.arms == Arms::Enclose {
                return neck;
            }
            // With sleeves the bodice covers the shoulders: its sides rise over the torso's top whatever the neckline,
            // so each armhole is a hole in the cloth, clear of the neckline.
            let side = 1.0 - sstep(SHOULDER.0, SHOULDER.1, from_side);
            let shoulder = self.column.top() + SHOULDER.2 * (self.column.neck_top() - self.column.top());
            return neck + side * (shoulder - neck).max(0.0);
        }
        if n.shape.stands() && self.closed() {
            // Cut in: from the armhole's floor at the side up to the collar line near the middle of the front (and, for
            // a high neck, of the back), so the shoulders are bare; a halter's back stays at the floor. The collar band
            // round the neck joins them.
            let floor = (self.column.top() - len * self.cut.armhole).min(self.top);
            if n.shape == NeckShape::Halter && a > PI / 2.0 {
                return floor;
            }
            let rise = sstep(CUT_IN.0, CUT_IN.1, from_side);
            return lerp(floor, self.top, rise);
        }
        let cut = 1.0 - sstep(ARMHOLE.0, ARMHOLE.0 + ARMHOLE.1, from_side);
        // The straps beside an armhole stay below the arms' roots, however high the neckline stands.
        let strap = 1.0 - sstep(ARMHOLE.0 + ARMHOLE.1 + STRAP.1, ARMHOLE.0 + ARMHOLE.1 + STRAP.2, from_side);
        let cap = self.column.top() - len * STRAP.0;
        lerp(neck - strap * (neck - cap).max(0.0), floor, cut)
    }

    /// Where a sleeveless coat's straps meet its front, as the angle from the centre front of the middle of the flat
    /// top beside each armhole, radians; its back mirrors it about the side.
    pub fn strap_angle(&self) -> f64 {
        PI / 2.0 - (ARMHOLE.0 + ARMHOLE.1 + STRAP.1 / 2.0)
    }

    /// The hem's height at angle `phi`.
    pub fn hem_at(&self, phi: f64) -> f64 {
        let back = (1.0 - wrap_angle(phi).cos()) / 2.0;
        self.hem - self.cut.back_drop * back.powf(1.5)
    }

    /// The lining's ring at height `y`: the bodice's above the waist seam, the skirt's below.
    ///
    /// # Errors
    /// As [`Column::ring`].
    pub fn ring(&self, y: f64) -> Result<Ring, GarmentError> {
        if y >= self.waist() { self.column.ring(y, self.cut.ease) } else { Ok(self.table.ring(y)) }
    }

    /// How far the cloth stands off the lining's ring at `(y, phi)`: the left panel lifted over the right where the
    /// front crosses, and the folds below the waist.
    pub fn lift(&self, y: f64, phi: f64) -> f64 {
        let mut lift = 0.0;
        if let Front::Open { .. } = self.cut.front {
            let e = self.edge(y);
            if e < 0.0 {
                let over = (-e / 0.05).min(1.0) * (self.cut.thickness + OVERLAP_GAP);
                lift += over * (1.0 - sstep(-e, -e + OVERLAP_RAMP, phi));
            }
        }
        let (count, depth) = self.cut.folds;
        if count > 0 && depth > 0.0 && y < self.waist() {
            #[allow(clippy::cast_precision_loss)] // a fold count is tiny
            let wave = f64::midpoint(1.0, (count as f64 * (phi - PI)).cos());
            lift += depth * self.fold_amount(y) * wave;
        }
        lift
    }

    /// How much of the folds' depth there is at `y`: none at the waist seam, all at the hem.
    fn fold_amount(&self, y: f64) -> f64 {
        sstep(self.waist(), self.hem, y)
    }

    /// The lining's point, the outside's point and the outward normal at `(y, phi)`.
    ///
    /// # Errors
    /// As [`Frame::ring`].
    pub fn at(&self, y: f64, phi: f64) -> Result<(DVec3, DVec3, DVec3), GarmentError> {
        let ring = self.ring(y)?;
        let normal = ring.normal(phi);
        let lining = ring.point(phi) + normal * self.lift(y, phi);
        Ok((lining, lining + normal * self.cut.thickness, normal))
    }

    /// A point of the outside at `(y, phi)` lifted `extra` further out, its normal and skin: where trims sit.
    ///
    /// # Errors
    /// As [`Frame::ring`].
    pub fn on(&self, y: f64, phi: f64, extra: f64) -> Result<(DVec3, DVec3, Skinning), GarmentError> {
        let (_, outer, normal) = self.at(y, phi)?;
        let p = outer + normal * extra;
        Ok((p, normal, self.skin(p)))
    }

    /// The centre of the ring at height `y`: what the shell's normals point away from.
    pub fn centre(&self, y: f64) -> DVec3 {
        self.ring(y).map_or(DVec3::new(0.0, y, 0.0), |r| r.centre)
    }

    /// The skin at `p`: the neck above the torso, the torso's bones down to the hips, then the hips and thighs.
    pub fn skin(&self, p: DVec3) -> Skinning {
        let column = &self.column;
        if p.y > column.top() {
            return vec![(JointRef::NECK, 1.0)];
        }
        let torso = self.weights.at(((p.y - column.bottom()) / column.length()).clamp(0.0, 1.0));
        #[allow(clippy::cast_possible_truncation)] // a share in 0..=1
        let w = sstep(self.torso(HIP_HANDOVER.0), self.torso(HIP_HANDOVER.1), p.y) as f32;
        if w <= 0.0 { torso } else { mix_skin(&torso, &self.thighs.at(p), w) }
    }
}

/// The seam at `waist` or the highest below it, down to `lowest`, whose ring stands clear of the arms by the cloth and a
/// margin: the skirt hangs from the seam's ring and isn't cut where the arms pass, so on a body whose arms hang close
/// beside the chest the seam comes down below them. A cape's seam encloses the arms and stays.
fn clear_seam(column: &Column, cloud: &Cloud, cut: &CoatCut, waist: f64, lowest: f64) -> Result<f64, GarmentError> {
    if cut.arms == Arms::Enclose {
        return Ok(waist);
    }
    let reach = cut.thickness + cut.lip + SEAM_MARGIN;
    let mut y = waist;
    while y > lowest {
        let ring = column.ring(y, cut.ease)?;
        let clear = cloud.arms_at(y, SEAM_STEP).all(|a| {
            let d = a.p - ring.centre;
            ring.along_ray(DVec2::new(d.dot(ring.across), d.dot(ring.through))).1 > reach
        });
        if clear {
            return Ok(y);
        }
        y -= SEAM_STEP;
    }
    Ok(lowest.max(y))
}

/// `phi` in `-PI..=PI`.
fn wrap_angle(phi: f64) -> f64 {
    let a = phi.rem_euclid(TAU);
    if a > PI { a - TAU } else { a }
}

/// The shell: its mesh and what the kinds and the weld need to know about it.
#[derive(Debug)]
pub struct Shell {
    /// The skirt.
    pub skirt: Sheet,
    /// The bodice.
    pub bodice: Sheet,
    /// Columns of both sheets (a closed shell's last repeats its first).
    pub cols: usize,
    /// The mesh.
    pub mesh: MeshBuilder,
    /// The bodice's outside and lining vertices in the mesh: what the sleeves' weld trims.
    pub bodice_vertices: [Range<usize>; 2],
}

impl Shell {
    /// The skirt's outside: the drape grid, `(rows, cols)` from vertex 0.
    pub fn drape_grid(&self) -> (usize, usize) {
        (self.skirt.rows, self.cols)
    }
}

/// The columns round the shell, as fractions `u`: [`SEGMENTS`] even ones, each cut in as many as keep the neckline's
/// height within [`TOP_STEP`] from one column to the next (an armhole's climb, a round neck's sides, a shoulder). Each
/// column's rows run evenly from the waist to its top, so where the top climbs fast neighbouring columns' rows are far
/// apart in height, and a quad between them is a long sheared one whose diagonal chords across the chest's curve, into
/// the body (the vest's columns do the same).
fn columns(frame: &Frame) -> Vec<f64> {
    let base: Vec<f64> = (0..=SEGMENTS).map(|j| fraction(j, SEGMENTS)).collect();
    let at = if frame.closed() { frame.waist() } else { frame.top };
    let top = |u: f64| frame.neck(frame.phi(u, at));
    let mut out = vec![base[0]];
    for pair in base.windows(2) {
        // The steepest climb inside the interval, sampled finely.
        let climb = (0..TOP_SAMPLES)
            .map(|k| {
                let (a, b) = (
                    lerp(pair[0], pair[1], fraction(k, TOP_SAMPLES)),
                    lerp(pair[0], pair[1], fraction(k + 1, TOP_SAMPLES)),
                );
                (top(b) - top(a)).abs()
            })
            .fold(0.0, f64::max)
            * fraction(TOP_SAMPLES, 1);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small count
        let split = ((climb / TOP_STEP).ceil() as usize).clamp(1, MAX_SPLIT);
        out.extend((1..=split).map(|k| lerp(pair[0], pair[1], fraction(k, split))));
    }
    out
}

/// The rows of each column of the skirt: heights from the waist seam to the hem, evenly along the outline.
fn skirt_heights(frame: &Frame, us: &[f64]) -> (usize, Vec<Vec<f64>>) {
    let lowest = us.iter().map(|&u| frame.hem_at(frame.phi(u, frame.hem))).fold(f64::INFINITY, f64::min);
    let longest = frame.table.length_at(lowest);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small row count
    let rows = ((longest / SKIRT_ROW).round() as usize).clamp(ROWS.0, ROWS.1);
    let heights = us
        .iter()
        .map(|&u| {
            let l = frame.table.length_at(frame.hem_at(frame.phi(u, frame.hem)));
            (0..=rows).map(|i| frame.table.height_at(l * fraction(i, rows))).collect()
        })
        .collect();
    (rows, heights)
}

/// The rows of each column of the bodice: heights from the waist seam up to the neckline.
fn bodice_heights(frame: &Frame, us: &[f64]) -> Result<(usize, Vec<Vec<f64>>), GarmentError> {
    let waist = frame.waist();
    let tops: Vec<f64> = us
        .iter()
        .map(|&u| if frame.closed() { frame.neck(frame.phi(u, waist)) } else { frame.neck(frame.phi(u, frame.top)) })
        .collect();
    if let Some(low) = tops.iter().copied().find(|&t| t < waist + 0.004) {
        return Err(GarmentError::invalid(
            "neckline",
            format!("the neckline or an armhole comes down to {low:.3} m, at the waist seam ({waist:.3} m)"),
        ));
    }
    let tallest = tops.iter().copied().fold(waist, f64::max) - waist;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small row count
    let rows = ((tallest / BODICE_ROW).round() as usize).clamp(ROWS.0, ROWS.1);
    Ok((rows, tops.iter().map(|&top| (0..=rows).map(|i| lerp(waist, top, fraction(i, rows))).collect()).collect()))
}

/// The sheet over `heights` (one list per column).
#[allow(clippy::needless_range_loop)] // the rows are walked across the columns' lists
fn sheet(frame: &Frame, us: &[f64], rows: usize, heights: &[Vec<f64>]) -> Result<Sheet, GarmentError> {
    let cols = us.len();
    let mut out = Sheet { rows: rows + 1, ..Sheet::default() };
    for i in 0..=rows {
        for (j, &u) in us.iter().enumerate() {
            let y = heights[j][i];
            let phi = frame.phi(u, y);
            let (lining, outer, normal) = if frame.closed() && j == cols - 1 {
                // The seam: the last column is the first again, exactly.
                let k = out.outer.len() + 1 - cols;
                (out.lining[k], out.outer[k], out.normals[k])
            } else {
                frame.at(y, phi)?
            };
            out.outer.push(outer);
            out.lining.push(lining);
            out.normals.push(normal);
            out.at.push((y, phi));
            out.skins.push(frame.skin(outer));
        }
    }
    Ok(out)
}

/// Builds the shell of `frame` in `tones`, its fabric tiled every `tile` metres.
///
/// # Errors
/// [`GarmentError`] for a neckline that reaches the waist seam, or a ring the fit can't give.
pub fn build(frame: &Frame, tones: &Tones, tile: Option<f64>) -> Result<Shell, GarmentError> {
    let us = columns(frame);
    let cols = us.len();
    let (skirt_rows, skirt_heights) = skirt_heights(frame, &us);
    let (bodice_rows, bodice_heights) = bodice_heights(frame, &us)?;
    let skirt = sheet(frame, &us, skirt_rows, &skirt_heights)?;
    let bodice = sheet(frame, &us, bodice_rows, &bodice_heights)?;
    let mut mesh = MeshBuilder::with_colours();
    let mut ranges = [0..0, 0..0];
    for (lining, sheet, slot) in
        [(false, &skirt, None), (false, &bodice, Some(0)), (true, &skirt, None), (true, &bodice, Some(1))]
    {
        let start = mesh.vertex_count();
        add_sheet(&mut mesh, frame, sheet, cols, lining, tones, tile)?;
        if let Some(slot) = slot {
            ranges[slot] = start..mesh.vertex_count();
        }
    }
    add_edges(&mut mesh, frame, &skirt, &bodice, cols, tones)?;
    let [outer, lining] = ranges;
    Ok(Shell { skirt, bodice, cols, mesh, bodice_vertices: [outer, lining] })
}

/// Adds the outside or the lining of `sheet`.
fn add_sheet(
    mesh: &mut MeshBuilder,
    frame: &Frame,
    sheet: &Sheet,
    cols: usize,
    lining: bool,
    tones: &Tones,
    tile: Option<f64>,
) -> Result<(), GarmentError> {
    let points = if lining { sheet.lining.clone() } else { sheet.outer.clone() };
    let grid = Grid::new(sheet.rows, cols, points)?;
    let uvs = tile_uvs(&grid, tile, frame.closed());
    let uv = |i: usize, j: usize| uvs.as_ref().map_or(DVec2::ZERO, |u| u[i * cols + j]);
    let inside = |i: usize, j: usize| frame.centre(sheet.at[i * cols + j].0);
    let skin = |_: DVec3, i: usize, j: usize| sheet.skins[i * cols + j].clone();
    let (count, depth) = frame.cut.folds;
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // shades in 0..=1
    let colour = |i: usize, j: usize, _: DVec3| {
        let (y, phi) = sheet.at[i * cols + j];
        let (tone, k) = if lining {
            (tones.lining, LINING_SHADE)
        } else {
            let light = lerp(f64::from(SHADE.0), f64::from(SHADE.1), sstep(frame.hem, frame.waist(), y)) as f32;
            let valley = if count > 0 && depth > 0.0 && y < frame.waist() {
                let wave = f64::midpoint(1.0, (count as f64 * (phi - PI)).cos());
                (VALLEY * (1.0 - wave as f32)) * frame.fold_amount(y) as f32
            } else {
                0.0
            };
            (tones.outer, light * (1.0 - valley))
        };
        [tone[0] * k, tone[1] * k, tone[2] * k, 1.0]
    };
    let options = GridOptions {
        wrap_j: frame.closed(),
        invert: lining,
        inside: Some(&inside),
        uv: uvs.is_some().then_some(&uv as &dyn Fn(usize, usize) -> DVec2),
        colour: Some(&colour),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    mesh.add_grid(&grid, &options)?;
    Ok(())
}

/// The rolled edges: round the whole border of an open shell, or round the hem and the neckline of a closed one. Each
/// covers the cloth's cut edge from the lining out to the lip beyond the outside.
fn add_edges(
    mesh: &mut MeshBuilder,
    frame: &Frame,
    skirt: &Sheet,
    bodice: &Sheet,
    cols: usize,
    tones: &Tones,
) -> Result<(), GarmentError> {
    // Half way through the cloth and out by the lip, so the roll's inside lies on the lining, never nearer the body.
    let lip = frame.cut.lip;
    let mid = |sheet: &Sheet, i: usize, j: usize| {
        let k = i * cols + j;
        (sheet.outer[k].midpoint(sheet.lining[k]) + sheet.normals[k] * lip, sheet.skins[k].clone())
    };
    let mut loops: Vec<Vec<(DVec3, Skinning)>> = Vec::new();
    let (sl, bl) = (skirt.rows - 1, bodice.rows - 1);
    if frame.closed() {
        loops.push((0..cols - 1).map(|j| mid(skirt, sl, j)).collect());
        loops.push((0..cols - 1).map(|j| mid(bodice, bl, j)).collect());
    } else {
        let last = cols - 1;
        let mut path = Vec::new();
        path.extend((0..=sl).rev().map(|i| mid(skirt, i, 0)));
        path.extend((1..=bl).map(|i| mid(bodice, i, 0)));
        path.extend((1..last).map(|j| mid(bodice, bl, j)));
        path.extend((1..=bl).rev().map(|i| mid(bodice, i, last)));
        path.extend((0..=sl).map(|i| mid(skirt, i, last)));
        path.extend((1..last).rev().map(|j| mid(skirt, sl, j)));
        loops.push(path);
    }
    // A tenth of a millimetre short of the lining, so the two never share a point.
    let radius = frame.cut.thickness / 2.0 + frame.cut.lip - 1e-4;
    for path in loops {
        let (points, skins): (Vec<DVec3>, Vec<Skinning>) = path.into_iter().unzip();
        let axis = |p: DVec3| frame.centre(p.y.min(frame.top));
        loop_tube(mesh, &Tube { points: &points, skins: &skins, radius, colour: tones.edge }, &axis)?;
    }
    Ok(())
}
