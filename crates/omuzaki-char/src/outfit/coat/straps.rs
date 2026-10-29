//! What holds up a sleeveless coat (#2447): straps over the shoulders, or a collar band round the neck.
//!
//! **Straps.** A coat cut with straps (no sleeves, a neckline that doesn't stand) ends below the arms' roots at the
//! straps' places on its front and back ([`Frame::neck`], [`Frame::strap_angle`]). Each strap is a [`patch`] that starts
//! [`OVERLAP`] down the bodice's outside there, runs up the chest, over the shoulder and down the back to the same place
//! behind. Each of its [`LINES`] lines across lies in the upright plane through its two ends; seen from a point
//! [`BELOW`] under the shoulder in that plane, it keeps outside everything near the plane, angle by angle: the torso's
//! rings with the arms' roots, the neck, the arms and their roots' caps by the ease, and the bodice's own outside by
//! [`ON_CLOTH`]. The radii are smoothed along the strap so it bends over the shoulder instead of kinking, never brought
//! in by the smoothing.
//!
//! **Collar bands.** A sleeveless high or halter neck is cut in toward the neck, its front (and a high neck's back)
//! rising to the collar line; a [`band`] round the neck from just above the torso's top to that line joins them into
//! one collar.

use std::f64::consts::PI;

use omuzaki_char_api::{BodyRegion, RegionEnd};
use omuzaki_core::glam::DVec3;

use super::edge::{Band, Patch, band, patch};
use super::shell::{Frame, Skinning, Tones};
use crate::kit::curve::{TAU, fraction, lerp};
use crate::kit::grid::MeshBuilder;
use crate::outfit::common::GarmentError;
use crate::outfit::loft::{MIN_EXPONENT, Surface, ring_at};
use crate::template::layout::section_basis;

/// How far down the bodice's outside a strap starts below the bodice's top, metres.
pub const OVERLAP: f64 = 0.012;
/// Lines across a strap.
const LINES: usize = 3;
/// Rows along a strap.
const ROWS: usize = 56;
/// What lies within this of a line's plane is what the line keeps outside, metres.
const SLAB: f64 = 0.006;
/// The angular bins the outline over the shoulder is taken in, radians.
const BIN: f64 = PI / 360.0;
/// Bins either side of a row's angle that its radius keeps outside of.
const REACH_BINS: isize = 3;
/// How far below the lower of its ends the point a strap is seen from lies, metres.
const BELOW: f64 = 0.02;
/// How far a strap's underside stays outside the bodice's outside, metres.
const ON_CLOTH: f64 = 0.0004;
/// Beyond the ease, what sampling the body's sections may miss between two samples, metres.
const MARGIN: f64 = 0.0012;
/// Heights and cap levels are sampled this far apart, metres.
const STEP: f64 = 0.002;
/// Points round each sampled ring.
const RING_POINTS: usize = 128;
/// How far along each arm its rings are taken, as a fraction of the arm.
const ARM_REACH: f64 = 0.35;
/// Rings along that part of each arm.
const ARM_RINGS: usize = 24;
/// The deepest a cap is sampled, metres.
const CAP_DEPTH: f64 = 0.2;
/// What lies further across than this from a strap is left out, metres.
const NEAR: f64 = 0.06;
/// How far below the strap's lower end the body is sampled, metres.
const SAMPLE_BELOW: f64 = 0.03;
/// Smoothing passes along a strap.
const SMOOTHING: usize = 8;
/// Columns round the bodice sampled for the straps to clear.
const BODICE_COLUMNS: usize = 256;
/// A collar band starts this far above the torso's top, metres, clear of the shoulders' roots.
const COLLAR_FOOT: f64 = 0.004;
/// The least height of a collar band, metres.
const MIN_COLLAR: f64 = 0.008;
/// How far round a collar band's point the shoulders' roots are looked for, beyond the cloth, metres.
const COLLAR_REACH: f64 = 0.01;
/// Points round a collar band.
const COLLAR_SEGMENTS: usize = 96;

/// A point a strap keeps outside, and by how much.
type Obstacle = (DVec3, f64);

