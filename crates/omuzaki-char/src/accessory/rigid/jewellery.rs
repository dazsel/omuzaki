//! Fine jewellery (#2413): `ring` on a finger, `earrings` on the ears, `bracelet` on a wrist and the ornate
//! `filigree-collar` round the base of the neck. Each is built from the shared shells (rings, lathes, wires) in the `metal`
//! finish, its stones in `gem` ([`Finish::Jewel`]), so a close-up reads banded gold and faceted stones, not plastic.
//!
//! **Where each sits.** A ring sits on the finger's proximal segment, its stone on the back of the hand, sized from the
//! spacing of the knuckles (the hand builds each finger that thick). Earrings hang from the lobe of each shell ear, under
//! the ear's root (`ear_l`, `ear_r`, as the head publishes them, and the ear's size from its cover). A bracelet wraps the
//! forearm's section a little above the wrist. The collar wraps the neck's section at its base and hangs its motifs
//! down the front. Any of them can be put elsewhere with the part's `attach`: the ring, bracelet and collar are then
//! built in the attachment's frame round its `+Y`, and the earrings hang from it.
//!
//! **Filigree.** The collar's motifs are wire scrolls: two C-curls back to back over a stem, the shape a jeweller bends
//! from one wire, each ending in a small stone; the band is a twisted pair of wires over a flat backing.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{
    AnchorsFact, BodyRegion, BuildCtx, BuildError, JointRef, PartCategory, PartKind, PartMeta, PartOutput, RecolourCtx,
    Recoloured, RigContribution,
};
use omuzaki_core::glam::{DMat3, DMat4, DQuat, DVec3};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    Detail, Finish, Follow, PendantShape, ResolvedAttach, Ring, Shells, Slot, attachment, kit_error, output_with,
    recolour_with,
};
use crate::head::ears::Side as EarSide;
use crate::head::ears::fact::EarCoverFact;
use crate::kit::KitError;
use crate::kit::curve::fraction;

/// Gold.
fn gold() -> ColourRef {
    ColourRef::rgb(0xe8, 0xc0, 0x60)
}

/// A ruby.
fn ruby() -> ColourRef {
    ColourRef::rgb(0xd0, 0x28, 0x4c)
}

/// Which hand or wrist.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Hand {
    /// The character's left.
    #[default]
    Left,
    /// The character's right.
    Right,
}

impl Hand {
    /// `+1` for the left (`+X`), `-1` for the right.
    fn sign(self) -> f64 {
        match self {
            Self::Left => 1.0,
            Self::Right => -1.0,
        }
    }

    /// The left-hand `bone`, mirrored for the right hand.
    fn bone(self, bone: HumanoidBone) -> HumanoidBone {
        match self {
            Self::Left => bone,
            Self::Right => bone.mirrored(),
        }
    }
}

/// A stone set in a piece.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Stone {
    /// Its cut: `gem` (six facets), `orb` (a cabochon) or `drop`.
    pub shape: PendantShape,
    /// Its radius, metres.
    pub size: f64,
}

impl Default for Stone {
    fn default() -> Self {
        Self { shape: PendantShape::Gem, size: 0.0022 }
    }
}

impl Validate for Stone {
    fn check(&self, c: &mut Check) {
        c.range("size", self.size, 0.0008, 0.012);
    }
}

/// The local frame at `origin` whose `+Y` is `up` and `+Z` is `front` (made square to `up`).
fn frame(origin: DVec3, up: DVec3, front: DVec3) -> DMat4 {
    let y = up.normalize_or_zero();
    let z = (front - y * front.dot(y)).try_normalize().unwrap_or_else(|| y.any_orthonormal_vector());
    DMat4::from_mat3_translation(DMat3::from_cols(y.cross(z), y, z), origin)
}

