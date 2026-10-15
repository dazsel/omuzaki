//! Medallion clasps: a stud on the surface, a bail, a ring with three jewelled beads and a collar bead, and a drop or
//! a small pearled cross under it (#189's reference, A, B and D: the robe's clasp, as parameters).
//!
//! Every piece is one of the prototype's primitives placed in the clasp's frame at its anchor: `y` up (the world's, which
//! is the prototype's surface up where the cloth is upright), `x = up x normal` across, `z = x x up` out. At [`CLASP_SIZE`] the clasp is the prototype's; any other size scales all
//! of it. The hanging pieces stand off the surface as the prototype's do, and further where the garment would
//! otherwise reach them, so the clasp never sinks into a garment that bulges below it.

use omuzaki_char_api::JointRef;
use omuzaki_core::glam::{DVec2, DVec3};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    Basis, Bevel, Finish, TrimError, checked, cuboid, cylinder, extrude, feature, position, robe_segments, sphere,
    torus,
};
use crate::kit::curve::{TAU, fraction};
use crate::kit::grid::MeshBuilder;
use crate::outfit::loft::Surface;

/// The ring: ring radius and wire radius, metres.
pub const RING: (f64, f64) = (0.0145, 0.0034);
/// The ring's segments round its wire and round itself.
pub const RING_SEGMENTS: (usize, usize) = (8, 32);
/// The clasp's size, metres: the ring's outer diameter.
pub const CLASP_SIZE: f64 = 2.0 * (RING.0 + RING.1);
/// How far the ring's top hangs below the anchor, and how far out from the cloth its centre is, metres.
pub const RING_HANG: (f64, f64) = (0.0105, 0.0085 + 0.0025);
/// A jewelled bead on the ring: radius, its squash out of the ring's plane, its segments (as the reference gives them).
pub const BEAD: (f64, f64, (usize, usize)) = (0.0050, 0.8, (14, 10));
/// The collar bead at the ring's top: radius, its scale, its segments.
pub const COLLAR: (f64, [f64; 3], (usize, usize)) = (0.0046, [1.1, 0.9, 0.9], (12, 8));
/// The drop's bead: radius, stretch down, segments, and how far below the ring it hangs.
pub const DROP: (f64, f64, (usize, usize), f64) = (0.0034, 1.3, (10, 8), 0.0085);
/// The drop's point: radius, height, segments and how far below the ring it hangs.
pub const POINT: (f64, f64, usize, f64) = (0.0026, 0.0075, 8, 0.0155);
/// The bail: ring and wire radii, segments, and its centre's height above the ring's top (as a share of the ring's
/// hang) and out.
pub const BAIL: ((f64, f64), (usize, usize), f64, f64) = ((0.0046, 0.0016), (6, 20), 0.55, 0.0012);
/// The stud: radius, flattening, segments, and how far out from the cloth.
pub const STUD: (f64, f64, (usize, usize), f64) = (0.0066, 0.55, (14, 10), 0.0028);
/// The stud's beaded rim: ring and wire radii, segments, and how far back of the stud.
pub const STUD_RIM: ((f64, f64), (usize, usize), f64) = ((0.0058, 0.0013), (6, 22), 0.0004);

/// What hangs under a clasp's ring.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaspStyle {
    /// A bead and a point (the robe's clasp).
    #[default]
    Medallion,
    /// The small pearled cross.
    Cross,
}

/// A medallion clasp at one point of the surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Clasp {
    /// Where its stud sits, `[t, phi]` on the surface.
    pub at: [f64; 2],
    /// What hangs under the ring.
    #[serde(default)]
    pub style: ClaspStyle,
    /// The ring's outer diameter, metres ([`CLASP_SIZE`] is the prototype's); every piece is in proportion.
    #[serde(default = "size")]
    pub size: f64,
    /// The colour of the three beads on the ring; `None` makes them metal like the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jewel: Option<ColourRef>,
    /// What the metal pieces are made of (gold unless set).
    #[serde(default)]
    pub finish: Finish,
}

fn size() -> f64 {
    CLASP_SIZE
}

