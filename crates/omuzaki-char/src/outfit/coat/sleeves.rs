//! A coat's sleeves: the shirt's sleeve ([`sleeve`](crate::outfit::shirt::sleeve)) cut a thickness wider, belled,
//! lengthened or lined as the kind asks, with a rolled cuff, and welded to the bodice the way the shirt's are.

use std::ops::Range;

use omuzaki_char_api::{BuildCtx, BuildError, PartOutput};
use omuzaki_core::glam::{DVec2, DVec3};

use super::edge::{Tube, loop_tube};
use super::shell::{Frame, Skinning, Tones};
use crate::kit::curve::fraction;
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
    /// A knitted rib cuff's length up the arm from the sleeve's end, metres; 0 for none.
    pub rib: f64,
    /// A pouch hanging from the sleeve's underside (a kimono's), if any.
    pub pouch: Option<Pouch>,
}

/// A pouch of cloth hanging from a sleeve's underside, from part way down the arm to its end, its far bottom corner
/// rounded (a kimono's sleeve).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Pouch {
    /// Where along the sleeve it starts, as a fraction from the shoulder (0) to the sleeve's end (1).
    pub from: f64,
    /// How far it hangs below the sleeve, as a fraction of the sleeve's length.
    pub drop: f64,
    /// Its far bottom corner's radius, as a fraction of the drop.
    pub corner: f64,
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
    if cut.rib > 0.0 {
        rib_cuff(&mut side, &left, cut.rib, frame.cut.thickness, tones)?;
    }
    if let Some(pouch) = &cut.pouch {
        hang_pouch(&mut side, &left, pouch, frame, tones, tile)?;
    }
    let mut mesh = side.clone();
    let offset = side.vertex_count();
    mesh.merge(&side.mirrored())?;
    let right: Vec<Range<usize>> = sheets.iter().map(|r| r.start + offset..r.end + offset).collect();
    sheets.extend(right);
    Ok(Sleeves { mesh, arm: left.arm, sheets, end_radius: left.end_radius })
}

/// Points round a rib cuff.
const RIB_AROUND: usize = 96;
/// Ribs round a rib cuff.
const RIB_RIBS: usize = 24;
/// How far a rib cuff stands off the sleeve under it, metres.
const RIB_STANDOFF: f64 = 0.0005;
/// How far past the sleeve's end a rib cuff reaches, metres, covering it.
const RIB_PAST: f64 = 0.002;

/// Adds a knitted rib cuff `length` long round the end of the left sleeve `left` (whose outside is the first sheet of
/// `side`): a closed [`band`](super::edge::band) following the sleeve, from up the arm to just past its end.
///
/// # Errors
/// The kit's.
#[allow(clippy::many_single_char_names, clippy::cast_precision_loss)] // the grid's textbook names; tiny counts
fn rib_cuff(
    side: &mut MeshBuilder,
    left: &sleeve::Sleeve,
    length: f64,
    thickness: f64,
    tones: &Tones,
) -> Result<(), GarmentError> {
    let cols = SEGMENTS + 1;
    let positions: Vec<DVec3> = side.mesh().positions[..left.rows * cols].iter().map(|p| p.as_dvec3()).collect();
    let skins = skins_of(side, 0..left.rows * cols);
    let along = |p: DVec3| (p - left.wrist).dot(left.forearm);
    #[allow(clippy::cast_precision_loss)] // a ring's point count is tiny
    let rows: Vec<f64> = (0..left.rows)
        .map(|r| positions[r * cols..r * cols + SEGMENTS].iter().map(|p| along(*p)).sum::<f64>() / SEGMENTS as f64)
        .collect();
    let end = rows[left.rows - 1];
    let middle = end + RIB_PAST - length / 2.0;
    // The sleeve's outside at `a` along the forearm and `u` (0 to 1) round it, pushed off it.
    let surface = |k: usize, dy: f64| -> Result<(DVec3, DVec3, Skinning), GarmentError> {
        let a = (middle - dy).min(end);
        let r = rows.iter().rposition(|x| *x <= a).unwrap_or(0).min(left.rows - 2);
        let f = ((a - rows[r]) / (rows[r + 1] - rows[r])).clamp(0.0, 1.0);
        let u = fraction(k, RIB_AROUND) * SEGMENTS as f64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a segment index
        let s = (u.floor() as usize).min(SEGMENTS - 1);
        let g = u - s as f64;
        let at = |row: usize| positions[row * cols + s].lerp(positions[row * cols + s + 1], g);
        let p = at(r).lerp(at(r + 1), f);
        let axis = left.wrist + left.forearm * along(p);
        let normal = (p - axis).normalize_or_zero();
        // Where the sleeve stops, the cuff runs on along the forearm.
        let p = p + left.forearm * (middle - dy - a);
        Ok((p + normal * RIB_STANDOFF, normal, skins[(r + usize::from(f > 0.5)) * cols + s].clone()))
    };
    super::edge::band(
        side,
        &super::edge::Band {
            around: RIB_AROUND,
            surface: &surface,
            height: length,
            thickness,
            colour: tones.outer,
            ribs: RIB_RIBS,
        },
    )
}