/// A stone sitting on local `+Y` at `base`, in a bezel cup of the primary metal.
fn set_stone(shells: &mut Shells<'_>, base: DVec3, stone: &Stone) -> Result<(), KitError> {
    let (rows, lathe) = (shells.detail.rows, shells.detail.lathe);
    let size = stone.size;
    let bezel = Ring {
        centre: base,
        ex: DVec3::X,
        ez: DVec3::Z,
        up: DVec3::Y,
        rx: size * 1.02,
        rz: size * 1.02,
        half_w: size * 0.28,
        half_t: size * 0.16,
    };
    shells.ring(Slot::Primary, &bezel)?;
    let profile = stone.shape.profile(size, rows);
    let lift = match stone.shape {
        PendantShape::Drop => size * 1.2,
        PendantShape::Orb | PendantShape::Gem => size * 0.55,
    };
    shells.lathe(Slot::Accent, base + DVec3::Y * lift, &profile, stone.shape.segments(lathe))
}

// ------------------------------------------------------------------------------------------------ ring

/// A finger.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Finger {
    /// The thumb.
    Thumb,
    /// The index finger.
    Index,
    /// The middle finger.
    Middle,
    /// The ring finger.
    #[default]
    Ring,
    /// The little finger.
    Little,
}

impl Finger {
    /// The left hand's proximal and next bone of the finger.
    fn bones(self) -> [HumanoidBone; 2] {
        use HumanoidBone as B;
        match self {
            Self::Thumb => [B::LeftThumbProximal, B::LeftThumbDistal],
            Self::Index => [B::LeftIndexProximal, B::LeftIndexIntermediate],
            Self::Middle => [B::LeftMiddleProximal, B::LeftMiddleIntermediate],
            Self::Ring => [B::LeftRingProximal, B::LeftRingIntermediate],
            Self::Little => [B::LeftLittleProximal, B::LeftLittleIntermediate],
        }
    }

    /// How thick the finger is against the middle and ring fingers.
    fn girth(self) -> f64 {
        match self {
            Self::Thumb => 1.15,
            Self::Index | Self::Middle | Self::Ring => 1.0,
            Self::Little => 0.88,
        }
    }
}

/// A finger's radius as a share of the spacing of the knuckles: the hand's own `FINGER_SHARE`.
const FINGER_SHARE: f64 = 0.46;

/// A ring on a finger, with an optional stone on the back of the hand.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RingParams {
    /// Which hand.
    pub hand: Hand,
    /// Which finger.
    pub finger: Finger,
    /// Where along the finger's first segment, `0` at the knuckle to `1` at the next joint.
    pub along: f64,
    /// The band's width along the finger, metres.
    pub width: f64,
    /// The band's thickness, metres.
    pub thickness: f64,
    /// The gap between the band and the finger, metres.
    pub ease: f64,
    /// The stone, or none. Left out when none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stone: Option<Stone>,
    /// The metal's colour.
    pub primary: ColourRef,
    /// The stone's colour.
    pub accent: ColourRef,
}

impl Default for RingParams {
    fn default() -> Self {
        Self {
            hand: Hand::Left,
            finger: Finger::Ring,
            along: 0.45,
            width: 0.0026,
            thickness: 0.0008,
            ease: 0.0002,
            stone: Some(Stone::default()),
            primary: gold(),
            accent: ruby(),
        }
    }
}

impl Validate for RingParams {
    fn check(&self, c: &mut Check) {
        c.range("along", self.along, 0.0, 1.0);
        c.range("width", self.width, 0.0008, 0.012);
        c.range("thickness", self.thickness, 0.0003, 0.004);
        c.range("ease", self.ease, 0.0, 0.004);
        if let Some(stone) = &self.stone {
            c.nested("stone", stone);
        }
    }
}

/// The `ring` kind.
pub struct FingerRing;