/// Adds a strap over each shoulder of `frame`'s coat, `width` metres wide, to `mesh`. Returns the open borders it adds
/// (each strap is a patch with two).
///
/// # Errors
/// [`GarmentError`] for a body whose sections can't be sampled, or a strap the kit can't mesh.
pub fn straps(frame: &Frame, width: f64, tones: &Tones, mesh: &mut MeshBuilder) -> Result<usize, GarmentError> {
    let front = frame.strap_angle();
    let mut borders = 0;
    for side in [1.0, -1.0] {
        let starts = starts(frame, side * front, width)?;
        let x = starts.iter().map(|(f, _)| f.x).sum::<f64>() / starts.len().max(1) as f64;
        let lowest = starts.iter().map(|(f, b)| f.y.min(b.y)).fold(f64::INFINITY, f64::min) - SAMPLE_BELOW;
        let obstacles = obstacles(frame, x, lowest)?;
        let lines: Vec<Vec<DVec3>> = starts.iter().map(|&(f, b)| line(&obstacles, f, b)).collect();
        strap(frame, &lines, tones, mesh)?;
        borders += 2;
    }
    Ok(borders)
}

/// The ends of each line across the strap on the side of `front` (the angle of its middle at the front): on the
/// bodice's outside, [`OVERLAP`] below its top, at the front and mirrored about the side at the back.
#[allow(clippy::cast_precision_loss)] // a line count is tiny
fn starts(frame: &Frame, front: f64, width: f64) -> Result<Vec<(DVec3, DVec3)>, GarmentError> {
    let at = |phi: f64| -> Result<DVec3, GarmentError> {
        let y = frame.neck(phi) - OVERLAP;
        Ok(frame.at(y, phi)?.1)
    };
    // The strap's width as an angle round the bodice where it starts.
    let middle = at(front)?;
    let centre = frame.centre(middle.y);
    let radius = (middle - centre).with_y(0.0).length().max(1e-3);
    let spread = width / radius;
    let back = front.signum() * PI - front;
    (0..LINES)
        .map(|j| {
            let s = (j as f64 / (LINES - 1) as f64 - 0.5) * spread;
            // Across the strap the lines keep their order front and back: the back mirrors the front about the side.
            Ok((at(front + s)?, at(back - s)?))
        })
        .collect()
}

/// What a strap near `x` keeps outside, from `lowest` up.
fn obstacles(frame: &Frame, x: f64, lowest: f64) -> Result<Vec<Obstacle>, GarmentError> {
    let ease = frame.cut.ease + MARGIN;
    let near = |p: &DVec3| (p.x - x).abs() < NEAR;
    let round = |k: usize| TAU * fraction(k, RING_POINTS);
    let column = &frame.column;
    let mut out = Vec::new();
    let heights: Vec<f64> = std::iter::successors(Some(lowest), |y| Some(y + STEP))
        .take_while(|&y| y <= column.neck_top() + STEP)
        .collect();
    for &y in &heights {
        let ring = column.ring(y, 0.0)?;
        out.extend((0..RING_POINTS).map(|k| ring.point(round(k))).filter(near).map(|p| (p, ease)));
    }
    out.extend(shoulders(frame).into_iter().filter(near).map(|p| (p, ease)));
    // The bodice's own outside, where it runs under the strap, and the rolled edge along its top.
    let roll = rolled(frame);
    for k in 0..BODICE_COLUMNS {
        let phi = -PI + TAU * fraction(k, BODICE_COLUMNS);
        let top = frame.neck(phi);
        for &y in heights.iter().filter(|&&y| y >= frame.waist() && y <= top) {
            let p = frame.at(y, phi)?.1;
            if near(&p) {
                out.push((p, ON_CLOTH));
            }
        }
        let edge = frame.at(top, phi)?.1;
        if near(&edge) {
            out.push((edge, roll));
        }
    }
    Ok(out)
}

/// How far outside the cloth's outside a piece laid over its top keeps, to clear the rolled edge there: the roll
/// stands out twice the lip ([`shell`](super::shell)'s edges), and [`ON_CLOTH`] beyond.
fn rolled(frame: &Frame) -> f64 {
    2.0 * frame.cut.lip + ON_CLOTH
}