/// Rows down a pouch.
const POUCH_ROWS: usize = 10;
/// How far a pouch keeps off the coat's outside, metres, beyond its rolled edges.
const POUCH_MARGIN: f64 = 0.004;
/// How far under the sleeve a pouch's top edge lies, metres.
const POUCH_UNDER: f64 = 0.001;

/// Adds `pouch` to `side`, under the left sleeve `left` (whose outside is the first sheet of `side`): a lined
/// [`patch`](super::edge::patch) hanging straight down from the lowest point of each of the sleeve's rings, skinned
/// as that point is, kept clear of the coat's outside (and so of the body). Two open borders, under its roll.
///
/// # Errors
/// [`GarmentError::Invalid`] for a pouch along fewer than two of the sleeve's rings; the kit's.
#[allow(clippy::many_single_char_names)] // the grid's textbook names
fn hang_pouch(
    side: &mut MeshBuilder,
    left: &sleeve::Sleeve,
    pouch: &Pouch,
    frame: &Frame,
    tones: &Tones,
    tile: Option<f64>,
) -> Result<(), GarmentError> {
    let cols = SEGMENTS + 1;
    let positions: Vec<DVec3> = side.mesh().positions[..left.rows * cols].iter().map(|p| p.as_dvec3()).collect();
    let skins = skins_of(side, 0..left.rows * cols);
    let along = |p: DVec3| (p - left.wrist).dot(left.forearm);
    // The lowest point of each ring, and its index.
    let lowest: Vec<usize> = (0..left.rows)
        .map(|r| {
            (r * cols..r * cols + SEGMENTS)
                .min_by(|a, b| positions[*a].y.total_cmp(&positions[*b].y))
                .unwrap_or(r * cols)
        })
        .collect();
    let first = along(positions[lowest[0]]);
    let last = along(positions[lowest[left.rows - 1]]);
    let length = (last - first).abs();
    let start = first + (last - first) * pouch.from.clamp(0.0, 1.0);
    let ring: Vec<usize> =
        lowest.iter().copied().filter(|k| (along(positions[*k]) - start) * (last - first).signum() >= 0.0).collect();
    if ring.len() < 2 {
        return Err(GarmentError::invalid("pouch", format!("{} of the sleeve's rings under it: need two", ring.len())));
    }
    let drop = pouch.drop * length;
    let radius = pouch.corner.clamp(0.0, 1.0) * drop;
    // Its plane holds the forearm's line across and the vertical: its normal faces the front.
    let across = DVec3::new(left.forearm.x, 0.0, left.forearm.z).try_normalize().unwrap_or(DVec3::X);
    let normal = {
        let n = across.cross(DVec3::Y);
        if n.z < 0.0 { -n } else { n }
    };
    let need = frame.cut.thickness + frame.cut.lip + POUCH_MARGIN;
    let (mut points, mut normals, mut patch_skins, mut uvs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for &k in &ring {
        let top = positions[k] - DVec3::Y * POUCH_UNDER;
        // How far short of the far end it is, metres: the corner rounds off the last `radius` of it.
        let short = (along(positions[ring[ring.len() - 1]]) - along(positions[k])).abs();
        let s = ((radius - short) / radius.max(1e-9)).clamp(0.0, 1.0);
        let reach = drop - radius * (1.0 - (1.0 - s * s).sqrt());
        for j in 0..POUCH_ROWS {
            let t = fraction(j, POUCH_ROWS - 1);
            let p = clear_of_coat(frame, top - DVec3::Y * reach * t, need);
            points.push(p);
            normals.push(normal);
            patch_skins.push(skins[k].clone());
            if let Some(tile) = tile {
                let a = (along(positions[k]) - along(positions[ring[0]])).abs();
                uvs.push(DVec2::new(a / tile, reach * t / tile));
            }
        }
    }
    let piece = super::edge::Patch {
        rows: ring.len(),
        cols: POUCH_ROWS,
        points,
        normals,
        skins: patch_skins,
        thickness: frame.cut.thickness,
        colours: (tones.outer, tones.lining),
        edge: tones.edge,
        uvs: tile.map(|_| uvs),
    };
    super::edge::patch(side, &piece)
}

/// `p` moved out from the coat's axis until it stands `need` outside the lining, where the coat reaches its height.
fn clear_of_coat(frame: &Frame, p: DVec3, need: f64) -> DVec3 {
    if !(frame.hem..=frame.top).contains(&p.y) {
        return p;
    }
    let Ok(ring) = frame.ring(p.y) else { return p };
    let d = p - ring.centre;
    let q = DVec2::new(d.dot(ring.across), d.dot(ring.through));
    let (_, outside) = ring.along_ray(q);
    let Some(dir) = q.try_normalize().filter(|_| outside < need) else { return p };
    let moved = dir * (need - outside);
    p + ring.across * moved.x + ring.through * moved.y
}

/// How far `p` is inside the bodice's outside (the cloth's ease and thickness over the torso), metres: negative outside
/// it.
fn in_body(frame: &Frame, p: DVec3) -> f64 {
    let column = &frame.column;
    if !(column.bottom()..=column.neck_top()).contains(&p.y) {
        return f64::NEG_INFINITY;
    }
    let outside = frame.cut.ease + frame.cut.thickness + union::LIFT;
    let Ok(ring) = column.ring(p.y, outside) else { return f64::NEG_INFINITY };
    let d = p - ring.centre;
    // Only round the armhole is the bodice welded to the sleeves: below it the depth falls away, so the cut closes off
    // smoothly and what is under it is moved clear instead ([`clear_torso`]).
    let falling = -ring.along_ray(DVec2::new(d.dot(ring.across), d.dot(ring.through))).1
        - (floor(frame) - p.y).max(0.0) * BELOW_ARMHOLE;
    // Over the bodice the cut (where the weld slides vertices to) never falls nearer the torso than half the ease and
    // half the margin [`clear_torso`] keeps, measured as it measures. Under the waist seam it falls away freely.
    if p.y < frame.waist() {
        return falling;
    }
    let Ok(body) = column.ring(p.y, 0.0) else { return falling };
    let d = p - body.centre;
    let near = frame.cut.ease / 2.0 + TORSO_MARGIN / 2.0
        - body.along_ray(DVec2::new(d.dot(body.across), d.dot(body.through))).1;
    falling.max(near)
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

/// How fast the depth the bodice cuts the sleeves by falls away below the armhole, metres per metre.
const BELOW_ARMHOLE: f64 = 1.0;
/// How far below the shoulder's joint the armhole reaches, in the arm's radii at its root.
const ARMPIT: f64 = 2.2;

/// The lowest the sleeves are welded to the bodice: the armpit, or the waist seam if that is higher (below it the skirt
/// hangs clear of the arms).
fn floor(frame: &Frame) -> f64 {
    let fit = frame.column.fit();
    let shoulder = fit.joint(&omuzaki_char_api::JointRef::Humanoid(omuzaki_anim_api::HumanoidBone::LeftUpperArm));
    let root = crate::outfit::loft::ring_at(fit.as_ref(), omuzaki_char_api::BodyRegion::ArmL, 0.0, 2.0, 0.0);
    let armpit = match (shoulder, root) {
        (Some(s), Ok(r)) => f64::from(s.y) - ARMPIT * r.radii.max_element(),
        _ => frame.waist(),
    };
    armpit.max(frame.waist())
}

/// How far beyond half the ease the sleeves' lining keeps off the torso below the armhole, metres.
const TORSO_MARGIN: f64 = 0.0012;
/// How many halvings find how far a vertex can be lifted off the torso before it meets the arm.
const LIFT_STEPS: usize = 24;
/// How far a vertex caught between the side and the arm slides out of the crease per step, metres, and at most how many
/// steps.
const CREASE_STEP: f64 = 0.0002;
const CREASE_STEPS: usize = 100;
/// The step of the numerical gradients that find the way out of the crease, metres.
const GRADIENT_STEP: f64 = 1e-4;

/// Moves the vertices of `<part>-sleeves` below the armhole ([`floor`]) out from the torso's axis until they clear it: on
/// a body whose arms hang against its sides a sleeve, round the arm with its ease, would come nearer the torso than the
/// cloth is allowed to (round the armhole the bodice is welded to it instead). The lining keeps half the ease and a margin off
/// the torso, the outside a cloth's thickness more. `ranges` are the lining's vertices; the weld keeps the order, so
/// this runs before it.
///
/// # Errors
/// [`BuildError::Part`] if the output has no sleeves or a ring the fit can't give.
pub fn clear_torso(
    output: &mut PartOutput,
    ctx: &BuildCtx<'_>,
    frame: &Frame,
    sleeves: &Sleeves,
) -> Result<(), BuildError> {
    let name = format!("{}-sleeves", ctx.part);
    let mesh = output
        .meshes
        .iter_mut()
        .find(|m| m.name == name)
        .ok_or_else(|| ctx.fail(format!("the garment has no mesh `{name}`")))?;
    let per_side = sleeves.sheets.len() / 2;
    let lining: Vec<&Range<usize>> =
        sleeves.sheets.iter().enumerate().filter(|(k, _)| per_side > 1 && k % per_side == 1).map(|(_, r)| r).collect();
    let least = frame.cut.ease / 2.0 + TORSO_MARGIN;
    let floor = floor(frame);
    let right = sleeves.arm.mirrored();
    let in_arm = |p: DVec3| sleeves.arm.inside(p).max(right.inside(p));
    let arm_room = frame.cut.ease / 2.0 - TORSO_MARGIN;
    for (k, p) in mesh.geometry.positions.iter_mut().enumerate() {
        let q = p.as_dvec3();
        if q.y >= floor {
            continue;
        }
        let inside = lining.iter().any(|r| r.contains(&k));
        let need = if inside { least } else { least + frame.cut.thickness };
        let moved = frame.column.lift(q, need).map_err(|e| ctx.fail(e.to_string()))?;
        // Where the arm hangs close against the side, the lift is cut short before it carries the vertex further into
        // the arm (with the sleeve's ease) than half the ease, or than it already was.
        let allowed = in_arm(q).max(arm_room);
        let mut t = 1.0;
        if in_arm(moved) > allowed {
            let (mut lo, mut hi) = (0.0, 1.0);
            for _ in 0..LIFT_STEPS {
                let mid = f64::midpoint(lo, hi);
                if in_arm(q.lerp(moved, mid)) > allowed { hi = mid } else { lo = mid }
            }
            t = lo;
        }
        let mut at = q.lerp(moved, t);
        if t < 1.0 {
            // Caught between the side and the arm: slide out of the crease, along it, where both fall away.
            let torso = |p: DVec3| {
                frame.column.ring(p.y, 0.0).map_or(0.0, |ring| {
                    let d = p - ring.centre;
                    need - ring.along_ray(DVec2::new(d.dot(ring.across), d.dot(ring.through))).1
                })
            };
            let arm = |p: DVec3| in_arm(p) - allowed;
            for _ in 0..CREASE_STEPS {
                if torso(at) <= 0.0 && arm(at) <= 0.0 {
                    break;
                }
                let away = |f: &dyn Fn(DVec3) -> f64| {
                    let dx = f(at + DVec3::X * GRADIENT_STEP) - f(at - DVec3::X * GRADIENT_STEP);
                    let dz = f(at + DVec3::Z * GRADIENT_STEP) - f(at - DVec3::Z * GRADIENT_STEP);
                    -DVec3::new(dx, 0.0, dz).normalize_or_zero()
                };
                let Some(dir) = (away(&torso) + away(&arm)).try_normalize() else { break };
                at += dir * CREASE_STEP;
            }
        }
        *p = at.as_vec3();
    }
    Ok(())
}