impl FingerRing {
    /// The ring's frame on its finger (`+Y` the back of the hand, `+Z` along the finger) and the finger's radius there.
    fn on_finger(ctx: &BuildCtx<'_>, p: &RingParams) -> Result<(ResolvedAttach, f64), BuildError> {
        let joint = |b: HumanoidBone| {
            ctx.joint(&JointRef::Humanoid(p.hand.bone(b))).map(DVec3::from).ok_or_else(|| {
                ctx.fail(format!("the body has no {} joint: a ring needs five-finger hands", p.hand.bone(b).name()))
            })
        };
        let [first, next] = p.finger.bones();
        let (root, end) = (joint(first)?, joint(next)?);
        let (middle, ring, index) = (
            joint(HumanoidBone::LeftMiddleProximal)?,
            joint(HumanoidBone::LeftRingProximal)?,
            joint(HumanoidBone::LeftIndexProximal)?,
        );
        let axis = (end - root).normalize_or_zero();
        // The back of the hand: square to the fingers and to the row of knuckles, outward for either hand.
        let row = ring - index;
        let back = (axis.cross(row) * p.hand.sign()).normalize_or_zero();
        let radius = FINGER_SHARE * middle.distance(ring) * p.finger.girth();
        let centre = root.lerp(end, p.along);
        let joint = JointRef::Humanoid(p.hand.bone(first));
        Ok((ResolvedAttach::new(joint, frame(centre, back, axis)), radius))
    }
}

impl PartKind for FingerRing {
    type Params = RingParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "ring",
            summary: "Put a ring on a finger: a metal band with an optional stone on the back of the hand.",
            category: PartCategory::Accessory,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &RingParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let (on_finger, finger) = Self::on_finger(ctx, p)?;
        let finger = if ctx.attach.is_some() { 0.006 } else { finger };
        let attach = attachment(ctx, |_| Ok(on_finger))?;
        let follow = Follow::Fixed(attach.joint.clone());
        let mut shells = Shells::new(&attach, &follow, Detail::of(ctx.quality));
        let r = finger + p.ease + p.thickness;
        // The band: round the finger (local X and Y), its width along it (local Z).
        let band = Ring {
            centre: DVec3::ZERO,
            ex: DVec3::X,
            ez: DVec3::Y,
            up: DVec3::Z,
            rx: r,
            rz: r,
            half_w: 0.5 * p.width,
            half_t: p.thickness,
        };
        let err = kit_error(ctx);
        shells.ring(Slot::Primary, &band).map_err(&err)?;
        if let Some(stone) = &p.stone {
            set_stone(&mut shells, DVec3::Y * (r + p.thickness * 0.5), stone).map_err(&err)?;
        }
        output_with(ctx, vec![shells], [&p.primary, &p.accent], 0.0, RigContribution::default(), Finish::Jewel)
    }

    fn recolour(p: &RingParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour_with(out, ctx, [&p.primary, &p.accent], 0.0, Finish::Jewel)
    }
}

omuzaki_char_api::register_part_kind!(FingerRing);

// ------------------------------------------------------------------------------------------------ earrings

/// The kind of earring.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EarringStyle {
    /// A stone on the lobe.
    Stud,
    /// A ring through the lobe.
    Hoop,
    /// A stone hanging on a short chain.
    #[default]
    Drop,
}

/// Which ears.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Ears {
    /// Both.
    #[default]
    Both,
    /// The left only.
    Left,
    /// The right only.
    Right,
}

/// A pair of earrings (or one), on the lobes of the side ears.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EarringsParams {
    /// The kind.
    pub style: EarringStyle,
    /// Which ears.
    pub ears: Ears,
    /// A hoop's radius, or a drop's chain length, metres.
    pub size: f64,
    /// The wire's thickness, metres.
    pub wire: f64,
    /// The stone (a stud's, or the drop's).
    pub stone: Stone,
    /// The metal's colour.
    pub primary: ColourRef,
    /// The stone's colour.
    pub accent: ColourRef,
}

impl Default for EarringsParams {
    fn default() -> Self {
        Self {
            style: EarringStyle::Drop,
            ears: Ears::Both,
            size: 0.008,
            wire: 0.0005,
            stone: Stone { shape: PendantShape::Drop, size: 0.0026 },
            primary: gold(),
            accent: ruby(),
        }
    }
}

impl Validate for EarringsParams {
    fn check(&self, c: &mut Check) {
        c.range("size", self.size, 0.002, 0.03);
        c.range("wire", self.wire, 0.0002, 0.002);
        c.nested("stone", &self.stone);
    }
}

/// How far below the ear's root its lobe is, as a share of the ear's half height.
const LOBE_DROP: f64 = 0.72;
/// How far the earring stands out of the ear's plane at the lobe, metres.
const LOBE_OUT: f64 = 0.0015;