impl Validate for Clasp {
    fn check(&self, c: &mut Check) {
        position(c, "at", self.at);
        feature(c, "size", self.size);
        c.nested("finish", &self.finish);
    }
}

/// A clasp's pieces, by material.
pub struct ClaspPieces {
    /// The stud, bail, ring, collar bead and drop or cross.
    pub metal: MeshBuilder,
    /// The three beads on the ring (empty when they are metal: they are then in `metal`).
    pub jewel: MeshBuilder,
}

/// The clasp's frame at the anchor `foot` with the surface's `up` and `normal` there: `y` up, `x = up x normal`,
/// `z = x x y` (the prototype's, with no tilt).
pub fn frame(foot: DVec3, up: DVec3, normal: DVec3) -> Basis {
    let x = up.cross(normal).normalize_or_zero();
    let z = x.cross(up).normalize_or_zero();
    Basis { origin: foot, x, y: up, z }
}

/// The clasp of `style` at `scale` of the prototype's: its stud in the surface's frame `on` and its hanging pieces in
/// the level frame `at` (both from [`frame`], at the same anchor; one frame on upright cloth), the hanging pieces a
/// further `standoff` out. The jewelled beads go to `jewel` when given, else to `metal`.
///
/// # Errors
/// The kit's errors for a degenerate piece.
pub fn pieces(
    metal: &mut MeshBuilder,
    mut jewel: Option<&mut MeshBuilder>,
    (on, at): (&Basis, &Basis),
    style: ClaspStyle,
    (scale, standoff): (f64, f64),
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    let k = scale;
    let whole = TAU / 2.0;
    let (ring_big, ring_r) = (RING.0 * k, RING.1 * k);
    let centre = at.at(DVec3::new(0.0, -(ring_big + RING_HANG.0 * k), RING_HANG.1 * k + standoff));
    torus(metal, &centre, (ring_big, ring_r), RING_SEGMENTS, DVec3::ONE, skin)?;
    let (bead_r, bead_squash, (ws, hs)) = BEAD;
    for a in [0.0, std::f64::consts::PI, -std::f64::consts::FRAC_PI_2] {
        let basis = centre.at(DVec3::new(a.cos() * ring_big, a.sin() * ring_big, 0.0));
        let target = match jewel.as_deref_mut() {
            Some(j) => j,
            None => &mut *metal,
        };
        sphere(target, &basis, bead_r * k, DVec3::new(1.0, 1.0, bead_squash), robe_segments(ws, hs), whole, skin)?;
    }
    let (collar_r, collar_scale, (ws, hs)) = COLLAR;
    let collar = centre.at(DVec3::new(0.0, ring_big, 0.0));
    sphere(metal, &collar, collar_r * k, collar_scale.into(), robe_segments(ws, hs), whole, skin)?;
    match style {
        ClaspStyle::Medallion => {
            let (drop_r, stretch, (ws, hs), below) = DROP;
            let drop = centre.at(DVec3::new(0.0, -ring_big - below * k, 0.0));
            sphere(metal, &drop, drop_r * k, DVec3::new(1.0, stretch, 1.0), robe_segments(ws, hs), whole, skin)?;
            let (point_r, point_h, point_segments, point_below) = POINT;
            // Turned half a turn about x, so its apex points down.
            let tip = centre.at(DVec3::new(0.0, -ring_big - point_below * k, 0.0));
            let tip = Basis { y: -tip.y, z: -tip.z, ..tip };
            cylinder(metal, &tip, (0.0, point_r * k), point_h * k, point_segments, skin)?;
        }
        ClaspStyle::Cross => pendant_cross(metal, &centre.at(DVec3::new(0.0, -ring_big, 0.0)), k, skin)?,
    }
    let ((bail_big, bail_r), bail_segments, bail_up, bail_out) = BAIL;
    let bail = centre.at(DVec3::new(0.0, ring_big + RING_HANG.0 * k * bail_up, bail_out * k)).turned_y(TAU / 4.0);
    torus(metal, &bail, (bail_big * k, bail_r * k), bail_segments, DVec3::ONE, skin)?;
    let (stud_r, flat, (ws, hs), stud_out) = STUD;
    let stud = on.at(DVec3::new(0.0, 0.0, stud_out * k));
    sphere(metal, &stud, stud_r * k, DVec3::new(1.0, 1.0, flat), robe_segments(ws, hs), whole, skin)?;
    let ((rim_big, rim_r), rim_segments, rim_back) = STUD_RIM;
    let rim = stud.at(DVec3::new(0.0, 0.0, -rim_back * k));
    torus(metal, &rim, (rim_big * k, rim_r * k), rim_segments, DVec3::ONE, skin)
}

