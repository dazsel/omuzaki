//! Hand styles for the `body` part: mitten and simple (`body.js` `handFrame`, `handBody`, `handThumb`, `buildHand`).
//!
//! Both styles are one palm-and-finger block (lofted elliptical rings along a slightly curled axis that closes into a
//! rounded cap) plus a swept thumb; they differ only in the block's surface ([`mitten`]: smooth, with the finger-hint
//! decals; [`simple`]: gap grooves between four fused fingers) and the thumb's rest angle. The left hand is built, and
//! the right hand is its mirror image ([`MeshBuilder::mirrored`]), so the two cannot differ.
//!
//! The hand sits on the template's own numbers: the origin, the knuckle row, the finger lines, the thumb's root and
//! direction and the curl come from the soft-sd hand table (the prototype's hand), placed on the layout's wrist and
//! forearm, and scaled by `Proportions::hand_size` and the [`HandParams`] palm and thumb numbers. The finger numbers of
//! [`HandParams`] shape the five-finger hands only, so a mitten ignores them (the API contract).
//!
//! Skin: the palm on the hand bone, its hidden wrist end fading up to half into the forearm, the finger block on the
//! four proximal finger bones (by lateral position, so every hand style shares one skeleton), the thumb on its three
//! bones.

#![allow(clippy::cast_possible_truncation)] // weights and tones are in 0..=1; f32 is the mesh's precision

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyParams, HandStyle, JointRef, MaterialName, PartMesh};
use omuzaki_core::glam::DVec3;

use crate::kit::KitError;
use crate::kit::curve::{Pchip, TAU, clamp, fraction, lerp, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::template::layout::{BodyLayout, HandFrame, HandTable};
use crate::template::soft_sd::TABLE;

pub mod five;
pub mod mitten;
pub mod simple;

/// The hand table every style is built from: the prototype's hand.
const HAND: HandTable = TABLE.hand;

/// Hand length along the forearm axis, from the hand's origin to the tip of the mitt.
const HAND_LENGTH: f64 = 0.050;
/// Where the rounded end cap begins, and the last station of the section curves.
const CAP_START: f64 = 0.040;
const LAST_STATION: f64 = 0.048;
/// Where the ring set starts: before the wrist, so the hand overlaps the sleeve end.
const START: f64 = -0.012;
/// Rings before the cap, evenly spaced from [`START`] to [`CAP_START`], and in the cap (spaced as a quarter sine).
const BODY_RINGS: usize = 7;
const CAP_RINGS: usize = 5;
/// Semi-axes of the section along the thumb side (width) and the palm normal (thickness), by distance from the origin.
const WIDTH: [(f64, f64); 6] =
    [(-0.012, 0.0170), (0.0, 0.0185), (0.015, 0.0215), (0.030, 0.0235), (0.042, 0.0225), (0.048, 0.0210)];
const THICKNESS: [(f64, f64); 6] =
    [(-0.012, 0.0125), (0.0, 0.0135), (0.015, 0.0140), (0.030, 0.0135), (0.042, 0.0125), (0.048, 0.0115)];
/// The palm side is flatter, the back of the hand plumper.
const PALM_FLATTEN: f64 = 0.80;
const BACK_PLUMP: f64 = 1.08;
/// The wrist end of the block, which a cuff hides, fades up to half into the forearm bone between these stations.
const WRIST_BLEND: (f64, f64) = (-0.004, -0.014);
/// Skin tones, linear RGBA: the base, and the warmer tint towards the finger tips (the tip ramp, over `u`).
const BASE_TONE: [f32; 4] = [1.0, 0.80, 0.70, 1.0];
const TIP_TONE: [f32; 4] = [1.0, 0.68, 0.60, 1.0];
const TIP_RAMP: (f64, f64) = (0.030, 0.050);

/// The thumb's rings, from root to tip (dense at both ends so the root blends in and the tip rounds), and their size.
const THUMB_T: [f64; 11] = [0.0, 0.1, 0.22, 0.36, 0.5, 0.64, 0.76, 0.86, 0.93, 0.975, 1.0];
const THUMB_SEGMENTS: usize = 16;
/// The thumb curls towards the palm by `BEND_N * t^2` and sideways by `BEND_T * t`.
const THUMB_BEND: (f64, f64) = (0.003, -0.002);
const THUMB_RADIUS: f64 = 0.0075;
const THUMB_TAPER: f64 = 0.25;
/// Where along the thumb its tip cap begins (a quarter ellipse), and how much narrower it is across than through.
const THUMB_CAP: f64 = 0.80;
const THUMB_ASPECT: f64 = 0.90;
/// The thumb's tint ramp over its ring index, as a fraction.
const THUMB_TIP_RAMP: (f64, f64) = (0.4, 1.0);

/// The thumb's hand over between its bones, along its rings (`t`): the metacarpal up to the first, the distal from the
/// second.
const THUMB_METACARPAL: (f64, f64) = (0.30, 0.05);
const THUMB_DISTAL: (f64, f64) = (0.50, 0.85);

/// The left hand's frame and size: where its parts go.
struct Hand {
    /// The wrist joint.
    wrist: DVec3,
    /// Along the forearm, towards the thumb side, out of the palm (a right-handed frame).
    frame: HandFrame,
    size: f64,
    palm_length: f64,
    palm_width: f64,
    thumb_length: f64,
    spread_deg: f64,
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
        let frame = HandFrame::of(wrist - elbow, HAND.palm_hint, HAND.thumb_hint);
        let slider = |v: f64| if v.is_finite() { clamp(v, 0.5, 2.0) } else { 1.0 };
        let spread = params.hand.thumb_spread_deg;
        Ok(Self {
            wrist,
            frame,
            size: slider(params.proportions.hand_size),
            palm_length: slider(params.hand.palm_length),
            palm_width: slider(params.hand.palm_width),
            thumb_length: slider(params.hand.thumb_length),
            spread_deg: if spread.is_finite() { clamp(spread, -30.0, 30.0) } else { 0.0 },
        })
    }

    /// The point `u` along the hand axis from its origin, `lat` towards the thumb and `nrm` out of the palm, in the
    /// prototype's (unscaled, unit-size) numbers. The palm (up to the knuckles) is stretched by the palm length and the
    /// width by the palm width, as the layout places the finger and thumb joints that skin it.
    fn place(&self, u: f64, lat: f64, nrm: f64) -> DVec3 {
        let along = (HAND.shift + u.min(HAND.knuckle)) * self.palm_length + (u - HAND.knuckle).max(0.0);
        let f = &self.frame;
        self.wrist + (f.axis * along + f.thumb * (lat * self.palm_width) + f.palm * nrm) * self.size
    }

    /// The fingertips' curl towards the palm at `u`.
    fn curl(u: f64) -> f64 {
        (HAND.curl)(u)
    }
}