/// The `earrings` kind.
pub struct Earrings;

impl Earrings {
    /// The frame at each chosen ear's lobe (`+Y` up the ear, `+Z` out of its face), following the head.
    fn lobes(ctx: &BuildCtx<'_>, p: &EarringsParams) -> Result<Vec<ResolvedAttach>, BuildError> {
        if ctx.attach.is_some() {
            return Ok(vec![attachment(ctx, |_| Err(ctx.fail("unreachable".to_owned())))?]);
        }
        let sides: &[EarSide] = match p.ears {
            Ears::Both => &EarSide::BOTH,
            Ears::Left => &[EarSide::Left],
            Ears::Right => &[EarSide::Right],
        };
        let covers = ctx.published.get::<EarCoverFact>();
        let anchors = ctx.published.get::<AnchorsFact>();
        sides
            .iter()
            .map(|side| {
                let name = crate::head::ears::anchor_name(*side);
                let cover = covers.iter().find(|((_, s), _)| s == side).map(|(_, c)| c);
                let anchor = anchors.iter().find(|((_, a), _)| a.as_str() == name).map(|(_, a)| a);
                let (Some(cover), Some(anchor)) = (cover, anchor) else {
                    return Err(ctx.fail(format!(
                        "earrings hang from the side ears, and the head has no `{name}`: give the head human or elf \
                         ears, or attach the earrings somewhere"
                    )));
                };
                let rotation = anchor.rotation.as_dquat();
                let (up, out) = (rotation * DVec3::Y, rotation * DVec3::Z);
                let lobe = DVec3::from(cover.centre) - up * (LOBE_DROP * f64::from(cover.radius)) + out * LOBE_OUT;
                // Hanging straight down whatever the ear's tilt, its face out of the ear.
                Ok(ResolvedAttach::new(anchor.bone.clone(), frame(lobe, DVec3::Y, out)))
            })
            .collect()
    }
}

impl PartKind for Earrings {
    type Params = EarringsParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "earrings",
            summary: "Hang earrings on the ear lobes: studs, hoops or drops with a stone, on one ear or both.",
            category: PartCategory::Accessory,
            requires: &["head"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &EarringsParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let lobes = Self::lobes(ctx, p)?;
        let follows: Vec<Follow> = lobes.iter().map(|a| Follow::Fixed(a.joint.clone())).collect();
        let detail = Detail::of(ctx.quality);
        let err = kit_error(ctx);
        let mut all = Vec::with_capacity(lobes.len());
        for (attach, follow) in lobes.iter().zip(&follows) {
            let mut shells = Shells::new(attach, follow, detail);
            let wire = p.wire;
            match p.style {
                EarringStyle::Stud => {
                    // The stone faces out of the ear.
                    let profile = p.stone.shape.profile(p.stone.size, detail.rows);
                    let seg = p.stone.shape.segments(detail.lathe);
                    shells
                        .lathe_along(Slot::Accent, DVec3::Z * p.stone.size * 0.4, DVec3::Z, &profile, seg)
                        .map_err(&err)?;
                }
                EarringStyle::Hoop => {
                    // Through the lobe, hanging below it, square to the ear's face.
                    let hoop = Ring {
                        centre: DVec3::new(0.0, -p.size + wire, 0.0),
                        ex: DVec3::Z,
                        ez: DVec3::Y,
                        up: DVec3::X,
                        rx: p.size,
                        rz: p.size,
                        half_w: wire * 1.6,
                        half_t: wire,
                    };
                    shells.ring(Slot::Primary, &hoop).map_err(&err)?;
                }
                EarringStyle::Drop => {
                    // A small loop through the lobe, a chain of `size`, a cap and the stone.
                    let loop_r = 0.0012_f64.max(wire * 2.5);
                    let hook = Ring {
                        centre: DVec3::new(0.0, -loop_r + wire, 0.0),
                        ex: DVec3::Z,
                        ez: DVec3::Y,
                        up: DVec3::X,
                        rx: loop_r,
                        rz: loop_r,
                        half_w: wire,
                        half_t: wire,
                    };
                    shells.ring(Slot::Primary, &hook).map_err(&err)?;
                    let top = -2.0 * loop_r;
                    let chain: Vec<DVec3> =
                        (0..=8).map(|k| DVec3::new(0.0, top - p.size * fraction(k, 8), 0.0)).collect();
                    shells
                        .wire(Slot::Primary, &chain, &|t| wire * (1.0 - (2.0 * t - 1.0).powi(8)).max(0.0))
                        .map_err(&err)?;
                    let base = DVec3::new(0.0, top - p.size, 0.0);
                    let stone = &p.stone;
                    let half = match stone.shape {
                        PendantShape::Drop => stone.size * 1.5,
                        PendantShape::Orb | PendantShape::Gem => stone.size,
                    };
                    let cap = [
                        (0.0, 0.0),
                        (-stone.size * 0.15, stone.size * 0.55),
                        (-stone.size * 0.35, stone.size * 0.6),
                        (-stone.size * 0.4, 0.0),
                    ];
                    shells.lathe(Slot::Primary, base, &cap, detail.lathe).map_err(&err)?;
                    let profile = stone.shape.profile(stone.size, detail.rows);
                    let centre = base - DVec3::Y * (stone.size * 0.3 + half);
                    shells.lathe(Slot::Accent, centre, &profile, stone.shape.segments(detail.lathe)).map_err(&err)?;
                }
            }
            all.push(shells);
        }
        output_with(ctx, all, [&p.primary, &p.accent], 0.0, RigContribution::default(), Finish::Jewel)
    }

