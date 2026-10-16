//! The body's published fit: the template layout's sections, widened wherever the body's own mesh stands outside them.
//!
//! The mesher lofts each region from the layout's sections, but not only from them. [`junction`](super::junction)
//! bridges the hip and the ankle (rings widened past the layout's) and grows each shoulder a root that leaves the arm
//! before its start; every region closes past its ends with a cap (the thighs' domes rise into the pelvis, the pelvis's
//! own floor sits between the thighs); corners are rounded. Those parts stand outside the layout's sections by up to
//! seven centimetres, and a garment built over the layout alone shows the body through it: the hips through a hem, the
//! shoulders through a yoke, the ankles through a boot.
//!
//! So the fit the body publishes is widened to its mesh. Each point of a region's mesh must lie in the section of the
//! region that holds it:
//! - within its own region's span, its own region;
//! - past an end, the region that end closes into: a shoulder's top (an arm's root above its first ring) and a thigh's
//!   dome the torso, a shin's ankle the foot, the pelvis's floor the thigh it lies least far outside of, the torso's
//!   top the neck (the root's underside runs into the armpit, between the arm and the torso, where a garment needs an
//!   armhole);
//! - and every point of a leg the torso's sections reach, the torso as well: the hips' outline is the pelvis's and the
//!   thighs' together, so a garment round the torso there clears the thighs.
//!
//! Where a vertex lies outside a section that must hold it, that section grows on the vertex's side until it does, and
//! so does every row any edge crosses outside it, at the point the edge crosses it (where the layout's profile curves
//! in, at a waist, the mesh's straight edges between two rings stand out of it even though their ends don't). Each row of
//! a region's table keeps how far its ellipse reaches past the layout's to either side and to the front and back; a
//! section between two rows takes the reaches between theirs, and an ellipse that reaches further one way than the
//! other moves its centre that way. A row keeps every point it was widened for, and the layout's own ellipse, inside
//! its widened ellipse (moving the centre pulls the far side's flanks in, so they are held again until all are). It
//! reaches no further across the body than it must, and takes what it must hold in front and behind: an ellipse round
//! the hips (the pelvis and both thighs, square-ish) then stops at the body's sides, where the arms hang.
//!
//! Then each reach is eased along its region, as a smoothstep no steeper than 45°, so the fit has no step for a
//! garment's facets to cut through or stand out of.
//!
//! A wrist's cap (the hand covers it) and a foot's heel and toe close into no region, and are left as they are. The
//! joints and the bind pose are the template's, unchanged.

use std::collections::BTreeMap;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_char_api::{BodyFit, BodyRegion, JointRef, Section};
use omuzaki_core::glam::{DVec2, DVec3, Vec3};
use omuzaki_core::mesh::MeshData;

use crate::kit::curve::{TAU, fraction};
use crate::template::layout::{BodyLayout, RegionSection, section_basis};
use crate::template::skeleton::TemplateFit;

/// Rows in each region's table of reaches: the fit is widened at this resolution along a region (about 4 mm a row on
/// a standard torso).
pub const ROWS: usize = 128;

/// How far across its reach a point may lie and still be held by growing the front-to-back reach alone.
const ACROSS_SHARE: f64 = 0.98;

/// Growth steps that push a section's reach out to a point before falling back to scaling the whole ellipse.
const GROWTH_STEPS: usize = 48;

/// How far inside a widened section a point it was widened for lies, metres: room for the `f32` the fit is published in
/// and for the frames of two rows taken between them.
const HOLD_MARGIN: f64 = 5e-5;

/// Points taken round the layout's ellipse in a row, which its widened ellipse keeps holding.
const LAYOUT_SAMPLES: usize = 48;
/// How far out the layout ellipse's points are taken, as a share of its radii: room for the chords between them.
const LAYOUT_SLACK: f64 = 0.003;
/// The steepest a section's reach past the layout's leans along its region, metres a metre: 45°. Steeper, a garment's
/// rings would need to be closer than a few millimetres to follow it; gentler, the widening at the hips would spread
/// up a chibi's waist to where its arms hang (on soft-sd they come within a centimetre of the torso).
const MAX_SLOPE: f64 = 1.0;
/// The shortest run over which an eased reach falls to nothing, metres.
const MIN_EASE: f64 = 0.01;