/// The pendant cross's arms: vertical length, and the down arm's share of it; horizontal length (metres).
pub const CROSS_ARMS: (f64, f64, f64) = (0.0165, 0.86, 0.0115);
/// An arm's outline: half-width at its root, length of the narrow root, half-width at its tip upright and across.
pub const ARM_OUTLINE: (f64, f64, f64, f64) = (0.0016, 0.006, 0.0042, 0.0040);
/// An arm's extrusion depth, metres.
pub const ARM_DEPTH: f64 = 0.0022;
/// An arm's bevel: thickness, size, segments.
pub const ARM_BEVEL: (f64, f64, usize) = (0.0007, 0.0007, 2);
/// The cross's centre box: side and depth.
pub const CROSS_BOX: (f64, f64) = (0.0072, 0.0022);
/// The cross's pearls: radius, segments, and how far past each arm's tip.
pub const PEARL: (f64, (usize, usize), f64) = (0.0030, (10, 8), 0.0014);
/// The centre gem: radius, flattening, segments, and how far out.
pub const GEM: (f64, f64, (usize, usize), f64) = (0.0036, 0.6, (12, 8), 0.0016);
/// The cross's bail: ring and wire radii, segments, scale, and its centre's drop below the cross's top and how far out.
pub type CrossBail = ((f64, f64), (usize, usize), [f64; 3], f64, f64);
/// The cross's bail (see [`CrossBail`]).
pub const CROSS_BAIL: CrossBail = ((0.0030, 0.0011), (5, 14), [1.3, 1.5, 1.0], 0.0008, 0.002);
/// The bead above the top arm: radius, segments, and how far above the top arm's tip.
pub const TOP_BEAD: (f64, (usize, usize), f64) = (0.0034, (10, 8), 0.0045);
/// How far below the hanging point the cross's top is, how far below that its top arm's tip, and how far out the cross
/// hangs (metres).
pub const CROSS_HANG: (f64, f64, f64) = (0.0055, 0.004, 0.004);
/// How far each arm is pushed back to centre its thickness, metres.
const ARM_BACK: f64 = 0.0011;
/// How far an arm's root reaches past the centre, inside the centre box, metres. The prototype's arms all start at the
/// centre, so opposite arms share their root edge; reaching a little further keeps each arm its own closed shell
/// without changing anything outside the box.
const ARM_ROOT_INSET: f64 = 0.0005;