    fn recolour(p: &EarringsParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour_with(out, ctx, [&p.primary, &p.accent], 0.0, Finish::Jewel)
    }
}

omuzaki_char_api::register_part_kind!(Earrings);

// ------------------------------------------------------------------------------------------------ bracelet

/// The kind of bracelet.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BraceletStyle {
    /// A round wire.
    #[default]
    Bangle,
    /// A wide flat band with a stone on the back of the wrist.
    Cuff,
    /// A string of beads.
    Beads,
}

/// A bracelet round a wrist.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BraceletParams {
    /// Which wrist.
    pub hand: Hand,
    /// The kind.
    pub style: BraceletStyle,
    /// How far up the forearm from the wrist, metres.
    pub along: f64,
    /// The band's width (a cuff's), or the wire's or a bead's diameter, metres.
    pub width: f64,
    /// The gap between it and the arm, metres.
    pub ease: f64,
    /// The cuff's stone. Left out when none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stone: Option<Stone>,
    /// The metal's colour (the beads').
    pub primary: ColourRef,
    /// The stone's colour.
    pub accent: ColourRef,
}

impl Default for BraceletParams {
    fn default() -> Self {
        Self {
            hand: Hand::Left,
            style: BraceletStyle::Bangle,
            along: 0.012,
            width: 0.0035,
            ease: 0.0025,
            stone: None,
            primary: gold(),
            accent: ruby(),
        }
    }
}

impl Validate for BraceletParams {
    fn check(&self, c: &mut Check) {
        c.range("along", self.along, 0.0, 0.08);
        c.range("width", self.width, 0.001, 0.04);
        c.range("ease", self.ease, 0.0, 0.02);
        if let Some(stone) = &self.stone {
            c.nested("stone", stone);
        }
    }
}

/// The `bracelet` kind.
pub struct Bracelet;