/// Rounds of easing a region's widening and holding its points again.
const EASE_ROUNDS: usize = 3;

/// Passes over a row's points before its ellipse is scaled to hold them all.
const SETTLE_PASSES: usize = 32;

/// How far past a section's ellipse a point may lie and still count as inside, as a level (`1` is on the ellipse).
const INSIDE_LEVEL: f64 = 1.0 + 1e-9;

/// How far a region's box reaches round its rows, in radii: a point the widening holds lies within it.
const BOUNDS_GROWTH: f64 = 3.0;

/// One row of a region's table: the layout's section there, with its plane's frame.
#[derive(Copy, Clone, Debug)]
struct Row {
    centre: DVec3,
    axis: DVec3,
    across: DVec3,
    through: DVec3,
    radii: DVec2,
}

impl Row {
    fn of(layout: &BodyLayout, region: BodyRegion, t: f64) -> Option<Self> {
        let s = layout.section64(region, t)?;
        Some(Self::from_section(region, s))
    }

    fn from_section(region: BodyRegion, s: RegionSection) -> Self {
        let (across, through) = section_basis(region, s.axis.as_vec3());
        Self { centre: s.centre, axis: s.axis, across: across.as_dvec3(), through: through.as_dvec3(), radii: s.radii }
    }

    /// The signed distance of `p` from this row's plane, along its axis.
    fn height(&self, p: DVec3) -> f64 {
        (p - self.centre).dot(self.axis)
    }

    /// The row a fraction `s` of the way to `other`.
    fn lerp(&self, other: &Self, s: f64) -> Self {
        Self {
            centre: self.centre.lerp(other.centre, s),
            axis: self.axis.lerp(other.axis, s).normalize_or_zero(),
            across: self.across.lerp(other.across, s).normalize_or_zero(),
            through: self.through.lerp(other.through, s).normalize_or_zero(),
            radii: self.radii.lerp(other.radii, s),
        }
    }
}

/// How far a section's ellipse reaches past the layout's on each side: `+across`, `-across`, `+through`, `-through`
/// (metres, never negative).
type Reach = [f64; 4];

/// An ellipse in a row's plane, from the layout's radii and the extra reach: its centre's offset and its radii.
fn ellipse(radii: DVec2, extra: Reach) -> (DVec2, DVec2) {
    let offset = DVec2::new(extra[0] - extra[1], extra[2] - extra[3]) / 2.0;
    let r = radii + DVec2::new(extra[0] + extra[1], extra[2] + extra[3]) / 2.0;
    (offset, r)
}

/// `q`'s level in the ellipse (below 1 inside).
fn level(q: DVec2, (offset, r): (DVec2, DVec2)) -> f64 {
    ((q - offset) / r.max(DVec2::splat(f64::MIN_POSITIVE))).length()
}

/// `extra` grown until the ellipse of `radii` and it holds `q`, keeping the reach across the body (`x`) as tight as it
/// can: a point the ellipse already reaches across is held by growing the front-to-back reach (`y`) on its side alone,
/// and only a point past the reach across grows that reach, just to it. An ellipse round a square-ish section (the hips,
/// the pelvis and both thighs) then reaches no further to the sides than the body does, where the arms hang, and takes
/// the corners in front and behind. If that hasn't closed in after [`GROWTH_STEPS`], every reach grows to scale the
/// whole ellipse round its centre, which always holds it.
fn hold(radii: DVec2, mut extra: Reach, q: DVec2) -> Reach {
    for _ in 0..GROWTH_STEPS {
        let (offset, r) = ellipse(radii, extra);
        if level(q, (offset, r)) <= INSIDE_LEVEL {
            return extra;
        }
        let d = q - offset;
        let (x_side, y_side) = (if d.x >= 0.0 { 0 } else { 1 }, if d.y >= 0.0 { 2 } else { 3 });
        // Growing one side by `g` moves its edge out by `g` and the centre and radius by half of it each.
        if d.x.abs() >= ACROSS_SHARE * r.x {
            // Reach across to the point, with a little to spare for its front-to-back growth to close on.
            extra[x_side] += 2.0 * (d.x.abs() - ACROSS_SHARE * r.x) / (1.0 + ACROSS_SHARE) + f64::EPSILON;
            continue;
        }
        // Across, the point lies within `c = sqrt(1 - (dx / rx)^2)` of the axis's reach; grow front to back until
        // `|dy| - g / 2 <= c (ry + g / 2)`.
        let c = (1.0 - (d.x / r.x).powi(2)).sqrt();
        extra[y_side] += 2.0 * (d.y.abs() - c * r.y).max(0.0) / (1.0 + c) * (1.0 + 1e-9) + f64::EPSILON;
    }
    let (offset, r) = ellipse(radii, extra);
    let l = level(q, (offset, r));
    if l > INSIDE_LEVEL {
        // Scale round the centre: each reach grows by (l - 1) times its radius.
        let more = (r * (l - 1.0)).to_array();
        extra = [extra[0] + more[0], extra[1] + more[0], extra[2] + more[1], extra[3] + more[1]];
    }
    extra
}