/// How a style shapes the block's surface and what it adds.
pub(super) struct Style {
    /// Segments once round a ring of the block.
    pub(super) around: usize,
    /// Added to the thumb's spread, degrees (a simple hand's thumb is splayed).
    pub(super) splay_deg: f64,
    /// Scales a ring point's lateral position and height from `(u, lateral)`: the finger gap grooves.
    pub(super) groove: fn(f64, f64) -> f64,
}

fn bone(bone: HumanoidBone) -> JointRef {
    JointRef::Humanoid(bone)
}

/// The skin at distance `u` from the origin, `lat` towards the thumb: the hand bone with the wrist end fading up to
/// half into the forearm; past the knuckle row, the nearest finger bones.
fn palm_skin(u: f64, lat: f64) -> Vec<(JointRef, f32)> {
    const FINGERS: [HumanoidBone; 4] = [
        HumanoidBone::LeftIndexProximal,
        HumanoidBone::LeftMiddleProximal,
        HumanoidBone::LeftRingProximal,
        HumanoidBone::LeftLittleProximal,
    ];
    let forearm = 0.5 * sstep(WRIST_BLEND.0, WRIST_BLEND.1, u);
    let fingers = sstep(HAND.knuckle - 0.004, HAND.knuckle + 0.004, u);
    let palm = 1.0 - fingers;
    let mut blend = vec![
        (bone(HumanoidBone::LeftHand), (palm * (1.0 - forearm)) as f32),
        (bone(HumanoidBone::LeftLowerArm), (palm * forearm) as f32),
    ];
    // A tent per finger over the finger lines' spacing: the four tents sum to 1 between the outer fingers.
    let spacing = HAND.finger_across[0] - HAND.finger_across[1];
    let lat = clamp(lat, HAND.finger_across[3], HAND.finger_across[0]);
    for (finger, across) in FINGERS.into_iter().zip(HAND.finger_across) {
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

fn tone(amount: f64) -> [f32; 4] {
    std::array::from_fn(|k| lerp(f64::from(BASE_TONE[k]), f64::from(TIP_TONE[k]), amount) as f32)
}

/// `n` as a float.
fn count(n: usize) -> f64 {
    fraction(n, 1)
}

/// The palm-and-finger block's rings: the stations (`u`) they lie at, and the section curves.
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
        let step = (CAP_START - START) / count(BODY_RINGS);
        let mut stations: Vec<f64> = (0..BODY_RINGS).map(|k| START + step * count(k)).collect();
        stations.extend((0..CAP_RINGS).map(|k| {
            CAP_START + (HAND_LENGTH - CAP_START) * (fraction(k, CAP_RINGS - 1) * std::f64::consts::FRAC_PI_2).sin()
        }));
        Ok(Self { stations, width: Pchip::new(&WIDTH)?, thickness: Pchip::new(&THICKNESS)? })
    }

    /// The block's grid, one ring per station, `style.around` segments round.
    fn grid(&self, hand: &Hand, style: &Style) -> Result<Grid, KitError> {
        let segments = style.around;
        Grid::from_fn(self.stations.len(), segments + 1, |i, j| {
            let u = self.stations[i];
            let (a, b) = self.section(u);
            let phi = fraction(j, segments) * TAU;
            let (sn, ct) = phi.sin_cos();
            // Flatten the palm side (sn > 0), plump the back.
            let b = b * if sn > 0.0 { PALM_FLATTEN } else { BACK_PLUMP };
            let lateral = a * ct;
            let groove = (style.groove)(u, lateral);
            hand.place(u, lateral * groove, b * sn * groove + Hand::curl(u))
        })
    }

    /// The centre of row `i`, for the interior reference of the normals.
    fn centre(&self, hand: &Hand, i: usize) -> DVec3 {
        let u = self.stations[i];
        hand.place(u, 0.0, Hand::curl(u))
    }
}