impl Bracelet {
    /// The frame round the forearm `along` above the wrist (`+Y` toward the hand, `+Z` the back of the wrist) and the
    /// arm's half extents there.
    fn on_wrist(ctx: &BuildCtx<'_>, p: &BraceletParams) -> Result<(ResolvedAttach, [f64; 2]), BuildError> {
        let region = match p.hand {
            Hand::Left => BodyRegion::ArmL,
            Hand::Right => BodyRegion::ArmR,
        };
        let wrist = ctx
            .joint(&JointRef::Humanoid(p.hand.bone(HumanoidBone::LeftHand)))
            .map(DVec3::from)
            .ok_or_else(|| ctx.fail("the body has no wrist joint"))?;
        let fit = ctx.fit()?;
        // The section `along` short of the wrist: the arm's sections run shoulder to hand.
        let mut best: Option<(f64, omuzaki_char_api::Section)> = None;
        for k in 0..=200 {
            #[allow(clippy::cast_possible_truncation)] // a fraction of an arm
            let t = (0.4 + 0.6 * fraction(k, 200)) as f32;
            let Some(section) = fit.section(region, t) else { continue };
            let miss = (DVec3::from(section.centre).distance(wrist) - p.along).abs();
            if best.as_ref().is_none_or(|(m, _)| miss < *m) {
                best = Some((miss, section));
            }
        }
        let (_, section) = best.ok_or_else(|| ctx.fail("the body has no forearm section to wrap"))?;
        let centre = DVec3::from(section.centre);
        let mut axis = DVec3::from(section.axis).normalize_or_zero();
        if axis.dot(wrist - centre) < 0.0 {
            axis = -axis;
        }
        let joint = JointRef::Humanoid(p.hand.bone(HumanoidBone::LeftLowerArm));
        let radii = [f64::from(section.radii.x), f64::from(section.radii.y)];
        Ok((ResolvedAttach::new(joint, frame(centre, axis, DVec3::Y)), radii))
    }
}

impl PartKind for Bracelet {
    type Params = BraceletParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "bracelet",
            summary: "Put a bracelet on a wrist: a bangle, a cuff with a stone or a string of beads.",
            category: PartCategory::Accessory,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &BraceletParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let (on_wrist, [across, through]) = Self::on_wrist(ctx, p)?;
        let attach = attachment(ctx, |_| Ok(on_wrist))?;
        let follow = Follow::Fixed(attach.joint.clone());
        let mut shells = Shells::new(&attach, &follow, Detail::of(ctx.quality));
        let err = kit_error(ctx);
        // Round the arm: its across and through extents lie in local X and Z (`+Z` faces up the T-pose's back).
        let half = 0.5 * p.width;
        let (rx, rz) = (across.max(through) + p.ease, across.max(through) + p.ease);
        match p.style {
            BraceletStyle::Bangle => {
                let band = Ring {
                    centre: DVec3::ZERO,
                    ex: DVec3::X,
                    ez: DVec3::Z,
                    up: DVec3::Y,
                    rx: rx + half,
                    rz: rz + half,
                    half_w: half,
                    half_t: half,
                };
                shells.ring(Slot::Primary, &band).map_err(&err)?;
            }
            BraceletStyle::Cuff => {
                let t = 0.0009;
                let band = Ring {
                    centre: DVec3::ZERO,
                    ex: DVec3::X,
                    ez: DVec3::Z,
                    up: DVec3::Y,
                    rx: rx + t,
                    rz: rz + t,
                    half_w: half,
                    half_t: t,
                };
                shells.ring(Slot::Primary, &band).map_err(&err)?;
                // A rolled edge at both rims.
                for edge in [-1.0, 1.0] {
                    let rim = Ring {
                        centre: DVec3::Y * (edge * half),
                        ex: DVec3::X,
                        ez: DVec3::Z,
                        up: DVec3::Y,
                        rx: rx + 2.0 * t,
                        rz: rz + 2.0 * t,
                        half_w: t,
                        half_t: t,
                    };
                    shells.ring(Slot::Primary, &rim).map_err(&err)?;
                }
                if let Some(stone) = &p.stone {
                    // On the back of the wrist, facing out.
                    let base = DVec3::Z * (rz + 2.0 * t);
                    let profile = stone.shape.profile(stone.size, shells.detail.rows);
                    let seg = stone.shape.segments(shells.detail.lathe);
                    shells
                        .lathe_along(Slot::Accent, base + DVec3::Z * stone.size * 0.55, DVec3::Z, &profile, seg)
                        .map_err(&err)?;
                }
            }
            BraceletStyle::Beads => {
                let bead = half;
                let ring_r = rx + bead;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few dozen beads
                let count = ((TAU * ring_r) / (2.1 * bead)).floor().max(6.0) as usize;
                for k in 0..count {
                    let a = TAU * fraction(k, count);
                    let centre = DVec3::new(ring_r * a.cos(), 0.0, ring_r * a.sin());
                    let slot = if k % 2 == 0 { Slot::Primary } else { Slot::Accent };
                    shells.ellipsoid(slot, centre, DVec3::splat(bead), DQuat::IDENTITY).map_err(&err)?;
                }
            }
        }
        output_with(ctx, vec![shells], [&p.primary, &p.accent], 0.0, RigContribution::default(), Finish::Jewel)
    }

    fn recolour(p: &BraceletParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour_with(out, ctx, [&p.primary, &p.accent], 0.0, Finish::Jewel)
    }
}