/// One region's table: the layout's rows and how far the fit reaches past each.
#[derive(Clone, Debug)]
struct Table {
    region: BodyRegion,
    rows: Vec<Row>,
    extra: Vec<Reach>,
    /// The points each row has been widened to hold, in its plane (each [`HOLD_MARGIN`] further out than the mesh's).
    kept: Vec<Vec<DVec2>>,
    /// A box round every row's ellipse grown by the largest reach: a point outside it is in none of the region's
    /// sections.
    bounds: (DVec3, DVec3),
}

/// Where a point lies on a region: the row before its section plane, how far to the next, and its coordinates in the
/// plane.
#[derive(Copy, Clone, Debug)]
struct Located {
    row: usize,
    share: f64,
    q: DVec2,
}

impl Table {
    fn of(layout: &BodyLayout, region: BodyRegion) -> Option<Self> {
        let rows = (0..=ROWS).map(|k| Row::of(layout, region, fraction(k, ROWS))).collect::<Option<Vec<_>>>()?;
        // Three times the widest radius: room for the widening, which never reaches further than the regions it joins.
        let reach = |r: &Row| DVec3::splat(BOUNDS_GROWTH * r.radii.max_element());
        let lo = rows.iter().map(|r| r.centre - reach(r)).reduce(DVec3::min)?;
        let hi = rows.iter().map(|r| r.centre + reach(r)).reduce(DVec3::max)?;
        Some(Self {
            region,
            extra: vec![[0.0; 4]; rows.len()],
            kept: vec![Vec::new(); rows.len()],
            rows,
            bounds: (lo, hi),
        })
    }

    /// Where `p` lies along the region in rows: `k + share` between rows `k` and `k + 1` whose planes it lies between
    /// ([`locate`](Self::locate)), else `-1` before the first row's plane and `ROWS + 1` past the last's.
    fn row_coordinate(&self, p: DVec3) -> f64 {
        match self.locate(p) {
            Some(at) => fraction(at.row, 1) + at.share,
            None if self.rows[0].height(p) < 0.0 => -1.0,
            None => fraction(ROWS + 1, 1),
        }
    }

    /// The plane of this region's sections through `p`, if one is: of the places along the region whose plane passes
    /// through `p` (where its height above two rows' planes changes sign), the one where it lies deepest in the layout's
    /// section, and its coordinates in that plane.
    fn locate(&self, p: DVec3) -> Option<Located> {
        if p.cmplt(self.bounds.0).any() || p.cmpgt(self.bounds.1).any() {
            return None;
        }
        let mut best: Option<(f64, Located)> = None;
        let mut below = self.rows[0].height(p);
        for (row, pair) in self.rows.windows(2).enumerate() {
            let (ha, hb) = (below, pair[1].height(p));
            below = hb;
            if ha.signum() == hb.signum() && ha != 0.0 {
                continue;
            }
            let share = if ha == hb { 0.0 } else { (ha / (ha - hb)).clamp(0.0, 1.0) };
            let plane = pair[0].lerp(&pair[1], share);
            let d = p - plane.centre;
            let q = DVec2::new(d.dot(plane.across), d.dot(plane.through));
            let l = level(q, (DVec2::ZERO, plane.radii));
            if best.as_ref().is_none_or(|(bl, _)| l < *bl) {
                best = Some((l, Located { row, share, q }));
            }
        }
        best.map(|(_, at)| at).or_else(|| self.at_an_end(p))
    }

