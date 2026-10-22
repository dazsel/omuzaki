//! A coat's sleeves: the shirt's sleeve ([`sleeve`](crate::outfit::shirt::sleeve)) cut a thickness wider, belled,
//! lengthened or lined as the kind asks, with a rolled cuff, and welded to the bodice the way the shirt's are.

use std::ops::Range;

use omuzaki_char_api::{BuildCtx, BuildError, PartOutput};
use omuzaki_core::glam::{DVec2, DVec3};

use super::edge::{Tube, loop_tube};
use super::shell::{Frame, Skinning, Tones};
use crate::kit::grid::MeshBuilder;
use crate::outfit::common::GarmentError;
use crate::outfit::shirt::sleeve::{self, SEGMENTS, SleeveShape, SleeveSpec};
use crate::outfit::shirt::union;

/// How far clear of the bodice a sleeve's end stands at least, metres.
const END_CLEARANCE: f64 = 0.004;
/// How much a sleeve too short to clear the bodice is lengthened at a time, as a fraction of the arm.
const LENGTH_STEP: f64 = 0.05;

/// How a coat's sleeves are cut.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SleeveCut {
    /// The extra width over the arm, metres.
    pub width: f64,
    /// Extra ease at the elbow, metres.
    pub ease_at_elbow: f64,
    /// How much of the arm they cover, 0 to 1.
    pub length: f64,
    /// The bell, the length past the wrist and the lining.
    pub shape: SleeveShape,
    /// The rolled cuff's radius beyond the cloth, metres; 0 for none.
    pub cuff: f64,
}

/// Both sleeves in one mesh, left then right.
#[derive(Debug)]
pub struct Sleeves {
    /// The mesh.
    pub mesh: MeshBuilder,
    /// The left arm with the sleeve's ease: what the bodice is trimmed against (the right is its mirror).
    pub arm: union::Tube,
    /// Each side's sleeve sheets (the outside and the lining) in the mesh: what the bodice trims.
    pub sheets: Vec<Range<usize>>,
    /// The sleeve's radius at its end, metres.
    pub end_radius: f64,
}

/// The skins of the vertices `range` of `mesh`, as joint lists.
fn skins_of(mesh: &MeshBuilder, range: Range<usize>) -> Vec<Skinning> {
    let (data, bones) = (mesh.mesh(), mesh.bones());
    range
        .map(|k| {
            data.joints[k]
                .iter()
                .zip(&data.weights[k])
                .filter(|(_, w)| **w > 0.0)
                .map(|(j, w)| (bones[usize::from(*j)].clone(), *w))
                .collect()
        })
        .collect()
}

/// Builds the sleeves of `frame` cut by `cut`, their cuffs' rolls in `tones.edge`.
///
/// # Errors
/// As [`sleeve::build_shaped`]; the kit's.
pub fn build(frame: &Frame, cut: &SleeveCut, tones: &Tones, tile: Option<f64>) -> Result<Sleeves, GarmentError> {
    let fit = frame.column.fit();
    let spec = SleeveSpec {
        ease: frame.cut.ease + frame.cut.thickness,
        width: cut.width,
        ease_at_elbow: cut.ease_at_elbow,
        length: cut.length,
    };
    let shape = SleeveShape { lining: if cut.shape.lining > 0.0 { frame.cut.thickness } else { 0.0 }, ..cut.shape };
    let mut left = sleeve::build_shaped(fit.as_ref(), &spec, &shape, tile)?;
    // On a short arm a short sleeve can end where the bodice still covers it: it is lengthened until its end stands
    // clear of the bodice, so the weld leaves it a cuff.
    let mut spec = spec;
    while spec.length < 1.0 && left.end.0.iter().any(|p| in_body(frame, *p) > -END_CLEARANCE) {
        spec.length = (spec.length + LENGTH_STEP).min(1.0);
        left = sleeve::build_shaped(fit.as_ref(), &spec, &shape, tile)?;
    }
    let ring = left.rows * (SEGMENTS + 1);
    let mut side = left.mesh.clone();
    let mut sheets = Vec::with_capacity(4);
    sheets.push(0..ring);
    if left.end.1.is_some() {
        sheets.push(ring..2 * ring);
    }
    if cut.cuff > 0.0 {
        let (outer, lining) = &left.end;
        let skins = skins_of(&side, (left.rows - 1) * (SEGMENTS + 1)..ring);
        let centres: Vec<DVec3> = match lining {
            Some(lining) => outer.iter().zip(lining).map(|(a, b)| a.midpoint(*b)).collect(),
            None => outer.clone(),
        };
        let (points, skins) = (&centres[..SEGMENTS], &skins[..SEGMENTS]);
        let wrist = left.wrist;
        let along = left.forearm;
        // The roll's rings face away from the sleeve's own line.
        let axis = |p: DVec3| wrist + along * (p - wrist).dot(along);
        let radius = frame.cut.thickness / 2.0 + cut.cuff;
        loop_tube(&mut side, &Tube { points, skins, radius, colour: tones.edge }, &axis)?;
    }
    let mut mesh = side.clone();
    let offset = side.vertex_count();
    mesh.merge(&side.mirrored())?;
    let right: Vec<Range<usize>> = sheets.iter().map(|r| r.start + offset..r.end + offset).collect();
    sheets.extend(right);
    Ok(Sleeves { mesh, arm: left.arm, sheets, end_radius: left.end_radius })
}