/// The arms near the shoulders, as points: their rings out to [`ARM_REACH`] and their roots' caps.
fn shoulders(frame: &Frame) -> Vec<DVec3> {
    let fit = frame.column.fit().clone();
    let round = |k: usize| TAU * fraction(k, RING_POINTS);
    let mut out = Vec::new();
    for arm in [BodyRegion::ArmL, BodyRegion::ArmR] {
        for k in 0..=ARM_RINGS {
            if let Ok(ring) = ring_at(fit.as_ref(), arm, ARM_REACH * fraction(k, ARM_RINGS), MIN_EXPONENT, 0.0) {
                out.extend((0..RING_POINTS).map(|k| ring.point(round(k))));
            }
        }
        let levels = std::iter::successors(Some(0.0), |h| Some(h + STEP)).take_while(|&h| h <= CAP_DEPTH);
        #[allow(clippy::cast_possible_truncation)] // a fit is sampled in f32
        for section in levels.map_while(|h| fit.cap(arm, RegionEnd::Start, h as f32)) {
            let (across, through) = section_basis(arm, section.axis);
            let (c, a, t) = (section.centre.as_dvec3(), across.as_dvec3(), through.as_dvec3());
            let r = section.radii.as_dvec2();
            out.extend((0..RING_POINTS).map(|k| c + a * (r.x * round(k).cos()) + t * (r.y * round(k).sin())));
        }
    }
    out
}

/// One line along a strap from `front` to `back`, its points [`ROWS`] + 1 even steps of angle apart round the point it
/// is seen from, each outside the `obstacles` near its plane.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)] // bin indices are small
fn line(obstacles: &[Obstacle], front: DVec3, back: DVec3) -> Vec<DVec3> {
    let toward = (front - back).with_y(0.0).normalize_or(DVec3::Z);
    let normal = toward.cross(DVec3::Y);
    let centre = front.midpoint(back).with_y(front.y.min(back.y) - BELOW);
    let polar = |p: DVec3| {
        let d = p - centre;
        let (u, v) = (d.dot(toward), d.y);
        (u.hypot(v), v.atan2(u))
    };
    let dir = |a: f64| toward * a.cos() + DVec3::Y * a.sin();
    let bins = (PI / BIN).ceil() as usize + 1;
    let mut outline = vec![0.0_f64; bins];
    for &(p, clear) in obstacles {
        if (p - centre).dot(normal).abs() > SLAB {
            continue;
        }
        let (r, a) = polar(p);
        if (0.0..=PI).contains(&a) {
            let b = (a / BIN).round() as usize;
            outline[b] = outline[b].max(r + clear);
        }
    }
    let ((r0, a0), (r1, a1)) = (polar(front), polar(back));
    let angles: Vec<f64> = (0..=ROWS).map(|i| lerp(a0, a1, fraction(i, ROWS))).collect();
    let mut radii: Vec<f64> = angles
        .iter()
        .enumerate()
        .map(|(i, &a)| {
            let b = (a / BIN).round() as isize;
            let reach = (b - REACH_BINS..=b + REACH_BINS)
                .filter_map(|k| usize::try_from(k).ok().and_then(|k| outline.get(k)))
                .fold(0.0_f64, |m, &r| m.max(r));
            reach.max(lerp(r0, r1, fraction(i, ROWS)))
        })
        .collect();
    smooth(&mut radii);
    angles.iter().zip(&radii).map(|(&a, &r)| centre + dir(a) * r).collect()
}

/// `radii` smoothed along the strap, each pass a `1 2 1` kernel that keeps the ends and never brings a radius in.
fn smooth(radii: &mut [f64]) {
    let n = radii.len();
    if n < 3 {
        return;
    }
    for _ in 0..SMOOTHING {
        let previous = radii.to_vec();
        for i in 1..n - 1 {
            radii[i] = previous[i].max((previous[i - 1] + 2.0 * previous[i] + previous[i + 1]) / 4.0);
        }
    }
}