    /// `p` located on an end row when it lies on that row's plane within [`END_SLACK`]: the mesh's end rings lie on
    /// those planes, and rounding puts their vertices a hair to either side.
    fn at_an_end(&self, p: DVec3) -> Option<Located> {
        let last = self.rows.len() - 1;
        let (row, share, plane) = if self.rows[0].height(p).abs() <= END_SLACK {
            (0, 0.0, self.rows[0])
        } else if self.rows[last].height(p).abs() <= END_SLACK {
            (last - 1, 1.0, self.rows[last])
        } else {
            return None;
        };
        let d = p - plane.centre;
        Some(Located { row, share, q: DVec2::new(d.dot(plane.across), d.dot(plane.through)) })
    }

    /// The layout's radii and the fit's reach where `at` lies.
    fn at(&self, at: &Located) -> (DVec2, Reach) {
        let (a, b) = (at.row, (at.row + 1).min(self.rows.len() - 1));
        let radii = self.rows[a].radii.lerp(self.rows[b].radii, at.share);
        let extra: [f64; 4] =
            std::array::from_fn(|k| self.extra[a][k] + (self.extra[b][k] - self.extra[a][k]) * at.share);
        (radii, extra)
    }

    /// `at`'s level in the layout's own section there.
    fn layout_level(&self, at: &Located) -> f64 {
        let (radii, _) = self.at(at);
        level(at.q, (DVec2::ZERO, radii))
    }

    /// Whether `at`'s point lies outside the layout's own section there.
    fn outside_layout(&self, at: &Located) -> bool {
        self.layout_level(at) > INSIDE_LEVEL
    }

    /// Keeps `p` in the two rows either side of its plane, [`HOLD_MARGIN`] further out, when it lies outside the
    /// layout's section there; [`settle_all`](Self::settle_all) then widens the rows to hold what they keep.
    fn keep_if_outside(&mut self, p: DVec3) {
        let Some(at) = self.locate(p) else { return };
        if !self.outside_layout(&at) {
            return;
        }
        let (a, b) = (at.row, (at.row + 1).min(self.rows.len() - 1));
        let out = at.q.normalize_or_zero();
        for k in [a, b] {
            self.kept[k].push(at.q + out * HOLD_MARGIN);
        }
    }

    /// Keeps `p`, which lies on row `k`'s plane, in that row, [`HOLD_MARGIN`] further out, when it lies outside the
    /// layout's section there.
    fn keep_on_row(&mut self, k: usize, p: DVec3) {
        let row = &self.rows[k];
        let d = p - row.centre;
        let q = DVec2::new(d.dot(row.across), d.dot(row.through));
        if level(q, (DVec2::ZERO, row.radii)) > INSIDE_LEVEL {
            self.kept[k].push(q + q.normalize_or_zero() * HOLD_MARGIN);
        }
    }

    /// Widens every row to hold what it keeps ([`settle`](Self::settle)), then eases the widening along the region
    /// ([`ease`](Self::ease)).
    fn settle_all(&mut self) {
        for k in 0..self.rows.len() {
            if !self.kept[k].is_empty() {
                self.settle(k);
            }
        }
        // Easing raises one side at a time, which moves a centre and can let a held point fall out of the far flank:
        // the rows are settled again, eased again, and settled last, since holding the mesh is what must hold.
        for _ in 0..EASE_ROUNDS {
            self.ease();
            for k in 0..self.rows.len() {
                if !self.kept[k].is_empty() || self.extra[k] != [0.0; 4] {
                    self.settle(k);
                }
            }
        }
    }