/// Adds the pendant cross (#189's reference, D) hanging from `at`'s origin (a chain's middle, or under a clasp's ring),
/// in its frame (`y` up, `z` out), at `scale` of the prototype's.
///
/// # Errors
/// The kit's errors for a degenerate piece.
pub fn pendant_cross(
    builder: &mut MeshBuilder,
    at: &Basis,
    scale: f64,
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    let k = scale;
    let whole = TAU / 2.0;
    let (lv, down_share, lh) = CROSS_ARMS;
    let (hang_top, hang_gap, hang_out) = CROSS_HANG;
    let top = -hang_top * k;
    let centre = at.at(DVec3::new(0.0, top - hang_gap * k - lv * k, hang_out * k));
    let (ww, root, tip_v, tip_h) = ARM_OUTLINE;
    let (bt, bs, segments) = ARM_BEVEL;
    let bevel = Bevel { thickness: bt * k, size: bs * k, segments };
    for (length, angle, tip) in [
        (lv, 0.0, tip_v),
        (lv * down_share, std::f64::consts::PI, tip_v),
        (lh, -TAU / 4.0, tip_h),
        (lh, TAU / 4.0, tip_h),
    ] {
        // Counter-clockwise: the narrow root, then the flare out to the tip.
        let outline = [
            DVec2::new(-ww, -ARM_ROOT_INSET),
            DVec2::new(ww, -ARM_ROOT_INSET),
            DVec2::new(ww, root),
            DVec2::new(tip, length),
            DVec2::new(-tip, length),
            DVec2::new(-ww, root),
        ]
        .map(|p| p * k);
        let arm = centre.at(DVec3::new(0.0, 0.0, -ARM_BACK * k)).turned_z(angle);
        extrude(builder, &arm, &outline, &[&[0, 1, 2, 5], &[5, 2, 3, 4]], ARM_DEPTH * k, bevel, skin)?;
    }
    let (side, depth) = CROSS_BOX;
    cuboid(builder, &centre, DVec3::new(side, side, depth) * k, skin)?;
    let (pearl_r, (ws, hs), past) = PEARL;
    for (x, y) in [(0.0, lv + past), (0.0, -(down_share * lv + past)), (lh + past, 0.0), (-(lh + past), 0.0)] {
        let pearl = centre.at(DVec3::new(x * k, y * k, 0.0));
        sphere(builder, &pearl, pearl_r * k, DVec3::ONE, robe_segments(ws, hs), whole, skin)?;
    }
    let (gem_r, flat, (ws, hs), gem_out) = GEM;
    let gem = centre.at(DVec3::new(0.0, 0.0, gem_out * k));
    sphere(builder, &gem, gem_r * k, DVec3::new(1.0, 1.0, flat), robe_segments(ws, hs), whole, skin)?;
    let ((bail_big, bail_r), bail_segments, bail_scale, bail_drop, bail_out) = CROSS_BAIL;
    let bail = at.at(DVec3::new(0.0, top - bail_drop * k, bail_out * k)).turned_y(TAU / 4.0);
    torus(builder, &bail, (bail_big * k, bail_r * k), bail_segments, bail_scale.into(), skin)?;
    let (bead_r, (ws, hs), above) = TOP_BEAD;
    let bead = centre.at(DVec3::new(0.0, (lv + above) * k, 0.0));
    sphere(builder, &bead, bead_r * k, DVec3::ONE, robe_segments(ws, hs), whole, skin)
}

/// How far along `at`'s `z` the points `probes` (local to `at`, each with how far its piece reaches back toward the
/// surface) must move for every one of them to keep its reach clear of `surface`: 0 when they already do.
///
/// # Errors
/// The surface's errors.
pub fn clearance(surface: &Surface, at: &Basis, probes: &[(DVec3, f64)]) -> Result<f64, TrimError> {
    let mut extra = 0.0;
    for _ in 0..STANDOFF_PASSES {
        let mut deficit = 0.0_f64;
        for (local, reach) in probes {
            let p = at.place(*local + DVec3::new(0.0, 0.0, extra));
            if let Some(projection) = surface.project(p)? {
                deficit = deficit.max(reach - projection.outside);
            }
        }
        if deficit <= SETTLED {
            break;
        }
        extra += deficit;
    }
    Ok(extra)
}

/// Points of the pendant cross hung from a frame's origin at `scale`, local to that frame, each with how far its piece
/// reaches back: down its upright and out along its arms.
pub fn cross_probes(scale: f64) -> Vec<(DVec3, f64)> {
    let k = scale;
    let (lv, down_share, lh) = CROSS_ARMS;
    let (hang_top, hang_gap, hang_out) = CROSS_HANG;
    let back = (ARM_DEPTH / 2.0 + ARM_BEVEL.0).max(PEARL.0) * k;
    let centre = DVec3::new(0.0, -(hang_top + hang_gap + lv) * k, hang_out * k);
    let reach = |x: f64, y: f64| (centre + DVec3::new(x, y, 0.0) * k, back);
    let mut probes: Vec<(DVec3, f64)> = (0..=8)
        .map(|i| reach(0.0, lv + TOP_BEAD.2 - (lv * (1.0 + down_share) + TOP_BEAD.2 + PEARL.2) * fraction(i, 8)))
        .collect();
    probes.extend((0..=4).map(|i| reach((lh + PEARL.2) * (2.0 * fraction(i, 4) - 1.0), 0.0)));
    probes
}