omuzaki_char_api::register_part_kind!(Bracelet);

// ------------------------------------------------------------------------------------------------ collar

/// An ornate collar of filigree round the base of the neck.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CollarParams {
    /// Where along the neck, `0` at its base.
    pub along: f64,
    /// The gap between the band and the neck, metres.
    pub ease: f64,
    /// How many filigree motifs hang across the front, `1..=15` (odd counts centre one).
    pub motifs: u8,
    /// How far round the neck the motifs spread, degrees either side of the front.
    pub spread_deg: f64,
    /// How far a motif hangs below the band, metres.
    pub drop: f64,
    /// The filigree wire's thickness, metres.
    pub wire: f64,
    /// The stones at the motifs' feet and the centre: their size, metres (0 for none).
    pub stone_size: f64,
    /// The metal's colour.
    pub primary: ColourRef,
    /// The stones' colour.
    pub accent: ColourRef,
}

impl Default for CollarParams {
    fn default() -> Self {
        Self {
            along: 0.08,
            ease: 0.004,
            motifs: 7,
            spread_deg: 70.0,
            drop: 0.022,
            wire: 0.00055,
            stone_size: 0.0018,
            primary: gold(),
            accent: ruby(),
        }
    }
}

impl Validate for CollarParams {
    fn check(&self, c: &mut Check) {
        c.range("along", self.along, 0.0, 1.0);
        c.range("ease", self.ease, 0.0, 0.03);
        c.range("motifs", f64::from(self.motifs), 1.0, 15.0);
        c.range("spread_deg", self.spread_deg, 10.0, 110.0);
        c.range("drop", self.drop, 0.006, 0.06);
        c.range("wire", self.wire, 0.0002, 0.002);
        c.range("stone_size", self.stone_size, 0.0, 0.008);
    }
}

/// The `filigree-collar` kind.
pub struct Collar;

/// Points along one motif in its own plane: `x` round the neck, `y` down; the motif is `width` wide and `drop` long.
/// Two C-curls back to back over a stem, then a small curl at the foot: one wire's worth each.
fn motif_curves(width: f64, drop: f64) -> Vec<Vec<[f64; 2]>> {
    let curl = |cx: f64, cy: f64, r: f64, from: f64, turns: f64, mirror: f64| -> Vec<[f64; 2]> {
        (0..=40)
            .map(|k| {
                let s = fraction(k, 40);
                let a = from + mirror * TAU * turns * s;
                let radius = r * (1.0 - 0.72 * s);
                [cx + mirror * radius * a.cos() * mirror, cy + radius * a.sin()]
            })
            .collect()
    };
    let r = 0.27 * width;
    let mut curves = Vec::new();
    for mirror in [-1.0, 1.0] {
        // From the band down and out, curling in toward the stem.
        curves.push(curl(mirror * 0.24 * width, -0.38 * drop, r, FRAC_PI_2, 0.9, mirror));
        // The lower curl, the other way, toward the foot.
        curves.push(curl(mirror * 0.15 * width, -0.78 * drop, 0.55 * r, -FRAC_PI_2, 0.8, -mirror));
    }
    // The stem.
    curves.push((0..=12).map(|k| [0.0, -drop * 0.9 * fraction(k, 12)]).collect());
    curves
}