    /// Eases each reach along the region: every row's reach becomes the highest of its neighbours' bumps, each a
    /// smoothstep falling from that row's reach to nothing over a run long enough that it never leans more than
    /// [`MAX_SLOPE`] (and at least [`MIN_EASE`]). A row's own bump peaks at its own reach, so no reach falls; and the
    /// bumps leave and meet the layout's section tangent to it, so a garment lofted over the fit in rings a few
    /// millimetres apart follows it without its facets standing out of a corner or cutting into a step.
    fn ease(&mut self) {
        let mut along = vec![0.0_f64; self.rows.len()];
        for k in 1..self.rows.len() {
            along[k] = along[k - 1] + self.rows[k - 1].centre.distance(self.rows[k].centre);
        }
        for side in 0..4 {
            let reach: Vec<f64> = self.extra.iter().map(|e| e[side]).collect();
            for k in 0..self.rows.len() {
                let eased = reach
                    .iter()
                    .zip(&along)
                    .filter(|(r, _)| **r > 0.0)
                    .map(|(r, s)| {
                        // A smoothstep's steepest slope is 1.5 over its run.
                        let run = (1.5 * r / MAX_SLOPE).max(MIN_EASE);
                        let x = ((along[k] - s).abs() / run).min(1.0);
                        r * (1.0 - x * x * (3.0 - 2.0 * x))
                    })
                    .fold(0.0, f64::max);
                self.extra[k][side] = eased.max(reach[k]);
            }
        }
    }

    /// Grows row `k` until its ellipse holds every point it keeps and the layout's own ellipse there. Growing one side
    /// moves the centre that way, which pulls the far side's flanks in, so a point held before can fall out: the
    /// points are held again until none does, and if that hasn't settled in [`SETTLE_PASSES`] the ellipse is scaled
    /// round its centre until it holds them all. A region on the body's midline (the torso, the neck) reaches alike to
    /// either side: the body is symmetric, and its fit stays so whichever thigh or shoulder was held first.
    fn settle(&mut self, k: usize) {
        let radii = self.rows[k].radii;
        let layout = (0..LAYOUT_SAMPLES).map(|j| {
            let (sin, cos) = (TAU * fraction(j, LAYOUT_SAMPLES)).sin_cos();
            DVec2::new(radii.x * cos, radii.y * sin) * (1.0 + LAYOUT_SLACK)
        });
        let points: Vec<DVec2> = layout.chain(self.kept[k].iter().copied()).collect();
        let midline = matches!(self.region, BodyRegion::Torso | BodyRegion::Neck);
        for _ in 0..SETTLE_PASSES {
            let before = self.extra[k];
            for q in &points {
                self.extra[k] = hold(radii, self.extra[k], *q);
            }
            if midline {
                // Alike to either side (only ever growing, so what was held stays held).
                let e = &mut self.extra[k];
                (e[0], e[1]) = (e[0].max(e[1]), e[0].max(e[1]));
            }
            if self.extra[k] == before {
                return;
            }
        }
        let (offset, r) = ellipse(radii, self.extra[k]);
        let l = points.iter().map(|q| level(*q, (offset, r))).fold(1.0, f64::max);
        if l > INSIDE_LEVEL {
            let more = (r * (l - 1.0)).to_array();
            let e = self.extra[k];
            self.extra[k] = [e[0] + more[0], e[1] + more[0], e[2] + more[1], e[3] + more[1]];
        }
    }

    /// The published section `t` of the way along: the rows' either side, taken between them as [`locate`] takes
    /// them (so the fit publishes exactly what it was widened against), reaching as far past the layout's ellipse as
    /// they do.
    ///
    /// [`locate`]: Self::locate
    fn section(&self, t: f64) -> Section {
        let x = t.clamp(0.0, 1.0) * fraction(ROWS, 1);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a clamped, non-negative row index
        let a = (x.floor() as usize).min(ROWS - 1);
        let share = x - fraction(a, 1);
        let row = self.rows[a].lerp(&self.rows[a + 1], share);
        let extra: [f64; 4] =
            std::array::from_fn(|k| self.extra[a][k] + (self.extra[a + 1][k] - self.extra[a][k]) * share);
        let (offset, radii) = ellipse(row.radii, extra);
        let centre = row.centre + row.across * offset.x + row.through * offset.y;
        Section { centre: centre.as_vec3(), axis: row.axis.as_vec3(), radii: radii.as_vec2() }
    }
}