/// The strap through `lines` (each [`ROWS`] + 1 points, front to back), as a patch on `mesh`.
fn strap(frame: &Frame, lines: &[Vec<DVec3>], tones: &Tones, mesh: &mut MeshBuilder) -> Result<(), GarmentError> {
    let (rows, cols) = (ROWS + 1, lines.len());
    let (mut points, mut normals, mut skins) = (Vec::new(), Vec::new(), Vec::<Skinning>::new());
    // The outward normal of each row: square to the strap's run along it and to its run across, away from the body.
    for i in 0..rows {
        let along = lines[cols / 2][(i + 1).min(rows - 1)] - lines[cols / 2][i.saturating_sub(1)];
        let across = lines[cols - 1][i] - lines[0][i];
        let mut n = along.cross(across).normalize_or_zero();
        let out = lines[cols / 2][i] - frame.centre(lines[cols / 2][i].y.min(frame.column.top()));
        if n.dot(out) < 0.0 {
            n = -n;
        }
        for line in lines {
            let p = line[i];
            points.push(p);
            normals.push(n);
            // Over the shoulder the strap moves with the top of the torso, not the neck.
            skins.push(frame.skin(p.with_y(p.y.min(frame.column.top() - 1e-3))));
        }
    }
    let piece = Patch {
        rows,
        cols,
        points,
        normals,
        skins,
        thickness: frame.cut.thickness,
        colours: (tones.outer, tones.lining),
        edge: tones.edge,
        uvs: None,
    };
    patch(mesh, &piece)
}

/// Adds the collar band of a sleeveless high or halter neck to `mesh`: round the neck from [`COLLAR_FOOT`] above the
/// torso's top (higher beside a shoulder's root that rises past it) up over the collar line, on the neck's own sections
/// outside the panels that rise to it and the rolled edge along their top, which it covers. Closed all round: no open
/// border.
///
/// # Errors
/// As [`band`].
pub fn collar(frame: &Frame, tones: &Tones, mesh: &mut MeshBuilder) -> Result<(), GarmentError> {
    let column = &frame.column;
    let roll = rolled(frame);
    let lowest = column.top() + COLLAR_FOOT;
    // The neck's own sections, not the column's rings there: those go round the arms' roots, out over the shoulders.
    let offset = frame.cut.ease + frame.cut.thickness + roll;
    let neck = Surface::new(column.fit().clone(), BodyRegion::Neck, frame.cut.exponent, offset)?;
    let span = (column.neck_top() - column.top()).max(1e-3);
    let ring = |y: f64| neck.body_ring(((y - column.top()) / span).clamp(0.0, 1.0));
    let phi = |k: usize| -PI + TAU * fraction(k, COLLAR_SEGMENTS);
    // Each point round the band starts above the shoulders' roots beside it, by the ease.
    let shoulders = shoulders(frame);
    let feet: Vec<f64> = (0..COLLAR_SEGMENTS)
        .map(|k| {
            let at = ring(lowest)?.point(phi(k));
            let reach = frame.cut.ease + frame.cut.thickness + COLLAR_REACH;
            let above = shoulders
                .iter()
                .filter(|p| (**p - at).with_y(0.0).length() < reach && p.y >= lowest - reach)
                .fold(lowest, |y, p| y.max(p.y + frame.cut.ease + frame.cut.thickness));
            Ok(above)
        })
        .collect::<Result<_, GarmentError>>()?;
    // Over the collar line and the roll along it, and over the shoulders' roots where they rise beside a short neck.
    let highest = feet.iter().copied().fold(lowest, f64::max);
    let top = (frame.top + roll).max(highest + MIN_COLLAR).min(column.neck_top());
    let feet: Vec<f64> = feet.into_iter().map(|f| f.min(top - MIN_COLLAR)).collect();
    let height = (top - lowest).max(MIN_COLLAR);
    let surface = |k: usize, dy: f64| -> Result<(DVec3, DVec3, Skinning), GarmentError> {
        // `dy` runs over the band's height either side of its middle: here, from this point's foot to the top.
        let y = lerp(feet[k], top, (dy / height + 0.5).clamp(0.0, 1.0));
        let ring = ring(y)?;
        let p = ring.point(phi(k));
        Ok((p, ring.normal(phi(k)), frame.skin(p)))
    };
    band(
        mesh,
        &Band {
            around: COLLAR_SEGMENTS,
            surface: &surface,
            height,
            thickness: frame.cut.thickness,
            colour: tones.outer,
            ribs: 0,
        },
    )
}
