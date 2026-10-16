//! Hand styles for the `body` part: mitten and simple (`body.js` `handFrame`, `handBody`, `handThumb`, `buildHand`).
//!
//! Both styles are one palm-and-finger block (lofted elliptical rings along a slightly curled axis that closes into a
//! rounded cap) plus a swept thumb; they differ only in the block's surface ([`mitten`]: smooth, with the finger-hint
//! decals; [`simple`]: gap grooves between four fused fingers) and the thumb's rest angle. The left hand is built, and
//! the right hand is its mirror image ([`MeshBuilder::mirrored`]), so the two cannot differ.
//!
//! **The prototype's hand, then two pieces of the kit's own.** A hand's mesh starts with the prototype's block (its 21
//! rows) and thumb (11 rings of 18 segments), vertex for vertex: on soft-sd at the default numbers they are the
//! prototype's. Two short pieces follow, which the prototype does not need because a cuff hides its wrist and its thumb
//! root:
//! - the **wrist sleeve**, one ring [`WRIST_OVERLAP`] behind the wrist joint (narrowed to fit inside the forearm) joined
//!   to the block's first row, so a bare arm's rounded end covers the hand's opening;
//! - the **thumb stem**, one ring back along the thumb's line and seated wholly inside the palm, joined to the thumb's
//!   root ring, so the thumb's open end lies inside the palm whatever the palm and thumb numbers.
//!
//! Each is its own grid, so the prototype's rows keep their own normals.
//!
//! The hand sits on the numbers of the template the layout was built from ([`BodyLayout::hand_table`]): the origin, the
//! knuckle row, the finger lines, the thumb's root and direction and the curl, placed on the layout's wrist and forearm
//! and scaled by `Proportions::hand_size` and the [`HandParams`](omuzaki_char_api::HandParams) palm and thumb numbers exactly as
//! [`crate::template::layout`] places the finger and thumb joints (the palm is scaled, then the thumb turns about its
//! metacarpal joint, the free thumb's own length is not scaled by the palm). The block's measurements (its widths, its
//! thickness, the thumb's radius) are the prototype's, in the prototype's own metres, and grow with the template's hand
//! by the ratio of its reach (knuckle row to middle fingertip) to the prototype's. The finger numbers of [`HandParams`](omuzaki_char_api::HandParams)
//! shape the five-finger hands only, so a mitten ignores them (the API contract).
//!
//! Skin: the palm on the hand bone, its hidden wrist end fading up to half into the forearm, the finger block on the
//! four proximal finger bones (by lateral position, so every hand style shares one skeleton), the thumb on its three
//! bones.

#![allow(clippy::cast_possible_truncation)] // weights and tones are in 0..=1; f32 is the mesh's precision

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyParams, BodyRegion, HandStyle, JointRef, MaterialName, PartMesh};
use omuzaki_core::glam::DVec3;