/// The body's published fit: the template's joints and bind pose, and the layout's sections widened to hold the body's
/// mesh (see the module docs).
#[derive(Clone, Debug)]
pub struct BodyMeshFit {
    base: TemplateFit,
    tables: BTreeMap<BodyRegion, Table>,
}

impl BodyMeshFit {
    /// The fit of `base`'s layout widened to hold `meshes`, each region's mesh (see the module docs for which region
    /// holds which part of it).
    pub fn new<'a>(base: TemplateFit, meshes: impl IntoIterator<Item = (BodyRegion, &'a MeshData)>) -> Self {
        let mut tables: Vec<Table> = super::mesh::REGIONS
            .iter()
            .chain(&[BodyRegion::Neck])
            .filter_map(|&region| Table::of(base.layout(), region))
            .collect();
        for (region, mesh) in meshes {
            keep_outside(&mut tables, region, mesh);
        }
        for table in &mut tables {
            table.settle_all();
        }
        Self { base, tables: tables.into_iter().map(|t| (t.region, t)).collect() }
    }

    /// The template fit under the widening.
    pub fn template(&self) -> &TemplateFit {
        &self.base
    }

    /// The layout behind the fit, at full precision.
    pub fn layout(&self) -> &BodyLayout {
        self.base.layout()
    }
}

/// How far off an end row's plane a point may lie and still be on it, metres.
const END_SLACK: f64 = 1e-6;

/// One end of a region.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Span {
    /// Before its start.
    Before,
    /// Past its end.
    After,
}

/// How near an end of its region, in rows, a point is held by the region that end closes into as well as its own.
const END_BAND: f64 = 1.0;