impl PartKind for Collar {
    type Params = CollarParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "filigree-collar",
            summary: "Build an ornate collar of gold filigree round the base of the neck: twisted wire, scroll motifs and \
                      stones.",
            category: PartCategory::Accessory,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &CollarParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let (section_frame, [across, front]) = super::Choker::neck_frame(ctx, p.along)?;
        let attach = attachment(ctx, |_| Ok(section_frame))?;
        let follow = Follow::Fixed(attach.joint.clone());
        let detail = Detail::of(ctx.quality);
        let mut shells = Shells::new(&attach, &follow, detail);
        let err = kit_error(ctx);
        let (rx, rz) = (across + p.ease, front + p.ease);
        let wire = p.wire;
        // The band: a flat backing and a twisted pair of wires along its top and bottom.
        let backing = Ring {
            centre: DVec3::ZERO,
            ex: DVec3::X,
            ez: DVec3::Z,
            up: DVec3::Y,
            rx: rx + wire,
            rz: rz + wire,
            half_w: 2.2 * wire,
            half_t: 0.6 * wire,
        };
        shells.ring(Slot::Primary, &backing).map_err(&err)?;
        let on_band = |theta: f64, out: f64, y: f64| DVec3::new((rx + out) * theta.sin(), y, (rz + out) * theta.cos());
        let steps = detail.ring * 3;
        for (edge, phase) in [(2.6 * wire, 0.0), (-2.6 * wire, PI)] {
            for strand in [0.0, PI] {
                let path: Vec<DVec3> = (0..=steps)
                    .map(|k| {
                        let theta = TAU * fraction(k, steps);
                        let twist = 90.0 * theta + strand + phase;
                        on_band(theta, 1.6 * wire + 0.7 * wire * twist.cos(), edge + 0.7 * wire * twist.sin())
                    })
                    .collect();
                shells.wire(Slot::Primary, &path, &|_| 0.75 * wire).map_err(&err)?;
            }
        }
        // The motifs across the front: each one's plane is tangent to the band, its wires standing just off the chest,
        // leaning out as they hang (the chest falls away from the neck).
        let spread = p.spread_deg.to_radians();
        let count = usize::from(p.motifs.max(1));
        #[allow(clippy::cast_precision_loss)] // a few motifs
        let spacing = if count > 1 { 2.0 * spread / (count - 1) as f64 } else { 0.0 };
        let width = (spacing * rx.max(rz)).clamp(0.008, 0.03);
        let curves = motif_curves(width, p.drop);
        for m in 0..count {
            #[allow(clippy::cast_precision_loss)] // a few motifs
            let theta = if count > 1 { -spread + spacing * m as f64 } else { 0.0 };
            let place = |[x, y]: [f64; 2]| {
                // `x` round the band (as an angle at the band's radius), `y` down, leaning out as it drops.
                let along = theta + x / rx.max(rz);
                on_band(along, 2.2 * wire - 0.35 * y, y - 2.4 * wire)
            };
            let scale = if m == count / 2 && count % 2 == 1 { 1.25 } else { 1.0 };
            for curve in &curves {
                let path: Vec<DVec3> = curve.iter().map(|&[x, y]| place([x * scale, y * scale])).collect();
                shells.wire(Slot::Primary, &path, &|t| wire * (1.0 - 0.5 * t)).map_err(&err)?;
            }
            if p.stone_size > 0.0 {
                let foot = place([0.0, -p.drop * scale]);
                let normal = (foot - DVec3::new(0.0, foot.y, 0.0)).normalize_or_zero();
                let size = p.stone_size * scale;
                let profile = PendantShape::Gem.profile(size, detail.rows);
                let seg = PendantShape::Gem.segments(detail.lathe);
                shells.lathe_along(Slot::Accent, foot + normal * (0.4 * size), normal, &profile, seg).map_err(&err)?;
            }
        }
        output_with(ctx, vec![shells], [&p.primary, &p.accent], 0.0, RigContribution::default(), Finish::Jewel)
    }

    fn recolour(p: &CollarParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour_with(out, ctx, [&p.primary, &p.accent], 0.0, Finish::Jewel)
    }
}

omuzaki_char_api::register_part_kind!(Collar);