use crate::kit::KitError;
use crate::kit::curve::{Pchip, TAU, clamp, fraction, lerp, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::template::layout::{BodyLayout, HandFrame, HandTable};

pub mod five;
pub mod mitten;
pub mod simple;

/// Hand length along the forearm axis, from the hand's origin to the tip of the mitt, in the prototype's metres
/// (`HAND_L`). A template's hand is this much longer by its reach over [`HAND_LENGTH`] ([`Hand::scale`]).
const HAND_LENGTH: f64 = 0.050;
/// Where the rounded end cap begins (`capStart`), and where the section curves stop: the cap's rows keep the size the
/// curves have there and close it as a quarter ellipse.
const CAP_START: f64 = 0.040;
const LAST_STATION: f64 = 0.042;
/// The block's rows before the cap: from `FIRST_ROW` every `ROW_STEP` while below [`CAP_START`], the step added up in
/// f64 as the prototype's loop does. The sum stays a hair below [`CAP_START`] at the thirteenth row, so there are 13
/// rows, and the cap's first row all but repeats the last of them; the prototype's mesh has both.
const FIRST_ROW: f64 = -0.014;
const ROW_STEP: f64 = 0.0045;
/// The cap's rows, from [`CAP_START`] to [`HAND_LENGTH`] spaced as a quarter sine.
const CAP_ROWS: usize = 8;
/// How far behind the wrist joint the wrist sleeve's open ring lies, so the arm's rounded end cap (which reaches only
/// about half a ring's width past the wrist) covers the hand's opening at every hand size. The prototype's first row
/// lies 8 mm past the wrist, where its cuff hides the opening.
const WRIST_OVERLAP: f64 = 0.004;
/// How much of the forearm's narrowest radius the open wrist ring may fill, so the ring lies inside the arm's surface
/// (the rest is margin for the arm's own offset from the hand's axis).
const WRIST_ROOM: f64 = 0.85;
/// Semi-axes of the section along the thumb side (width, `rt`) and the palm normal (thickness, `rn`), by distance from
/// the origin; held at the end values outside the knots, as the prototype's curves are.
const WIDTH: [(f64, f64); 6] =
    [(-0.014, 0.0112), (0.0, 0.0124), (0.010, 0.0148), (0.022, 0.0168), (0.034, 0.0172), (0.042, 0.0160)];
const THICKNESS: [(f64, f64); 6] =
    [(-0.014, 0.0098), (0.0, 0.0106), (0.010, 0.0122), (0.022, 0.0132), (0.034, 0.0130), (0.042, 0.0122)];
/// The palm side is flatter, the back of the hand plumper.
const PALM_FLATTEN: f64 = 0.9;
const BACK_PLUMP: f64 = 1.05;
/// The wrist end of the block, which a cuff hides, fades up to half into the forearm bone between these stations.
const WRIST_BLEND: (f64, f64) = (-0.004, -0.014);
/// Skin tones, linear RGB with opaque alpha, as the prototype has them: the base, and the warmer tint towards the
/// finger tips (the tip ramp, over `u`) and the thumb's tip. The renderer multiplies the material's albedo by the vertex
/// colour, so the mesh carries them relative to the base ([`tone`]): white at the wrist, where the hand meets the bare
/// arm, and `TIP_TONE / BASE_TONE` at the tips.
const BASE_TONE: [f32; 4] = [1.0, 0.93, 0.91, 1.0];
const TIP_TONE: [f32; 4] = [1.0, 0.86, 0.83, 1.0];
const TIP_RAMP: (f64, f64) = (0.026, 0.050);
const THUMB_TIP_TONE: [f32; 4] = [1.0, 0.87, 0.84, 1.0];

/// The thumb's rings, from root to tip (dense at both ends so the root blends in and the tip rounds), and their size.
const THUMB_T: [f64; 11] = [0.0, 0.1, 0.22, 0.36, 0.5, 0.64, 0.76, 0.86, 0.93, 0.975, 1.0];
pub(super) const THUMB_SEGMENTS: usize = 18;
/// The thumb curls towards the palm by `BEND_N * t^2` and towards the thumb side by `BEND_T * t`.
const THUMB_BEND: (f64, f64) = (0.0045, 0.0015);
const THUMB_RADIUS: f64 = 0.0083;
/// How much of the palm's half-width and half-thickness the thumb's open stem ring may use.
const STEM_ROOM: f64 = 0.9;
const THUMB_TAPER: f64 = 0.16;
/// Where along the thumb its tip cap begins (a quarter ellipse), and how much narrower it is across than through.
const THUMB_CAP: f64 = 0.84;
const THUMB_ASPECT: f64 = 0.98;
/// The thumb's tint ramp over its ring index, as a fraction of the last.
const THUMB_TIP_RAMP: (f64, f64) = (0.4, 1.0);

/// The thumb's hand over between its bones, along its rings (`t`): the metacarpal up to the first, the distal from the
/// second.
const THUMB_METACARPAL: (f64, f64) = (0.30, 0.05);
const THUMB_DISTAL: (f64, f64) = (0.50, 0.85);

/// The left hand's frame and size: where its parts go.
struct Hand {
    /// The template's hand: where the origin, the knuckle row, the fingers and the thumb lie.
    table: HandTable,
    /// How much longer the template's hand is than the prototype's (its reach over [`HAND_LENGTH`]), which the block's
    /// own measurements grow by.
    scale: f64,
    /// The wrist joint.
    wrist: DVec3,
    /// Along the forearm, towards the thumb side, out of the palm (a right-handed frame).
    frame: HandFrame,
    size: f64,
    palm_length: f64,
    palm_width: f64,
    thumb_length: f64,
    spread_deg: f64,
    /// How much the wrist sleeve's open ring is narrowed from the block's first row so it lies inside the forearm,
    /// `0..=1` ([`WRIST_ROOM`]).
    taper: f64,
}

impl Hand {
    /// The hand of the layout's left arm, for `params`. Out-of-range or non-finite numbers are clamped, as the layout
    /// clamps them when it places the joints.
    fn new(params: &BodyParams, layout: &BodyLayout) -> Result<Self, KitError> {
        let joint = |bone: HumanoidBone| {
            layout
                .joint64(bone)
                .filter(|p| p.is_finite())
                .ok_or(KitError::Degenerate { what: "a joint the hand hangs on" })
        };
        let (wrist, elbow) = (joint(HumanoidBone::LeftHand)?, joint(HumanoidBone::LeftLowerArm)?);
        let table = *layout.hand_table().ok_or(KitError::Degenerate { what: "the template's hand table" })?;
        let scale = (table.knuckle + table.finger_length[1]) / HAND_LENGTH;
        if !(scale.is_finite() && scale > 0.0) {
            return Err(KitError::Degenerate { what: "the template's hand reach" });
        }
        let frame = HandFrame::of(wrist - elbow, table.palm_hint, table.thumb_hint);
        let slider = |v: f64| if v.is_finite() { clamp(v, 0.5, 2.0) } else { 1.0 };
        let spread = params.hand.thumb_spread_deg;
        let (size, palm_length, palm_width) =
            (slider(params.proportions.hand_size), slider(params.hand.palm_length), slider(params.hand.palm_width));
        // The open wrist ring lies `WRIST_OVERLAP` behind the wrist, scaled as the hand is; it must fit inside the arm's
        // section there, which the block's own wrist size (the curves' first knot) does not at any hand but a small one.
        let arm = layout.region(BodyRegion::ArmL).ok_or(KitError::Degenerate { what: "the forearm the hand joins" })?;
        let back = WRIST_OVERLAP * scale * palm_length * size;
        let room = WRIST_ROOM * arm.section(1.0 - back / arm.length().max(f64::EPSILON)).radii.min_element();
        let (across, through) =
            (WIDTH[0].1 * scale * palm_width * size, THICKNESS[0].1 * PALM_FLATTEN.max(BACK_PLUMP) * scale * size);
        let taper = clamp((room / across).min(room / through), 0.0, 1.0);
        Ok(Self {
            table,
            scale,
            wrist,
            frame,
            size,
            palm_length,
            palm_width,
            thumb_length: slider(params.hand.thumb_length),
            spread_deg: if spread.is_finite() { clamp(spread, -30.0, 30.0) } else { 0.0 },
            taper,
        })
    }

    /// The point `c` of the hand's own numbers (along the forearm, towards the thumb, out of the palm; from the wrist),
    /// in the world: scaled by the hand size about the wrist, as the layout places the joints.
    fn world(&self, c: DVec3) -> DVec3 {
        let f = &self.frame;
        self.wrist + (f.axis * c.x + f.thumb * c.y + f.palm * c.z) * self.size
    }

    /// The point `u` along the hand axis from its origin, `lat` towards the thumb and `nrm` out of the palm, in the
    /// prototype's metres. They grow by [`Self::scale`] to the template's own; the palm (up to the knuckles) is then
    /// stretched by the palm length and the width by the palm width, as the layout places the finger joints that skin
    /// it.
    fn place(&self, u: f64, lat: f64, nrm: f64) -> DVec3 {
        let (u, lat, nrm) = (u * self.scale, lat * self.scale, nrm * self.scale);
        let t = &self.table;
        let along = (t.shift + u.min(t.knuckle)) * self.palm_length + (u - t.knuckle).max(0.0);
        self.world(DVec3::new(along, lat * self.palm_width, nrm))
    }

    /// The fingertips' curl towards the palm at `u` (prototype's metres, as [`Self::place`] takes it).
    fn curl(&self, u: f64) -> f64 {
        (self.table.curl)(u * self.scale) / self.scale
    }

    /// Where the knuckle row lies, in prototype's metres.
    fn knuckle(&self) -> f64 {
        self.table.knuckle / self.scale
    }

    /// Where the wrist sleeve's open ring lies: [`WRIST_OVERLAP`] behind the wrist joint (the origin lies `shift` past
    /// it).
    fn start(&self) -> f64 {
        -(self.table.shift / self.scale) - WRIST_OVERLAP
    }
}

/// How a style shapes the block's surface and what it adds.
pub(super) struct Style {
    /// Segments once round a ring of the block.
    pub(super) around: usize,
    /// Added to the thumb's spread, degrees (a simple hand's thumb is splayed).
    pub(super) splay_deg: f64,
    /// Scales a ring point's lateral position and height from `(u, lateral)`: the finger gap grooves. `u` is measured
    /// as if the knuckle row were the prototype's, [`PROTOTYPE_KNUCKLE`].
    pub(super) groove: fn(f64, f64) -> f64,
}

/// Where the prototype's knuckle row lies; the finger-hint lines and the grooves are drawn from it.
pub(super) const PROTOTYPE_KNUCKLE: f64 = 0.030;

fn bone(bone: HumanoidBone) -> JointRef {
    JointRef::Humanoid(bone)
}

/// The skin at distance `u` from the origin, `lat` towards the thumb (prototype's metres): the hand bone with the wrist
/// end fading up to half into the forearm; past the knuckle row, the nearest finger bones.
fn palm_skin(hand: &Hand, u: f64, lat: f64) -> Vec<(JointRef, f32)> {
    const FINGERS: [HumanoidBone; 4] = [
        HumanoidBone::LeftIndexProximal,
        HumanoidBone::LeftMiddleProximal,
        HumanoidBone::LeftRingProximal,
        HumanoidBone::LeftLittleProximal,
    ];
    let (s, t) = (hand.scale, &hand.table);
    let (u, lat) = (u * s, lat * s);
    let forearm = 0.5 * sstep(WRIST_BLEND.0 * s, WRIST_BLEND.1 * s, u);
    let fingers = sstep(t.knuckle - 0.004 * s, t.knuckle + 0.004 * s, u);
    let palm = 1.0 - fingers;
    let mut blend = vec![
        (bone(HumanoidBone::LeftHand), (palm * (1.0 - forearm)) as f32),
        (bone(HumanoidBone::LeftLowerArm), (palm * forearm) as f32),
    ];
    // A tent per finger over the finger lines' spacing: the four tents sum to 1 between the outer fingers.
    let spacing = t.finger_across[0] - t.finger_across[1];
    let lat = clamp(lat, t.finger_across[3], t.finger_across[0]);
    for (finger, across) in FINGERS.into_iter().zip(t.finger_across) {
        blend.push((bone(finger), (fingers * (1.0 - (lat - across).abs() / spacing).max(0.0)) as f32));
    }
    blend
}

/// The skin of the thumb's ring `t` (0 at the root, 1 at the tip).
fn thumb_skin(t: f64) -> Vec<(JointRef, f32)> {
    let metacarpal = sstep(THUMB_METACARPAL.0, THUMB_METACARPAL.1, t);
    let distal = sstep(THUMB_DISTAL.0, THUMB_DISTAL.1, t);
    vec![
        (bone(HumanoidBone::LeftThumbMetacarpal), metacarpal as f32),
        (bone(HumanoidBone::LeftThumbProximal), (1.0 - metacarpal - distal).max(0.0) as f32),
        (bone(HumanoidBone::LeftThumbDistal), distal as f32),
    ]
}

/// The vertex colour `amount` of the way from the wrist to the tip tone `tip`: white at 0, `tip` over the base at 1.
fn tone(tip: [f32; 4], amount: f64) -> [f32; 4] {
    std::array::from_fn(|k| lerp(1.0, f64::from(tip[k] / BASE_TONE[k]), amount) as f32)
}

/// `n` as a float.
fn count(n: usize) -> f64 {
    fraction(n, 1)
}

/// The palm-and-finger block's rows: the stations (`u`) they lie at, and the section curves.
struct Block {
    stations: Vec<f64>,
    width: Pchip,
    thickness: Pchip,
}

impl Block {
    /// The section's semi-axes at `u`: closing as a quarter ellipse past the cap's start.
    fn section(&self, u: f64) -> (f64, f64) {
        let cap = if u <= CAP_START {
            1.0
        } else {
            (1.0 - ((u - CAP_START) / (HAND_LENGTH - CAP_START)).powi(2)).max(0.0).sqrt()
        };
        let u = u.min(LAST_STATION);
        (self.width.eval(u) * cap, self.thickness.eval(u) * cap)
    }

    fn new() -> Result<Self, KitError> {
        let mut stations = Vec::new();
        // The prototype's loop, summed the same way: the row count depends on how the steps round.
        let mut u = FIRST_ROW;
        while u < CAP_START {
            stations.push(u);
            u += ROW_STEP;
        }
        stations.extend((0..CAP_ROWS).map(|k| {
            CAP_START + (HAND_LENGTH - CAP_START) * (fraction(k, CAP_ROWS - 1) * std::f64::consts::FRAC_PI_2).sin()
        }));
        Ok(Self { stations, width: Pchip::new(&WIDTH)?, thickness: Pchip::new(&THICKNESS)? })
    }

    /// Point `j` of the ring at `u`, its section scaled by `fit`.
    fn point(&self, hand: &Hand, style: &Style, u: f64, fit: f64, j: usize) -> DVec3 {
        let (a, b) = self.section(u);
        let (a, b) = (a * fit, b * fit);
        let phi = fraction(j, style.around) * TAU;
        let (sn, ct) = phi.sin_cos();
        // Flatten the palm side (sn > 0), plump the back.
        let b = b * if sn > 0.0 { PALM_FLATTEN } else { BACK_PLUMP };
        let lateral = a * ct;
        let groove = (style.groove)(u + PROTOTYPE_KNUCKLE - hand.knuckle(), lateral);
        hand.place(u, lateral * groove, b * sn * groove + hand.curl(u))
    }

    /// The block's grid, one ring per station, `style.around` segments round: the prototype's rows.
    fn grid(&self, hand: &Hand, style: &Style) -> Result<Grid, KitError> {
        Grid::from_fn(self.stations.len(), style.around + 1, |i, j| self.point(hand, style, self.stations[i], 1.0, j))
    }

    /// The wrist sleeve: the open ring [`WRIST_OVERLAP`] behind the wrist joint (at the size of the block's first row,
    /// narrowed by [`Hand::taper`] to fit inside the forearm), and the block's first row again, which it joins.
    fn sleeve(&self, hand: &Hand, style: &Style) -> Result<Grid, KitError> {
        let rows = [(hand.start(), hand.taper), (self.stations[0], 1.0)];
        Grid::from_fn(rows.len(), style.around + 1, |i, j| self.point(hand, style, rows[i].0, rows[i].1, j))
    }

    /// The centre of the ring at `u`, for the interior reference of the normals.
    fn centre(hand: &Hand, u: f64) -> DVec3 {
        hand.place(u, 0.0, hand.curl(u))
    }
}

/// The thumb: the prototype's rings (`grid`, centred on `centres`) and the stem joined to its root ring (`stem`, centred
/// on `stem_centres`).
struct Thumb {
    grid: Grid,
    centres: Vec<DVec3>,
    stem: Grid,
    stem_centres: [DVec3; 2],
}

/// The thumb. Frames are rebuilt per ring from the tangent, with the palm normal projected off it, so the thumb does not
/// twist.
///
/// It is placed as the layout places the thumb joints: the metacarpal joint and the root are scaled with the palm, the
/// thumb turns about the metacarpal joint in the plane of the palm, and the free thumb (its direction, which the
/// thumb length scales) is not scaled by the palm. The root ring sits on the layout's proximal joint.
///
/// The root ring is open, and the joint can lie outside the palm (a narrow palm, a wide spread), so the thumb has a
/// stem: one more ring, one root radius back along the thumb's line, seated wholly inside the palm's section there.
/// It shrinks until it fits, and its centre moves towards the axis as far as the room left by its radius allows; a
/// ball round the centre holds the whole ring, whatever way it is turned.
fn thumb(hand: &Hand, block: &Block, splay_deg: f64) -> Result<Thumb, KitError> {
    let (t, s) = (&hand.table.thumb, hand.scale);
    let on_palm = |p: DVec3| DVec3::new((hand.table.shift + p.x) * hand.palm_length, p.y * hand.palm_width, p.z);
    let (base, root) = (on_palm(t.base), on_palm(t.root));
    let (sin, cos) = (hand.spread_deg + splay_deg).to_radians().sin_cos();
    let turn = |v: DVec3| DVec3::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos, v.z);
    let root = base + turn(root - base);
    let direction = turn(t.direction.normalize_or_zero());
    let length = t.length * hand.thumb_length;
    // Centres in the hand's own numbers (`x` along, `y` thumb side, `z` palm), then in the world.
    let bend = (THUMB_BEND.0 * s, THUMB_BEND.1 * s);
    let centres: Vec<DVec3> = THUMB_T
        .iter()
        .map(|&k| hand.world(root + direction * (length * k) + DVec3::new(0.0, bend.1 * k, bend.0 * k * k)))
        .collect();
    let radii = |k: f64| {
        let base = THUMB_RADIUS * (1.0 - THUMB_TAPER * k) * s;
        let cap =
            if k < THUMB_CAP { 1.0 } else { (1.0 - ((k - THUMB_CAP) / (1.0 - THUMB_CAP)).powi(2)).max(0.0).sqrt() };
        (base * cap * THUMB_ASPECT * hand.size, base * cap * hand.size)
    };
    // Point `j` of a ring centred on `centre`, facing along `tangent`, with semi-axes `(ra, rb)`.
    let ring = |centre: DVec3, tangent: DVec3, (ra, rb): (f64, f64), j: usize| {
        let tangent = tangent.normalize_or_zero();
        let normal = (hand.frame.palm - tangent * hand.frame.palm.dot(tangent)).normalize_or_zero();
        let binormal = tangent.cross(normal).normalize_or_zero();
        let (sn, c) = (fraction(j, THUMB_SEGMENTS) * TAU).sin_cos();
        centre + normal * (c * ra) + binormal * (sn * rb)
    };
    let last = centres.len() - 1;
    let tangent = |k: usize| match k {
        0 => centres[1] - centres[0],
        _ if k == last => centres[k] - centres[k - 1],
        _ => centres[k + 1] - centres[k - 1],
    };
    let grid =
        Grid::from_fn(centres.len(), THUMB_SEGMENTS + 1, |k, j| ring(centres[k], tangent(k), radii(THUMB_T[k]), j))?;

    // The stem's ring, behind the root along the thumb's line and seated inside the palm's section there. The ring is
    // tilted, so it reaches up to its radius along the hand either way: the room is the palm's smallest section over
    // that stretch, which (each curve rising to one peak and falling) is at one of its ends or the middle.
    let reach = THUMB_RADIUS * s;
    let behind = root - direction * reach;
    let station = |x: f64| ((x / hand.palm_length - hand.table.shift) / s).clamp(FIRST_ROW, HAND_LENGTH);
    let (half_width, half_thick) = [behind.x - reach, behind.x, behind.x + reach]
        .map(|x| block.section(station(x)))
        .into_iter()
        .fold((f64::INFINITY, f64::INFINITY), |(w, t), (a, b)| (w.min(a), t.min(b)));
    let room =
        (STEM_ROOM * half_width * s * hand.palm_width, STEM_ROOM * half_thick * s * PALM_FLATTEN.min(BACK_PLUMP));
    let fit = (room.0.min(room.1) / reach).min(1.0);
    let spare = |room: f64| (room - fit * reach).max(0.0);
    let seat = hand.world(DVec3::new(
        behind.x,
        behind.y.clamp(-spare(room.0), spare(room.0)),
        behind.z.clamp(-spare(room.1), spare(room.1)),
    ));
    let (ra, rb) = radii(0.0);
    let stem_rows = [(seat, (ra * fit, rb * fit)), (centres[0], (ra, rb))];
    // Both rings face along the thumb's root, so the stem's second ring is the root ring itself.
    let stem = Grid::from_fn(2, THUMB_SEGMENTS + 1, |k, j| ring(stem_rows[k].0, tangent(0), stem_rows[k].1, j))?;
    Ok(Thumb { grid, stem_centres: [seat, centres[0]], centres, stem })
}