/// The thumb: its grid and the centres of its rings. Frames are rebuilt per ring from the tangent, with the palm normal
/// projected off it, so the thumb does not twist.
fn thumb(hand: &Hand, splay_deg: f64) -> Result<(Grid, Vec<DVec3>), KitError> {
    let t = &HAND.thumb;
    // Spread opens the thumb about its metacarpal joint, in the plane of the palm.
    let (sin, cos) = (hand.spread_deg + splay_deg).to_radians().sin_cos();
    let turn = |p: DVec3| {
        let v = p - t.base;
        t.base + DVec3::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos, v.z)
    };
    let root = turn(t.root);
    let direction = turn(t.base + t.direction.normalize_or_zero()) - t.base;
    let length = t.length * hand.thumb_length;
    // Centres in the hand's own numbers (`x` along, `y` thumb side, `z` palm), then placed.
    let centres: Vec<DVec3> = THUMB_T
        .iter()
        .map(|&s| {
            let p = root + direction * (length * s) + DVec3::new(0.0, THUMB_BEND.1 * s, THUMB_BEND.0 * s * s);
            hand.place(p.x, p.y, p.z)
        })
        .collect();
    let radii = |s: f64| {
        let base = THUMB_RADIUS * (1.0 - THUMB_TAPER * s);
        let cap =
            if s < THUMB_CAP { 1.0 } else { (1.0 - ((s - THUMB_CAP) / (1.0 - THUMB_CAP)).powi(2)).max(0.0).sqrt() };
        (base * cap * THUMB_ASPECT * hand.size, base * cap * hand.size)
    };
    let last = centres.len() - 1;
    let grid = Grid::from_fn(centres.len(), THUMB_SEGMENTS + 1, |k, j| {
        let tangent = match k {
            0 => centres[1] - centres[0],
            _ if k == last => centres[k] - centres[k - 1],
            _ => centres[k + 1] - centres[k - 1],
        }
        .normalize_or_zero();
        let normal = (hand.frame.palm - tangent * hand.frame.palm.dot(tangent)).normalize_or_zero();
        let binormal = tangent.cross(normal).normalize_or_zero();
        let (ra, rb) = radii(THUMB_T[k]);
        let (s, c) = (fraction(j, THUMB_SEGMENTS) * TAU).sin_cos();
        centres[k] + normal * (c * ra) + binormal * (s * rb)
    })?;
    Ok((grid, centres))
}

/// The left hand: the block and the thumb in one builder (the prototype's `mb`).
fn left_hand(hand: &Hand, style: &Style) -> Result<(MeshBuilder, Block), KitError> {
    let block = Block::new()?;
    let block_grid = block.grid(hand, style)?;
    let mut builder = MeshBuilder::with_colours();
    let inside = |i: usize, _j: usize| block.centre(hand, i);
    let colour = |i: usize, _j: usize, _p: DVec3| tone(sstep(TIP_RAMP.0, TIP_RAMP.1, block.stations[i]));
    let skin = |_p: DVec3, i: usize, j: usize| {
        let (a, _) = block.section(block.stations[i]);
        palm_skin(block.stations[i], a * (fraction(j, style.around) * TAU).cos())
    };
    builder.add_grid(
        &block_grid,
        &GridOptions {
            wrap_j: true,
            inside: Some(&inside),
            colour: Some(&colour),
            skin: Skin::Per(&skin),
            ..GridOptions::default()
        },
    )?;
    let (grid, centres) = thumb(hand, style.splay_deg)?;
    let inside = |i: usize, _j: usize| centres[i];
    let colour = |i: usize, _j: usize, _p: DVec3| tone(sstep(THUMB_TIP_RAMP.0, THUMB_TIP_RAMP.1, fraction(i, 10)));
    let skin = |_p: DVec3, i: usize, _j: usize| thumb_skin(THUMB_T[i]);
    builder.add_grid(
        &grid,
        &GridOptions {
            wrap_j: true,
            inside: Some(&inside),
            colour: Some(&colour),
            skin: Skin::Per(&skin),
            ..GridOptions::default()
        },
    )?;
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
    let mirrored: Vec<_> = parts.iter().map(|(name, b)| (name.replace("-l", "-r"), b.mirrored())).collect();
    parts
        .into_iter()
        .map(|(name, b)| (name.to_string(), b))
        .chain(mirrored)
        .map(|(name, b)| b.finish(name, material.clone()))
        .collect()
}