/// The clasp `spec` describes on `surface`: its frame at the anchor, the hanging pieces pushed out further only where
/// the garment would reach them.
///
/// # Errors
/// [`TrimError::Invalid`] for a spec that fails validation; the surface's and the kit's errors.
pub fn build(surface: &Surface, spec: &Clasp) -> Result<ClaspPieces, TrimError> {
    checked(spec)?;
    let anchor = surface.at(spec.at[0], spec.at[1])?;
    // Up is the world's, so the pieces hang straight down and stand off level: the prototype's surface up wherever the
    // cloth is upright, and on a slope (a bust) they hang down rather than along it into the garment. A surface
    // facing almost straight up or down has no level out, so there the clasp follows the surface.
    let up = if anchor.normal.y.abs() < LEVEL_LIMIT { DVec3::Y } else { anchor.along };
    let basis = frame(anchor.point, up, anchor.normal);
    let on = frame(anchor.point, anchor.along, anchor.normal);
    let k = spec.size / CLASP_SIZE;
    let skin = surface.weights().at(spec.at[0]);
    let standoff = standoff(surface, &basis, spec.style, k)?;
    let mut metal = MeshBuilder::new();
    let mut jewel = MeshBuilder::new();
    let beads = spec.jewel.as_ref().map(|_| &mut jewel);
    pieces(&mut metal, beads, (&on, &basis), spec.style, (k, standoff), &skin)?;
    Ok(ClaspPieces { metal, jewel })
}

/// How much further out than the prototype's the hanging pieces must stand for every one of them to keep its own
/// thickness clear of `surface`.
fn standoff(surface: &Surface, at: &Basis, style: ClaspStyle, k: f64) -> Result<f64, TrimError> {
    let (ring_big, ring_r) = (RING.0 * k, RING.1 * k);
    let centre = DVec3::new(0.0, -(ring_big + RING_HANG.0 * k), RING_HANG.1 * k);
    // Points of the hanging pieces in the clasp's frame, each with how far its piece reaches back toward the surface.
    let mut probes: Vec<(DVec3, f64)> = (0..12)
        .map(|i| {
            let a = TAU * fraction(i, 12);
            (centre + DVec3::new(a.cos(), a.sin(), 0.0) * ring_big, ring_r.max(BEAD.0 * BEAD.1 * k))
        })
        .collect();
    // The collar bead and the bail above the ring.
    probes.push((centre + DVec3::new(0.0, ring_big, 0.0), COLLAR.0 * COLLAR.1[0] * k));
    let ((bail_big, bail_r), _, bail_up, bail_out) = BAIL;
    let bail = centre + DVec3::new(0.0, ring_big + RING_HANG.0 * k * bail_up, bail_out * k);
    probes.push((bail, (bail_big + bail_r) * k));
    match style {
        ClaspStyle::Medallion => {
            let lowest = POINT.3 + POINT.1 / 2.0;
            probes.extend((0..=4).map(|i| {
                let down = ring_big + lowest * k * fraction(i, 4);
                (centre - DVec3::new(0.0, down, 0.0), DROP.0 * DROP.1 * k)
            }));
        }
        ClaspStyle::Cross => {
            let hung = centre - DVec3::new(0.0, ring_big, 0.0);
            probes.extend(cross_probes(k).into_iter().map(|(p, reach)| (hung + p, reach)));
        }
    }
    clearance(surface, at, &probes)
}

/// The most passes that push hanging pieces out: each moves them by the worst shortfall, until none is left.
const STANDOFF_PASSES: usize = 16;
/// A shortfall this small is none, metres.
const SETTLED: f64 = 1e-6;
/// Above this share of straight up or down, a surface has no level direction to hang a clasp from.
const LEVEL_LIMIT: f64 = 0.95;