/// Adds `grid`, a tube closed round its rings, to `builder`: ring `i` is centred on `centre(i)`, tinted `colour(i)` and
/// skinned `skin(i, j)` at its point `j`.
fn add_rings(
    builder: &mut MeshBuilder,
    grid: &Grid,
    centre: &dyn Fn(usize) -> DVec3,
    colour: &dyn Fn(usize) -> [f32; 4],
    skin: &dyn Fn(usize, usize) -> Vec<(JointRef, f32)>,
) -> Result<(), KitError> {
    let inside = |i: usize, _j: usize| centre(i);
    let colour = |i: usize, _j: usize, _p: DVec3| colour(i);
    let skin = |_p: DVec3, i: usize, j: usize| skin(i, j);
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        colour: Some(&colour),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    builder.add_grid(grid, &options)?;
    Ok(())
}

/// The left hand in one builder: the prototype's block and thumb (its `mb`), then the wrist sleeve and the thumb's
/// stem (see the module docs).
fn left_hand(hand: &Hand, style: &Style) -> Result<(MeshBuilder, Block), KitError> {
    let block = Block::new()?;
    let mut builder = MeshBuilder::with_colours();
    // The block's rings by station, and the thumb's by the prototype's ring index (the stem's take the root's).
    let palm_centre = |u: f64| Block::centre(hand, u);
    let palm_tone = |u: f64| tone(TIP_TONE, sstep(TIP_RAMP.0, TIP_RAMP.1, u));
    let palm_skin_at = |u: f64, j: usize| {
        let (a, _) = block.section(u);
        palm_skin(hand, u, a * (fraction(j, style.around) * TAU).cos())
    };
    let thumb_tone =
        |k: usize| tone(THUMB_TIP_TONE, sstep(THUMB_TIP_RAMP.0, THUMB_TIP_RAMP.1, fraction(k, THUMB_T.len() - 1)));
    let thumb = thumb(hand, &block, style.splay_deg)?;
    let stations = &block.stations;
    add_rings(
        &mut builder,
        &block.grid(hand, style)?,
        &|i| palm_centre(stations[i]),
        &|i| palm_tone(stations[i]),
        &|i, j| palm_skin_at(stations[i], j),
    )?;
    add_rings(&mut builder, &thumb.grid, &|i| thumb.centres[i], &thumb_tone, &|i, _| thumb_skin(THUMB_T[i]))?;
    let sleeve = [hand.start(), stations[0]];
    add_rings(
        &mut builder,
        &block.sleeve(hand, style)?,
        &|i| palm_centre(sleeve[i]),
        &|i| palm_tone(sleeve[i]),
        &|i, j| palm_skin_at(sleeve[i], j),
    )?;
    add_rings(&mut builder, &thumb.stem, &|i| thumb.stem_centres[i], &|_| thumb_tone(0), &|_, _| thumb_skin(0.0))?;
    Ok((builder, block))
}

