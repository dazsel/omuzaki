//! Five-finger hands (RG-09): the palm block ended at the knuckle row, four fingers and the thumb, each finger a tapered
//! tube through its three joints and skinned to its VRM finger bones, so the hand-shape library, hand tracking and IK can
//! curl every finger.
//!
//! The palm is the mitten's block (the same section curves, rows and wrist sleeve) up to the knuckle row, closed over the
//! knuckles by a short rounded cap ([`KNUCKLE_CAP`]). The thumb is the mitten's, which is already free. Each finger runs
//! along the joints the layout placed for it (the proximal joint is the knuckle), so it follows `HandParams` exactly as
//! the skeleton does: its length and the hand size move the joints, and `finger_thickness` and `taper` shape the tube.
//! The tube starts a radius behind the knuckle, inside the palm's cap, so its open end never shows, and closes at the tip
//! in a quarter ellipse.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::JointRef;
use omuzaki_core::glam::DVec3;

use super::{Block, Hand, Style, THUMB_T, add_rings, bone, palm_skin, thumb, thumb_skin, tone};
use crate::kit::KitError;
use crate::kit::curve::{TAU, clamp, fraction, sstep};
use crate::kit::grid::{Grid, MeshBuilder};
use crate::template::layout::BodyLayout;

/// The palm's surface: smooth, as the mitten's, with no thumb splay (the fingers carry the shape).
pub(super) const STYLE: Style = Style { around: 28, splay_deg: 0.0, groove: |_, _| 1.0 };

/// The cap that closes the palm over the knuckles, in the prototype's metres along the hand, and its rows.
const KNUCKLE_CAP: f64 = 0.006;
const KNUCKLE_CAP_ROWS: usize = 6;

/// The left fingers' bones, index to little, proximal to distal.
const FINGERS: [[HumanoidBone; 3]; 4] = [
    [HumanoidBone::LeftIndexProximal, HumanoidBone::LeftIndexIntermediate, HumanoidBone::LeftIndexDistal],
    [HumanoidBone::LeftMiddleProximal, HumanoidBone::LeftMiddleIntermediate, HumanoidBone::LeftMiddleDistal],
    [HumanoidBone::LeftRingProximal, HumanoidBone::LeftRingIntermediate, HumanoidBone::LeftRingDistal],
    [HumanoidBone::LeftLittleProximal, HumanoidBone::LeftLittleIntermediate, HumanoidBone::LeftLittleDistal],
];

/// A finger's radius across the hand as a share of the spacing between finger lines, so neighbours just touch at the
/// knuckle; through the hand it is [`FINGER_ASPECT`] of that.
const FINGER_SHARE: f64 = 0.46;
const FINGER_ASPECT: f64 = 0.88;
/// How much narrower a finger is at the start of its tip cap than at the knuckle, at a `taper` of 1.
const FINGER_TAPER: f64 = 0.2;
/// Where along the finger (root to tip, 0..1) the tip cap begins.
const FINGER_CAP: f64 = 0.86;
/// The rings along a finger, from the root inside the palm to the tip: dense at the joints, where it bends, and at the
/// tip, where it rounds.
const FINGER_T: [f64; 16] = [0.0, 0.06, 0.12, 0.2, 0.3, 0.38, 0.46, 0.54, 0.62, 0.7, 0.78, 0.86, 0.92, 0.96, 0.99, 1.0];
const FINGER_SEGMENTS: usize = 14;
/// The skin hands over from one finger bone to the next over this share of the shorter segment either side of a joint.
const JOINT_BLEND: f64 = 0.35;

/// The left hand's palm, thumb and fingers in one builder.
pub(super) fn left_hand(hand: &Hand, layout: &BodyLayout) -> Result<MeshBuilder, KitError> {
    let block = Block::new()?;
    let style = &STYLE;
    let mut builder = MeshBuilder::with_colours();
    let palm_skin_at = |u: f64, j: usize| {
        let (a, _) = block.section(u);
        palm_skin(hand, u, a * (fraction(j, style.around) * TAU).cos())
    };

    // The palm's rows up to the knuckle row, then the cap: each row at its station with its section scaled down.
    let knuckle = hand.knuckle();
    let mut rows: Vec<(f64, f64)> = block.stations.iter().copied().filter(|&u| u < knuckle).map(|u| (u, 1.0)).collect();
    rows.extend((0..KNUCKLE_CAP_ROWS).map(|k| {
        let t = fraction(k, KNUCKLE_CAP_ROWS - 1);
        (knuckle + KNUCKLE_CAP * (t * std::f64::consts::FRAC_PI_2).sin(), (1.0 - t * t).max(0.0).sqrt())
    }));
    let palm = Grid::from_fn(rows.len(), style.around + 1, |i, j| block.point(hand, style, rows[i].0, rows[i].1, j))?;
    let white = |_: usize| tone(super::TIP_TONE, 0.0);
    add_rings(&mut builder, &palm, &|i| Block::centre(hand, rows[i].0), &white, &|i, j| palm_skin_at(rows[i].0, j))?;
    let sleeve = [hand.start(), block.stations[0]];
    add_rings(&mut builder, &block.sleeve(hand, style)?, &|i| Block::centre(hand, sleeve[i]), &white, &|i, j| {
        palm_skin_at(sleeve[i], j)
    })?;

    // The thumb, as the mitten has it.
    let thumb = thumb(hand, &block, style.splay_deg)?;
    let thumb_tone = |k: usize| tone(super::THUMB_TIP_TONE, sstep(0.4, 1.0, fraction(k, THUMB_T.len() - 1)));
    add_rings(&mut builder, &thumb.grid, &|i| thumb.centres[i], &thumb_tone, &|i, _| thumb_skin(THUMB_T[i]))?;
    add_rings(&mut builder, &thumb.stem, &|i| thumb.stem_centres[i], &|_| thumb_tone(0), &|_, _| thumb_skin(0.0))?;

    for bones in &FINGERS {
        let finger = Finger::new(hand, layout, bones)?;
        let grid = Grid::from_fn(FINGER_T.len(), FINGER_SEGMENTS + 1, |k, j| finger.ring(hand, k, j))?;
        let finger_tone = |k: usize| tone(super::TIP_TONE, sstep(0.5, 1.0, FINGER_T[k]));
        add_rings(&mut builder, &grid, &|k| finger.centre(FINGER_T[k]), &finger_tone, &|k, _| {
            finger.skin(FINGER_T[k])
        })?;
    }
    Ok(builder)
}