/// How far `p` is inside the bodice's outside (the cloth's ease and thickness over the torso), metres: negative
/// outside it.
fn in_body(frame: &Frame, p: DVec3) -> f64 {
    let column = &frame.column;
    if !(column.bottom()..=column.neck_top()).contains(&p.y) {
        return f64::NEG_INFINITY;
    }
    let outside = frame.cut.ease + frame.cut.thickness + union::LIFT;
    let Ok(ring) = column.ring(p.y, outside) else { return f64::NEG_INFINITY };
    let d = p - ring.centre;
    -ring.along_ray(DVec2::new(d.dot(ring.across), d.dot(ring.through))).1
}

/// Welds the sleeves (`<part>-sleeves`) and the bodice of the shell (`<part>-shell`) in `output` into one surface: the
/// bodice trimmed where the arms with the sleeves' ease pass through it, each sleeve where it runs inside the bodice
/// ([`union::weld`]). Each mesh is welded on its own: what the other piece is is only its depth function.
///
/// # Errors
/// [`BuildError::Part`] for a mesh the output lacks or a weld that fails.
pub fn weld(
    output: &mut PartOutput,
    ctx: &BuildCtx<'_>,
    frame: &Frame,
    bodice: &[Range<usize>; 2],
    sleeves: &Sleeves,
) -> Result<(), BuildError> {
    let arm = &sleeves.arm;
    let right = arm.mirrored();
    let in_arm = |p: DVec3| arm.inside(p).max(right.inside(p));
    let in_body = |p: DVec3| in_body(frame, p);
    let bodice: Vec<union::Cut<'_>> =
        bodice.iter().map(|r| union::Cut { vertices: r.clone(), depth: &in_arm }).collect();
    let sleeve_cuts: Vec<union::Cut<'_>> =
        sleeves.sheets.iter().map(|r| union::Cut { vertices: r.clone(), depth: &in_body }).collect();
    for (name, cuts) in [("shell", bodice), ("sleeves", sleeve_cuts)] {
        let name = format!("{}-{name}", ctx.part);
        let mesh = output
            .meshes
            .iter_mut()
            .find(|m| m.name == name)
            .ok_or_else(|| ctx.fail(format!("the garment has no mesh `{name}`")))?;
        union::weld(&mut mesh.geometry, &cuts).map_err(|e| ctx.fail(e.to_string()))?;
        mesh.validate().map_err(|e| ctx.fail(format!("the welded `{name}`: {e}")))?;
    }
    Ok(())
}

/// Tints the vertex colours of `<part>-sleeves`: the sleeves' own shading times the cloth's colour on the outside and
/// the lining's inside. `ranges` are the lining's vertices before the weld; the weld keeps the order, so this runs
/// before it.
///
/// # Errors
/// [`BuildError::Part`] if the output has no sleeves.
pub fn tint(output: &mut PartOutput, ctx: &BuildCtx<'_>, sleeves: &Sleeves, tones: &Tones) -> Result<(), BuildError> {
    let name = format!("{}-sleeves", ctx.part);
    let mesh = output
        .meshes
        .iter_mut()
        .find(|m| m.name == name)
        .ok_or_else(|| ctx.fail(format!("the garment has no mesh `{name}`")))?;
    let per_side = sleeves.sheets.len() / 2;
    let lining: Vec<&Range<usize>> =
        sleeves.sheets.iter().enumerate().filter(|(k, _)| k % per_side == 1).map(|(_, r)| r).collect();
    let outer: Vec<&Range<usize>> =
        sleeves.sheets.iter().enumerate().filter(|(k, _)| k % per_side == 0).map(|(_, r)| r).collect();
    for (ranges, tone) in [(outer, tones.outer), (lining, tones.lining)] {
        for range in ranges {
            for c in &mut mesh.geometry.colours[range.clone()] {
                for (channel, t) in c.iter_mut().zip(tone) {
                    *channel *= t;
                }
            }
        }
    }
    Ok(())
}