/// The hand meshes of `params.hands` for both hands of `layout`, using `material`: for a mitten or simple hand, the
/// left hand, its mirror image, and (mitten) the finger-hint lines of each. Five-finger hands are built elsewhere, so
/// they add nothing here.
///
/// # Errors
/// [`KitError`] if a hand cannot be built.
pub fn build(params: &BodyParams, layout: &BodyLayout, material: &MaterialName) -> Result<Vec<PartMesh>, KitError> {
    let (style, decals) = match params.hands {
        HandStyle::Mitten => (mitten::STYLE, true),
        HandStyle::Simple => (simple::STYLE, false),
        HandStyle::FiveFinger => return Ok(Vec::new()),
    };
    let hand = Hand::new(params, layout)?;
    let (left, block) = left_hand(&hand, &style)?;
    let mut parts = vec![("hand-l", left)];
    if decals {
        parts.push(("hand-lines-l", mitten::lines(&hand, &block)?));
    }
    // The layout is symmetric about x = 0 and the hand is built from the left joints, so the right hand is the left
    // one mirrored (x negated, winding reversed, bones swapped), never built separately.
    let mirrored: Vec<_> =
        parts.iter().map(|(name, b)| (format!("{}-r", name.trim_end_matches("-l")), b.mirrored())).collect();
    parts
        .into_iter()
        .map(|(name, b)| (name.to_string(), b))
        .chain(mirrored)
        .map(|(name, b)| b.finish(name, material.clone()))
        .collect()
}