/// One finger: its path from a radius behind the knuckle, through its joints, to the tip, and its radii.
struct Finger {
    /// The path's points: the root inside the palm, the proximal (knuckle), intermediate and distal joints, the tip.
    points: [DVec3; 5],
    /// Arc length at each point.
    at: [f64; 5],
    /// The bones, proximal to distal.
    bones: [HumanoidBone; 3],
    /// Semi-axes at the knuckle, across the hand and through it.
    radius: (f64, f64),
    taper: f64,
}

impl Finger {
    fn new(hand: &Hand, layout: &BodyLayout, bones: &[HumanoidBone; 3]) -> Result<Self, KitError> {
        let joint = |b: HumanoidBone| {
            layout.joint64(b).filter(|p| p.is_finite()).ok_or(KitError::Degenerate { what: "a finger joint" })
        };
        let (p0, p1, p2) = (joint(bones[0])?, joint(bones[1])?, joint(bones[2])?);
        let split = hand.table.joint_split;
        // The tip lies past the distal joint as far as the rest of the finger's length, along the distal segment.
        let rest = (1.0 - split[2]) / (split[2] - split[1]).max(f64::EPSILON);
        let tip = p2 + (p2 - p1) * rest;
        let t = &hand.table;
        let spacing = (t.finger_across[0] - t.finger_across[1]).abs() * hand.palm_width * hand.size;
        let across = FINGER_SHARE * spacing * hand.finger_thickness;
        let radius = (across, across * FINGER_ASPECT);
        let root = p0 - (p1 - p0).normalize_or_zero() * radius.0.max(radius.1);
        let points = [root, p0, p1, p2, tip];
        let mut at = [0.0; 5];
        for k in 1..5 {
            at[k] = at[k - 1] + points[k].distance(points[k - 1]);
        }
        if !(at[4].is_finite() && at[4] > 0.0 && radius.0.is_finite() && radius.0 > 0.0) {
            return Err(KitError::Degenerate { what: "a finger" });
        }
        Ok(Self { points, at, bones: *bones, radius, taper: hand.finger_taper })
    }

    /// The point at `t` of the path's length (0 at the root, 1 at the tip).
    fn centre(&self, t: f64) -> DVec3 {
        let s = clamp(t, 0.0, 1.0) * self.at[4];
        let k = (1..5).find(|&k| s <= self.at[k]).unwrap_or(4);
        let span = (self.at[k] - self.at[k - 1]).max(f64::EPSILON);
        self.points[k - 1].lerp(self.points[k], (s - self.at[k - 1]) / span)
    }

    /// The path's direction at `t`, from the neighbouring rings.
    fn tangent(&self, t: f64) -> DVec3 {
        let h = 0.02;
        (self.centre(t + h) - self.centre(t - h)).normalize_or_zero()
    }

    /// Point `j` of ring `k`: an ellipse across the tangent, its narrower axis out of the palm.
    fn ring(&self, hand: &Hand, k: usize, j: usize) -> DVec3 {
        let t = FINGER_T[k];
        let tangent = self.tangent(t);
        let palm = hand.frame.palm;
        let normal = (palm - tangent * palm.dot(tangent)).normalize_or_zero();
        let binormal = tangent.cross(normal).normalize_or_zero();
        let narrow = 1.0 - FINGER_TAPER * self.taper * clamp(t / FINGER_CAP, 0.0, 1.0);
        let cap =
            if t < FINGER_CAP { 1.0 } else { (1.0 - ((t - FINGER_CAP) / (1.0 - FINGER_CAP)).powi(2)).max(0.0).sqrt() };
        let (sn, c) = (fraction(j, FINGER_SEGMENTS) * TAU).sin_cos();
        self.centre(t) + normal * (c * self.radius.1 * narrow * cap) + binormal * (sn * self.radius.0 * narrow * cap)
    }

    /// The skin at `t`: the hand behind the knuckle, then each finger bone over its own segment, handing over across
    /// each joint.
    fn skin(&self, t: f64) -> Vec<(JointRef, f32)> {
        let s = clamp(t, 0.0, 1.0) * self.at[4];
        let over = |joint: usize| {
            let before = self.at[joint] - self.at[joint - 1];
            let after = self.at[joint + 1] - self.at[joint];
            let half = JOINT_BLEND * before.min(after);
            sstep(self.at[joint] - half, self.at[joint] + half, s)
        };
        let (into_finger, into_intermediate, into_distal) = (over(1), over(2), over(3));
        vec![
            (bone(HumanoidBone::LeftHand), (1.0 - into_finger) as f32),
            (bone(self.bones[0]), (into_finger * (1.0 - into_intermediate)) as f32),
            (bone(self.bones[1]), (into_intermediate * (1.0 - into_distal)) as f32),
            (bone(self.bones[2]), into_distal as f32),
        ]
    }
}