/// Which of `tables` must hold point `p` of the mesh of `region` (indices into `tables`):
/// - a point within its region's span, that region;
/// - a point past an end, the region that end closes into: a shoulder's top (an arm's root above its first ring) and a
///   thigh's dome the torso, a shin's ankle the foot, the pelvis's floor the thigh it lies least far outside of, the
///   torso's top the neck; the root's underside (the armpit), a wrist's cap (the hand covers it) and a foot's heel and
///   toe none;
/// - and a point of a leg that the torso's sections reach, the torso too: the hips' outline is the pelvis's and the
///   thighs' together, so a garment round the torso there clears the thighs.
fn holders(tables: &[Table], region: BodyRegion, p: DVec3) -> Vec<usize> {
    use BodyRegion as R;
    let index = |r: BodyRegion| tables.iter().position(|t| t.region == r);
    let Some(own) = index(region) else { return Vec::new() };
    let x = tables[own].row_coordinate(p);
    let rows = fraction(ROWS, 1);
    // The region each end closes into. The pelvis's floor closes into whichever thigh it lies least far outside of.
    let closes_into = |end: Span| -> Option<usize> {
        match (region, end) {
            // The shoulder's top: the root's upper side, above the arm's first ring, makes the shoulder line. Its
            // underside runs between the arm and the torso into the armpit, where a garment needs an armhole, not
            // a torso wide enough to reach the arm.
            (R::ArmL | R::ArmR, Span::Before) => index(R::Torso).filter(|_| p.y >= tables[own].rows[0].centre.y),
            (R::LegL | R::LegR, Span::Before) => index(R::Torso),
            (R::LegL, Span::After) => index(R::FootL),
            (R::LegR, Span::After) => index(R::FootR),
            (R::Torso, Span::After) => index(R::Neck),
            (R::Torso, Span::Before) => {
                let level = |r: BodyRegion| {
                    index(r).and_then(|k| tables[k].locate(p).map(|at| (k, tables[k].layout_level(&at))))
                };
                [level(R::LegL), level(R::LegR)]
                    .into_iter()
                    .flatten()
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(k, _)| k)
            }
            _ => None,
        }
    };
    let mut out: Vec<usize> = Vec::new();
    if (0.0..=rows).contains(&x) {
        out.push(own);
    }
    // Within a row of an end, the region it closes into holds the point too: a point that close to the end's plane may
    // fall on either side of it as another table samples the region.
    if x < END_BAND {
        out.extend(closes_into(Span::Before));
    }
    if x > rows - END_BAND {
        out.extend(closes_into(Span::After));
    }
    if matches!(region, R::LegL | R::LegR)
        && let Some(torso) = index(R::Torso)
    {
        out.push(torso);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Finds the points of the mesh of `region` that lie outside the layout's sections of the regions [`holders`] names
/// for them, and gives each to those regions' rows to keep: every vertex, and every point where an edge crosses a
/// holder's rows. The edges matter even between two vertices the layout holds: where the layout's profile curves in
/// (a waist), the mesh's straight edges between two rings stand out of it.
fn keep_outside(tables: &mut [Table], region: BodyRegion, mesh: &MeshData) {
    let points: Vec<DVec3> = mesh.positions.iter().map(|v| v.as_dvec3()).collect();
    let owners: Vec<Vec<usize>> = points.iter().map(|p| holders(tables, region, *p)).collect();
    for (p, owners) in points.iter().zip(&owners) {
        for &k in owners {
            tables[k].keep_if_outside(*p);
        }
    }
    let own = tables.iter().position(|t| t.region == region);
    let torso = tables.iter().position(|t| t.region == BodyRegion::Torso);
    // Where each vertex lies along each table, worked out the first time an edge needs it.
    let mut along: Vec<Vec<Option<f64>>> = vec![vec![None; points.len()]; tables.len()];
    let mut edges = std::collections::BTreeSet::new();
    for tri in mesh.indices.chunks_exact(3) {
        let corners = [tri[0], tri[1], tri[2]].map(|i| i as usize);
        if corners.iter().any(|&i| i >= points.len()) {
            continue;
        }
        for (i, j) in [(corners[0], corners[1]), (corners[1], corners[2]), (corners[2], corners[0])] {
            if !edges.insert((i.min(j), i.max(j))) {
                continue;
            }
            let mut candidates: Vec<usize> = owners[i].iter().chain(&owners[j]).copied().collect();
            candidates.sort_unstable();
            candidates.dedup();
            for k in candidates {
                let mut coordinate = |v: usize| *along[k][v].get_or_insert_with(|| tables[k].row_coordinate(points[v]));
                let (xa, xb) = (coordinate(i), coordinate(j));
                // A point on the region's own rows, or on the torso's rows from a leg, is that table's to hold.
                let always =
                    Some(k) == own || (Some(k) == torso && matches!(region, BodyRegion::LegL | BodyRegion::LegR));
                for (row, p) in crossings(&tables[k], (points[i], xa), (points[j], xb)) {
                    if always || holders(tables, region, p).contains(&k) {
                        tables[k].keep_on_row(row, p);
                    }
                }
            }
        }
    }
}

/// The rows of `table` whose planes the segment from `a` to `b` crosses (each end with where it lies along the table,
/// [`Table::row_coordinate`]), and the point where it crosses each.
fn crossings(table: &Table, (a, xa): (DVec3, f64), (b, xb): (DVec3, f64)) -> Vec<(usize, DVec3)> {
    let (lo, hi) = (xa.min(xb).max(0.0), xa.max(xb).min(fraction(ROWS, 1)));
    if lo > hi {
        return Vec::new();
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // row indices in 0..=ROWS
    let (first, last) = (lo.ceil() as usize, hi.floor() as usize);
    (first..=last)
        .filter_map(|k| {
            let row = &table.rows[k];
            let (ha, hb) = (row.height(a), row.height(b));
            (ha.signum() != hb.signum()).then(|| (k, a.lerp(b, ha / (ha - hb))))
        })
        .collect()
}

impl BodyFit for BodyMeshFit {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        if !t.is_finite() {
            return None;
        }
        match self.tables.get(&region) {
            Some(table) => Some(table.section(f64::from(t))),
            None => self.base.section(region, t),
        }
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        self.base.joint(joint)
    }

    fn bind(&self) -> HumanoidPose {
        self.base.bind()
    }
}
