//! The hair part's parameters (spec CB-30..34): a hairstyle is a set of **groups**, each a generator that compiles
//! to **clumps**, tapered lens-shaped tubes along a guide curve. The clump is the common currency: the generators,
//! the physics (Verlet chains, stiff at the root and loose at the tip), the tube geometry and the glTF bake all work
//! on [`HairClump`]s, and hand-edited guide curves (CB-32) are simply a group of explicit clumps.
//!
//! Points follow the head where possible: a [`GuidePoint::Surface`] is anchored to the head surface (angles from the
//! top and from the front, plus an offset), so changing the head shape (a close-up variant with a longer lower
//! face) reshapes the hair with it. Free points hang from the head or the body.
//!
//! All lengths are metres. Heights and positions marked *head-local* are relative to the head centre.

use std::collections::BTreeMap;

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_core::id::{Slug, SlugError};
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{Check, ColourError, ColourRef, Palette, Validate, narrow};
use omuzaki_render_api::{ParamValue, ProgramParams, Shading};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{GroupId, JointRef};
use crate::kind::HeadSurface;

/// The name of the renderer shading program hair uses (`Shading::Program`). The program is written in `omuzaki-render`;
/// this is its contract with the character kit.
pub const HAIR_PROGRAM: &str = "hair";

/// The hair program's first extra vertex attribute: 2 floats per vertex, `(across, along)` the clump, `across` in
/// `-1..=1` and `along` in `0..=1` (the prototype's `hairUV`).
pub const HAIR_UV: &str = "hair-uv";

/// The hair program's second extra vertex attribute: 3 floats per vertex, `(strand seed, shade, polar angle)` (the
/// prototype's `hairInfo`): a per-clump random number in `0..=1`, the clump's shade multiplier, and the angle from the
/// top of the head in radians (the angel ring follows it).
pub const HAIR_INFO: &str = "hair-info";

// ------------------------------------------------------------------------------------------------ profiles and points

/// Width of a clump along its length `t` in `0..=1`: `start` at the root, `mid` at `tip_at`, then narrowing to `tip`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Taper {
    /// Width at the root.
    pub start: f64,
    /// Width at `tip_at`.
    pub mid: f64,
    /// Width at the very tip.
    pub tip: f64,
    /// Where along the clump the widest part sits, `0.1..=0.95`.
    pub tip_at: f64,
}

impl Default for Taper {
    fn default() -> Self {
        Self { start: 0.04, mid: 0.05, tip: 0.01, tip_at: 0.8 }
    }
}

impl Taper {
    /// The width at `t` (clamped to `0..=1`). Evaluated in `f32`: this runs per vertex when hair geometry is built.
    pub fn at(&self, t: f32) -> f32 {
        let (start, mid, tip, tip_at) = (narrow(self.start), narrow(self.mid), narrow(self.tip), narrow(self.tip_at));
        let t = t.clamp(0.0, 1.0);
        if t < tip_at {
            start + (mid - start) * (t / tip_at)
        } else {
            let u = ((t - tip_at) / (1.0 - tip_at).max(f32::EPSILON)).powf(0.8);
            mid + (tip - mid) * u
        }
    }
}

impl Validate for Taper {
    fn check(&self, c: &mut Check) {
        c.range("start", self.start, 0.001, 0.3);
        c.range("mid", self.mid, 0.001, 0.3);
        c.range("tip", self.tip, 0.0005, 0.3);
        c.range("tip_at", self.tip_at, 0.1, 0.95);
    }
}

/// Thickness of a clump (front to back) along `t`: `base + swell * sin(pi * t)`, a soft lens.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Bulge {
    /// Thickness at both ends.
    pub base: f64,
    /// Extra thickness in the middle.
    pub swell: f64,
}

impl Default for Bulge {
    fn default() -> Self {
        Self { base: 0.006, swell: 0.004 }
    }
}

impl Bulge {
    /// The thickness at `t` (clamped to `0..=1`), in `f32` like [`Taper::at`].
    pub fn at(&self, t: f32) -> f32 {
        narrow(self.base) + narrow(self.swell) * (std::f32::consts::PI * t.clamp(0.0, 1.0)).sin()
    }
}

impl Validate for Bulge {
    fn check(&self, c: &mut Check) {
        c.range("base", self.base, 0.0005, 0.05);
        c.range("swell", self.swell, 0.0, 0.05);
    }
}

/// A smooth transition from `root` to `tip` between `t = from` and `t = to`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Falloff {
    /// Value at the root.
    pub root: f64,
    /// Value at the tip.
    pub tip: f64,
    /// Where the transition starts, `0..=1`.
    pub from: f64,
    /// Where it ends, `0..=1`.
    pub to: f64,
}

impl Default for Falloff {
    fn default() -> Self {
        Self { root: 1.0, tip: 0.07, from: 0.28, to: 0.75 }
    }
}

impl Falloff {
    /// The value at `t`: a smoothstep between `from` and `to`, in `f32` like [`Taper::at`].
    pub fn at(&self, t: f32) -> f32 {
        let (from, to) = (narrow(self.from), narrow(self.to));
        let u = ((t - from) / (to - from).max(f32::EPSILON)).clamp(0.0, 1.0);
        narrow(self.root) + (narrow(self.tip) - narrow(self.root)) * (u * u * (3.0 - 2.0 * u))
    }
}

impl Validate for Falloff {
    fn check(&self, c: &mut Check) {
        c.range("root", self.root, 0.0, 1.0);
        c.range("tip", self.tip, 0.0, 1.0);
        c.range("from", self.from, 0.0, 1.0);
        c.range("to", self.to, 0.0, 1.0);
        if self.to <= self.from {
            c.fail("to", "must be greater than `from`");
        }
    }
}

/// One control point of a guide curve.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GuidePoint {
    /// On the head surface: `[theta_deg, phi_deg, offset]`. `theta` is the angle from the top of the head (0) down
    /// to the chin (180); `phi` goes around from the front (0) toward the character's left (+90) to the back (180);
    /// `offset` pushes the point outward from the surface, in metres.
    Surface([f64; 3]),
    /// Free in the head frame: `[x, y, z]` metres from the head centre. Follows the head.
    Head([f64; 3]),
    /// Free in bind space: `[x, y, z]` metres. Hangs from the body, blending toward the head near the scalp.
    Body([f64; 3]),
}

impl GuidePoint {
    /// The left-right mirror image: `phi` and `x` change sign.
    #[must_use]
    pub fn mirrored(self) -> Self {
        match self {
            Self::Surface([t, p, o]) => Self::Surface([t, -p, o]),
            Self::Head([x, y, z]) => Self::Head([-x, y, z]),
            Self::Body([x, y, z]) => Self::Body([-x, y, z]),
        }
    }

    /// The point in bind space, given the head it follows. `Surface` angles follow the convention above:
    /// direction `(sin theta sin phi, cos theta, sin theta cos phi)` from the head centre, pushed `offset` outward.
    pub fn resolve(self, head: &dyn HeadSurface) -> Vec3 {
        match self {
            Self::Surface([theta_deg, phi_deg, offset]) => {
                let (theta, phi) = (narrow(theta_deg).to_radians(), narrow(phi_deg).to_radians());
                let dir = Vec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos());
                head.point(dir, narrow(offset))
            }
            Self::Head([x, y, z]) => head.centre() + Vec3::new(narrow(x), narrow(y), narrow(z)),
            Self::Body([x, y, z]) => Vec3::new(narrow(x), narrow(y), narrow(z)),
        }
    }

    fn values(self) -> [f64; 3] {
        match self {
            Self::Surface(v) | Self::Head(v) | Self::Body(v) => v,
        }
    }
}

impl Validate for GuidePoint {
    fn check(&self, c: &mut Check) {
        if self.values().iter().any(|v| !v.is_finite()) {
            c.fail("", "a guide point has a non-finite coordinate");
        }
        if let Self::Surface([theta, _, offset]) = self {
            c.range("theta", *theta, 0.0, 180.0);
            c.range("offset", *offset, -0.05, 0.3);
        }
    }
}

/// A sideways wave that grows along a clump, with an optional curl at the tip.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Wave {
    /// Sideways amplitude at full growth.
    pub amplitude: f64,
    /// Full waves along the clump.
    pub frequency: f64,
    /// Phase in radians, so neighbouring clumps differ.
    pub phase: f64,
    /// How far the tip curls sideways.
    pub curl: f64,
    /// `t` where the wave starts to grow and where it is fully grown.
    pub growth: [f64; 2],
}

impl Default for Wave {
    fn default() -> Self {
        Self { amplitude: 0.0, frequency: 2.0, phase: 0.0, curl: 0.0, growth: [0.15, 0.65] }
    }
}

impl Validate for Wave {
    fn check(&self, c: &mut Check) {
        c.range("amplitude", self.amplitude, 0.0, 0.2);
        c.range("frequency", self.frequency, 0.0, 12.0);
        c.range("phase", self.phase, -7.0, 7.0);
        c.range("curl", self.curl, -0.2, 0.2);
        c.range("growth/0", self.growth[0], 0.0, 1.0);
        c.range("growth/1", self.growth[1], 0.0, 1.0);
    }
}

// ------------------------------------------------------------------------------------------------ generators

/// Which side(s) of the head a generator makes locks for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HairSide {
    /// The character's left (+x).
    Left,
    /// The character's right (−x).
    Right,
    /// Both, mirrored.
    Both,
}

/// How a clump follows the character while it moves.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Follow {
    /// Rigid to the head bone.
    Head,
    /// Hangs from the body: follows the head near the scalp and the chest further down (hair lies on the back).
    Body,
}

/// The scalp shell: dense short locks that cover the skull so no skin shows between locks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CapGen {
    /// Polar angle (from the top) where the hairline ends at the front.
    pub hairline_front_deg: f64,
    /// Polar angle where the shell ends at the back (down to the nape).
    pub hairline_back_deg: f64,
    /// How far the shell is raised above the ear, in degrees.
    pub ear_notch_deg: f64,
    /// Shell offset from the skull at the hairline. The shell rises by `crown_rise` of it toward the crown, so the
    /// crown stands `(1 + crown_rise) * thickness` off the skull.
    pub thickness: f64,
    /// The range of `cos(phi)` (`phi` the azimuth from the front) over which the hairline eases from the back
    /// height to the front one: what counts as the forehead.
    pub front_blend: [f64; 2],
    /// Width of the ear notch along the side of the head, in `cos^2(phi)`.
    pub notch_width: f64,
    /// The range of `cos(phi)` over which the ear notch fades in from the back to the front half of the head.
    pub notch_edges: [f64; 2],
    /// How much higher the shell stands at the crown than at the hairline, as a fraction of `thickness`.
    pub crown_rise: f64,
    /// `hair-info.x`: the shell's strand seed, `0..=1`.
    pub strand_seed: f64,
    /// `hair-info.y`: the shell's shade, the underlayer sitting in the shadow of the locks, `0.5..=1`.
    pub shade: f64,
}

impl Default for CapGen {
    fn default() -> Self {
        Self {
            hairline_front_deg: 58.0,
            hairline_back_deg: 118.0,
            ear_notch_deg: 16.0,
            thickness: 0.016,
            front_blend: [-0.2, 0.75],
            notch_width: 0.10,
            notch_edges: [-0.6, 0.2],
            crown_rise: 0.625,
            strand_seed: 0.37,
            shade: 0.93,
        }
    }
}

/// Locks that sweep from the part line down each side of the head to the braid line or behind the ear.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PartSweepGen {
    /// Where the part line starts at the front, degrees from the vertical (negative is forward).
    pub part_front_deg: f64,
    /// Where it ends toward the crown.
    pub part_back_deg: f64,
    /// Locks per side.
    pub count: u16,
    /// Around-the-head angle where the first lock ends.
    pub end_from_deg: f64,
    /// Where the last lock ends.
    pub end_to_deg: f64,
    /// Head-local height where the locks end at the front.
    pub end_height_front: f64,
    /// Head-local height where the locks end at the back.
    pub end_height_back: f64,
    /// Clump width.
    pub width: Taper,
    /// Clump thickness.
    pub thickness: Bulge,
    /// Where on the part line the first lock starts, as a fraction of its length.
    pub part_start: f64,
    /// How far each side's roots are nudged apart across the part, metres.
    pub part_spread: f64,
    /// Exponent spreading the end azimuths: below 1 packs more locks toward the temple.
    pub end_spread: f64,
    /// Exponent easing a lock along its path: below 1 leaves the part line quickly.
    pub sweep_ease: f64,
    /// Azimuth range, degrees from the front, over which the end height drops from its front value to its back one.
    pub height_blend_deg: [f64; 2],
    /// Height of a lock above the head at its ends, metres.
    pub lift: f64,
    /// Extra height at the middle of a lock, metres.
    pub belly: f64,
    /// Extra height of every other lock, so neighbours stack instead of fighting, metres.
    pub alternate_step: f64,
    /// Shade of the even and the odd locks, `0.5..=1`.
    pub shades: [f64; 2],
}

impl Default for PartSweepGen {
    fn default() -> Self {
        Self {
            part_front_deg: -32.0,
            part_back_deg: 42.0,
            count: 26,
            end_from_deg: 62.0,
            end_to_deg: 178.0,
            end_height_front: 0.04,
            end_height_back: -0.04,
            width: Taper { start: 0.05, mid: 0.06, tip: 0.03, tip_at: 0.8 },
            thickness: Bulge { base: 0.01, swell: 0.004 },
            part_start: 0.02,
            part_spread: 0.02,
            end_spread: 0.9,
            sweep_ease: 0.9,
            height_blend_deg: [70.0, 180.0],
            lift: 0.030,
            belly: 0.016,
            alternate_step: 0.005,
            shades: [0.97, 1.0],
        }
    }
}

/// Locks that frame the face: from the part over the forehead, past the eyes, then tucked behind the ear or hanging.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FramingGen {
    /// Which side.
    pub side: HairSide,
    /// Locks per side.
    pub count: u8,
    /// Gap between neighbouring locks where they leave the forehead.
    pub spread: f64,
    /// The inner edge of the framing hair as `[head-local y, x]` pairs from the forehead down to the jaw: the lock
    /// follows this outline on the head surface (traced from the sheet's front view, CB-32).
    pub edge: Vec<[f64; 2]>,
    /// Free points continuing after the outline: behind the ear, or hanging in front of the shoulder.
    pub tail: Vec<GuidePoint>,
    /// Sway added along the tail.
    pub wave: Wave,
    /// Clump width.
    pub width: Taper,
    /// Clump thickness.
    pub thickness: Bulge,
    /// Hand-placed lock heights (the prototype's tuck, ear and drop positions). `None` builds each lock from `edge`
    /// and `tail` alone, so a file without them keeps its meaning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locks: Option<FramingLocks>,
}

impl Default for FramingGen {
    fn default() -> Self {
        Self {
            side: HairSide::Both,
            count: 3,
            spread: 0.024,
            edge: vec![[0.12, 0.012], [0.0, 0.08], [-0.058, 0.136], [-0.12, 0.17], [-0.2, 0.192]],
            tail: vec![GuidePoint::Head([0.2, -0.18, 0.04]), GuidePoint::Head([0.22, -0.3, 0.02])],
            wave: Wave::default(),
            width: Taper { start: 0.03, mid: 0.05, tip: 0.006, tip_at: 0.8 },
            thickness: Bulge { base: 0.006, swell: 0.004 },
            locks: None,
        }
    }
}

/// The heights and points a framing lock passes through, which `edge` and `tail` cannot say: the prototype's
/// hand-placed `Y_TOP`, `Y_UPPER`, `Y_LOWER`, `Y_CHEEK`, `Y_TUCK_A`, `Y_TUCK_B`, `X_TUCK_MAX`, `EAR_TOP`, `EAR_BACK`,
/// `DROP` and `Y_END`. Every height and point is head-local (metres from the head centre); `x` is for the character's
/// left (+x) and is mirrored for the right. The prototype's `RH` is not stored: it is the head's own half-width.
///
/// The left side tucks locks `1..` behind the ear and lets lock `0` frame the cheek; the right side hangs every lock
/// to its `end` height.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct FramingLocks {
    /// Height where the forehead part of the lock starts to move outward with its index.
    pub top: f64,
    /// Heights down the forehead, descending, `2..=8`.
    pub upper: Vec<f64>,
    /// Heights of the points beside the face of a lock that does not tuck, descending, at most 6.
    pub lower: Vec<f64>,
    /// Height where the cheek-framing and hanging locks start.
    pub cheek: f64,
    /// Height whose outline `x` the first tuck point is read at.
    pub tuck_a: f64,
    /// Height of the first tuck point.
    pub tuck_b: f64,
    /// Largest `x` of the first tuck point.
    pub tuck_max_x: f64,
    /// `[x, y, z]` over the top of the ear.
    pub ear_top: [f64; 3],
    /// `[x, y, z]` behind the ear.
    pub ear_back: [f64; 3],
    /// `[x, y, z]` points down the back of the ear, `1..=8`; the last `y` rises by `0.02` per lock index.
    pub drop: Vec<[f64; 3]>,
    /// Where each hanging lock ends, by lock index (the last entry serves any further lock), `1..=12`.
    pub end: Vec<f64>,
}

impl FramingLocks {
    /// The prototype's arrangement scaled to the default head (`FramingGen::default`).
    pub fn new() -> Self {
        Self {
            top: 0.12,
            upper: vec![0.12, 0.08, 0.04, 0.0, -0.058],
            lower: vec![-0.12, -0.16, -0.2],
            cheek: -0.2,
            tuck_a: -0.12,
            tuck_b: -0.14,
            tuck_max_x: 0.19,
            ear_top: [0.2, -0.02, -0.02],
            ear_back: [0.2, -0.05, -0.08],
            drop: vec![[0.19, -0.1, -0.1], [0.18, -0.16, -0.12], [0.17, -0.22, -0.12], [0.16, -0.3, -0.12]],
            end: vec![-0.4, -0.45, -0.5, -0.55, -0.6],
        }
    }
}

impl Default for FramingLocks {
    fn default() -> Self {
        Self::new()
    }
}

impl Validate for FramingLocks {
    fn check(&self, c: &mut Check) {
        let h = |c: &mut Check, name: &str, v: f64| c.range(name, v, -1.5, 1.0);
        h(c, "top", self.top);
        h(c, "cheek", self.cheek);
        h(c, "tuck_a", self.tuck_a);
        h(c, "tuck_b", self.tuck_b);
        c.range("tuck_max_x", self.tuck_max_x, 0.0, 1.0);
        let list = |c: &mut Check, name: &str, values: &[f64], lo: usize, hi: usize, descending: bool| {
            if !(lo..=hi).contains(&values.len()) {
                c.fail(name, format!("{} entries is out of range: use {lo} to {hi}", values.len()));
            }
            for (i, v) in values.iter().enumerate() {
                c.range(&format!("{name}/{i}"), *v, -1.5, 1.0);
            }
            if descending && values.windows(2).any(|w| w[1] > w[0]) {
                c.fail(name, "heights must descend");
            }
        };
        list(c, "upper", &self.upper, 2, 8, true);
        list(c, "lower", &self.lower, 0, 6, true);
        list(c, "end", &self.end, 1, 12, false);
        let point = |c: &mut Check, name: &str, p: &[f64; 3]| {
            for (axis, v) in ["x", "y", "z"].iter().zip(p) {
                c.range(&format!("{name}/{axis}"), *v, -1.5, 1.5);
            }
        };
        point(c, "ear_top", &self.ear_top);
        point(c, "ear_back", &self.ear_back);
        if !(1..=8).contains(&self.drop.len()) {
            c.fail("drop", format!("{} points is out of range: use 1 to 8", self.drop.len()));
        }
        for (i, p) in self.drop.iter().enumerate() {
            point(c, &format!("drop/{i}"), p);
        }
    }
}

/// A braid crown: several interleaved strands following a path round the head.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BraidGen {
    /// Around-the-head angle where the braid starts (the temple).
    pub path_from_deg: f64,
    /// Where it ends (past the back centre, overlapping the other side).
    pub path_to_deg: f64,
    /// Head-local height of the path at the front.
    pub height_front: f64,
    /// Head-local height of the path at the back.
    pub height_back: f64,
    /// Interleaved strands, `2..=5`.
    pub strands: u8,
    /// Lobes along the whole path.
    pub lobes: f64,
    /// Sideways swing of a strand.
    pub across: f64,
    /// Outward bulge of a strand.
    pub out: f64,
    /// Width of one strand.
    pub strand_width: f64,
    /// Thickness of one strand.
    pub strand_thickness: f64,
}

impl Default for BraidGen {
    fn default() -> Self {
        Self {
            path_from_deg: 64.0,
            path_to_deg: 182.0,
            height_front: 0.04,
            height_back: -0.043,
            strands: 3,
            lobes: 9.0,
            across: 0.0135,
            out: 0.0055,
            strand_width: 0.024,
            strand_thickness: 0.014,
        }
    }
}

/// The long hair: a curtain of guide clumps rooted round the back of the head, each owning a strip of columns that
/// reach halfway to its neighbours, so the bottom splits into pointed clumps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CurtainGen {
    /// Guide clumps across the back.
    pub count: u16,
    /// Half-angle round the back they spread over, degrees.
    pub spread_deg: f64,
    /// Head-local root height at the sides.
    pub root_height_front: f64,
    /// Head-local root height at the back.
    pub root_height_back: f64,
    /// Hair length at the sides.
    pub length_front: f64,
    /// Hair length at the back.
    pub length_back: f64,
    /// How far the hair stands from the body axis, as `[bind-space y, distance]` pairs from the shoulders down
    /// (traced from the sheet's silhouette).
    pub envelope: Vec<[f64; 2]>,
    /// The wave and tip curl.
    pub wave: Wave,
    /// Columns per strip (the width of the curtain's clump separation), `1..=9`.
    pub columns: u8,
    /// Clump width.
    pub width: Taper,
    /// Clump thickness.
    pub thickness: Bulge,
}

impl Default for CurtainGen {
    fn default() -> Self {
        Self {
            count: 44,
            spread_deg: 118.0,
            root_height_front: 0.05,
            root_height_back: -0.04,
            length_front: 0.47,
            length_back: 0.56,
            envelope: vec![[0.7, 0.2], [0.5, 0.31], [0.3, 0.34], [0.2, 0.32]],
            wave: Wave { amplitude: 0.036, frequency: 2.1, phase: 0.0, curl: 0.045, growth: [0.15, 0.65] },
            columns: 5,
            width: Taper { start: 0.066, mid: 0.095, tip: 0.014, tip_at: 0.8 },
            thickness: Bulge { base: 0.006, swell: 0.004 },
        }
    }
}

/// One layer of bangs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BangLayer {
    /// Clumps in the layer.
    pub count: u8,
    /// Half-angle of the fan across the forehead, degrees.
    pub spread_deg: f64,
    /// Head-local height of each clump's tip, left to right across the forehead (the last value repeats if there are
    /// fewer values than clumps). Different lengths per lock give the fringe its rhythm.
    pub tip_heights: Vec<f64>,
    /// Width of a clump at its widest.
    pub width: f64,
    /// How far the tips hook inward, degrees (more at the outer clumps).
    pub hook_deg: f64,
    /// Distance from the skull at the root.
    pub root_offset: f64,
    /// Distance from the skull at the tip.
    pub tip_offset: f64,
    /// Shade multiplier (under-layers sit darker), `0.5..=1`.
    pub shade: f64,
}

impl Default for BangLayer {
    fn default() -> Self {
        Self {
            count: 9,
            spread_deg: 44.0,
            tip_heights: vec![0.0],
            width: 0.064,
            hook_deg: 3.0,
            root_offset: 0.046,
            tip_offset: 0.009,
            shade: 1.0,
        }
    }
}

/// A loose wisp falling between the eyes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Wisp {
    /// Angle from the front, degrees (positive toward the character's left).
    pub phi_deg: f64,
    /// Head-local height of its tip.
    pub tip_height: f64,
    /// Width.
    pub width: f64,
}

impl Default for Wisp {
    fn default() -> Self {
        Self { phi_deg: 0.0, tip_height: -0.05, width: 0.022 }
    }
}

/// The fringe (CB-31): layers of clumps fanning from the part, and loose wisps. With `brows_through` shading, the
/// brows show through it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BangsGen {
    /// Layers, drawn under to over (broad short under-layers first, pointed top layer last).
    pub layers: Vec<BangLayer>,
    /// Loose wisps.
    pub wisps: Vec<Wisp>,
    /// How far the whole fringe sweeps toward the character's right, degrees.
    pub sweep_deg: f64,
}

impl Default for BangsGen {
    fn default() -> Self {
        Self { layers: vec![BangLayer::default()], wisps: Vec::new(), sweep_deg: 7.0 }
    }
}

/// A ponytail or twintail (a new design: the prototype has none). Twintails are a tail group with `mirror` set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TailGen {
    /// Where the tail is tied, a surface point `[theta_deg, phi_deg, offset]`.
    pub root: [f64; 3],
    /// Length.
    pub length: f64,
    /// Clumps in the tail.
    pub count: u8,
    /// Angle the clumps fan over, degrees.
    pub spread_deg: f64,
    /// How far it droops under gravity at rest, `0..=1`.
    pub droop: f64,
    /// Waviness.
    pub wave: Wave,
    /// Clump width.
    pub width: Taper,
    /// Clump thickness.
    pub thickness: Bulge,
    /// Radius of the bunch at the tie, where the clumps are gathered, in metres.
    pub tie_radius: f64,
    /// Bones in the tail's spring chain (`4..=6`), root to tip.
    pub chain_bones: u8,
}

impl Default for TailGen {
    fn default() -> Self {
        Self {
            root: [80.0, 150.0, 0.03],
            length: 0.4,
            count: 8,
            spread_deg: 14.0,
            droop: 0.6,
            wave: Wave::default(),
            width: Taper::default(),
            thickness: Bulge::default(),
            tie_radius: 0.012,
            chain_bones: 5,
        }
    }
}

/// A bun (a new design: the prototype has none): strands wound round a ball that sits on the head.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BunGen {
    /// Where it sits, a surface point `[theta_deg, phi_deg, offset]`.
    pub centre: [f64; 3],
    /// Radius of the bun.
    pub radius: f64,
    /// Strands wound round it.
    pub strands: u8,
    /// Turns of each strand.
    pub wraps: f64,
    /// Strand width.
    pub width: Taper,
    /// Strand thickness.
    pub thickness: Bulge,
}

impl Default for BunGen {
    fn default() -> Self {
        Self {
            centre: [30.0, 180.0, 0.02],
            radius: 0.05,
            strands: 6,
            wraps: 1.5,
            width: Taper { start: 0.02, mid: 0.034, tip: 0.012, tip_at: 0.5 },
            thickness: Bulge { base: 0.012, swell: 0.006 },
        }
    }
}

/// Explicit clumps: hand-edited or traced guide curves (CB-32). Every other generator can be baked into these.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GuidesGen {
    /// The clumps.
    pub clumps: Vec<GuideClump>,
}

/// One explicit clump.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GuideClump {
    /// Control points root first; at least two. The builder fits a smooth curve through them.
    pub points: Vec<GuidePoint>,
    /// Width profile.
    pub width: Taper,
    /// Thickness profile.
    pub thickness: Bulge,
    /// Shade multiplier, `0.5..=1`.
    pub shade: f64,
}

impl Default for GuideClump {
    fn default() -> Self {
        Self { points: Vec::new(), width: Taper::default(), thickness: Bulge::default(), shade: 1.0 }
    }
}

/// What a group makes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Generator {
    /// The scalp shell.
    Cap(CapGen),
    /// Locks from the part line down the sides.
    PartSweep(PartSweepGen),
    /// Locks framing the face.
    Framing(FramingGen),
    /// A braid crown.
    Braid(BraidGen),
    /// The long curtain.
    Curtain(CurtainGen),
    /// The fringe.
    Bangs(BangsGen),
    /// A ponytail or twintail.
    Tail(TailGen),
    /// A bun.
    Bun(BunGen),
    /// Explicit guide curves.
    Guides(GuidesGen),
}

/// How a group moves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Dynamics {
    /// Simulated (hair physics) or rigid to the head.
    pub simulate: bool,
    /// What the group hangs from.
    pub follow: Follow,
    /// How firmly each point is pulled back to the styled shape: 1 rigid, 0 free, root to tip.
    pub stiffness: Falloff,
}

impl Default for Dynamics {
    fn default() -> Self {
        Self { simulate: false, follow: Follow::Head, stiffness: Falloff::default() }
    }
}

/// One group of a hairstyle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HairGroup {
    /// Skip the group without deleting it (CB-91: hair variants side by side).
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Also build the mirror image across the midline: the group's clumps, then their reflections, so one side defines
    /// both. A generator that already makes both sides of the head (`is_bilateral` in the builder: the cap, part
    /// sweep, braid crown, curtain and bangs, and framing at `Both`) ignores it.
    #[serde(default)]
    pub mirror: bool,
    /// Motion.
    #[serde(default)]
    pub dynamics: Dynamics,
    /// What the group generates.
    pub generator: Generator,
}

fn yes() -> bool {
    true
}

// ------------------------------------------------------------------------------------------------ shading and physics

/// Hair shading (CB-30, CB-34): a root-to-tip gradient with strand lines and a painted angel ring. These feed the
/// renderer's `"hair"` shading program ([`HairShading::shading`]); the hair meshes carry the matching [`HAIR_UV`] and
/// [`HAIR_INFO`] extra attributes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HairShading {
    /// Colour at the root.
    pub root: ColourRef,
    /// Colour in the middle.
    pub mid: ColourRef,
    /// Colour at the tip.
    pub tip: ColourRef,
    /// The colour on the shadowed side.
    pub shade: ColourRef,
    /// Strand-line colour.
    pub line: ColourRef,
    /// Outline colour. The default is the prototype's hair outline, `#a47868` (hair's own, not the `#8c685c` line colour).
    pub outline: ColourRef,
    /// Angel-ring highlight colour.
    pub ring: ColourRef,
    /// Strength of the angel ring, `0..=1`.
    pub ring_strength: f64,
    /// Strength of the strand lines, `0..=1`.
    pub strand_lines: f64,
    /// How much darker each clump's edges are, so overlapping locks separate, `0..=1`.
    pub edge_darkening: f64,
    /// How much of the hair's own colour survives strongly coloured light, `0..=1` ("keep identity", CB-34).
    pub keep_identity: f64,
    /// Outline thickness in metres. The default, 0.0014, is the prototype's hair outline.
    pub outline_width: f64,
    /// Draw the brows over the hair (the anime see-through bangs convention).
    pub brows_through: bool,
    /// Coloured streaks laid over shares of the clumps, applied in order (CB-34). See [`HairShading::apply_to_clumps`].
    pub streaks: Vec<Streak>,
    /// How much of the prototype's inner-layer darkening is applied, `0..=1`: `1` (the default) is the designed
    /// darkening, so a clump authored darker (`shade < 1`) keeps its shade; `0` is none, and every clump is drawn at
    /// full shade `1.0`. Applied to [`HairClump::shade`] on the CPU as `shade += (1 - d) * (1 - shade)`, so the hair
    /// program's parameters don't change.
    pub inner_darkening: f64,
}

impl Default for HairShading {
    fn default() -> Self {
        Self {
            root: ColourRef::rgb(0xc8, 0xb0, 0x9a),
            mid: ColourRef::rgb(0xe6, 0xd2, 0xbe),
            tip: ColourRef::rgb(0xdc, 0xc4, 0xb0),
            shade: ColourRef::rgb(0xb0, 0x8e, 0x88),
            line: ColourRef::rgb(0x8c, 0x68, 0x5c),
            outline: ColourRef::rgb(0xa4, 0x78, 0x68),
            ring: ColourRef::rgb(0xff, 0xf4, 0xe8),
            ring_strength: 0.22,
            strand_lines: 0.10,
            edge_darkening: 0.14,
            keep_identity: 0.36,
            outline_width: 0.0014,
            brows_through: true,
            streaks: Vec::new(),
            inner_darkening: 1.0,
        }
    }
}

/// A streak (CB-34): a coloured lock laid over a share of the clumps. Streaks apply in
/// order, so a later one draws over an earlier one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Streak {
    /// Share of the eligible clumps that take the streak, `0..=1`. Each clump is picked by a hash of `seed` and its
    /// position among the eligible clumps, so the same seed picks the same clumps on every build.
    pub fraction: f64,
    /// The streak colour: a palette slot or a literal.
    pub colour: ColourRef,
    /// How strongly the colour takes effect, `0..=1`.
    pub strength: f64,
    /// The groups it applies to. Empty means every group.
    pub groups: Vec<GroupId>,
    /// Chooses which clumps take the streak.
    pub seed: u32,
    /// How the colour combines with the hair under it.
    pub blend: StreakBlend,
}

impl Default for Streak {
    fn default() -> Self {
        Self {
            fraction: 0.2,
            colour: ColourRef::rgb(0x9b, 0x6e, 0xd6),
            strength: 0.6,
            groups: Vec::new(),
            seed: 1,
            blend: StreakBlend::default(),
        }
    }
}

impl Streak {
    /// The colour after this streak, given the colour so far (linear light).
    fn paint(&self, current: Vec3, colour: Vec3) -> Vec3 {
        let strength = narrow(self.strength);
        match self.blend {
            StreakBlend::Replace => current.lerp(colour, strength),
            StreakBlend::Multiply => current * Vec3::ONE.lerp(colour, strength),
        }
    }
}

impl Validate for Streak {
    fn check(&self, c: &mut Check) {
        c.range("fraction", self.fraction, 0.0, 1.0);
        c.range("strength", self.strength, 0.0, 1.0);
    }
}

/// How a streak's colour combines with the hair under it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StreakBlend {
    /// Mixes toward the streak colour by `strength`: the hair is replaced where the streak lies.
    #[default]
    Replace,
    /// Multiplies the hair by the streak colour, faded in by `strength`: a tint that keeps the hair's shading.
    Multiply,
}

/// A deterministic number in `0..1` for the `index`-th eligible clump of a streak with `seed`: one step of the core
/// `SplitMix` generator, so the same seed picks the same clumps on every build.
fn unit(seed: u32, index: usize) -> f64 {
    let index = u64::try_from(index).unwrap_or(u64::MAX);
    f64::from(Rng::new((u64::from(seed) << 32) | index).next_f32())
}

/// Why hair shading can't be turned into a renderer shading.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HairShadingError {
    /// A colour slot the palette doesn't define.
    #[error(transparent)]
    Colour(#[from] ColourError),
    /// An internal parameter name isn't a valid slug (a bug).
    #[error("internal program name: {0}")]
    Name(#[from] SlugError),
}

impl HairShading {
    /// The renderer's shading for this hair: `Shading::Program { name: "hair", params }`. Parameters, all read by the
    /// program: colours `root`, `mid`, `tip`, `shade`, `line`, `ring` (resolved through `palette`) and floats
    /// `ring-strength`, `strand-lines`, `edge-darkening`, `keep-identity`. The outline width and `brows_through` are not
    /// shader parameters: the first goes on the material's `Outline`, the second adds the brow overlay.
    ///
    /// # Errors
    /// [`HairShadingError`] for an undefined palette slot.
    pub fn shading(&self, palette: &Palette) -> Result<Shading, HairShadingError> {
        let mut params = ProgramParams::new();
        for (name, colour) in [
            ("root", &self.root),
            ("mid", &self.mid),
            ("tip", &self.tip),
            ("shade", &self.shade),
            ("line", &self.line),
            ("ring", &self.ring),
        ] {
            params.insert(Slug::new(name)?, ParamValue::Colour(palette.resolve(colour)?));
        }
        for (name, value) in [
            ("ring-strength", self.ring_strength),
            ("strand-lines", self.strand_lines),
            ("edge-darkening", self.edge_darkening),
            ("keep-identity", self.keep_identity),
        ] {
            params.insert(Slug::new(name)?, ParamValue::Float(narrow(value)));
        }
        Ok(Shading::Program { name: Slug::new(HAIR_PROGRAM)?, params })
    }

    /// The generator pass that runs once every group has compiled, before the clumps become meshes. Streaks pick clumps
    /// across groups, so this can't run per group. It applies [`inner_darkening`](Self::inner_darkening) to each
    /// clump's `shade` and compiles the streaks into each clump's [`HairClump::tint`].
    ///
    /// A streak is compiled against the hair's `mid` colour: the tint is the streak's final colour on the strands. The
    /// mesh carries it as vertex colours. Deterministic: the same clumps and shading give the same result.
    ///
    /// # Errors
    /// [`HairShadingError`] for an undefined palette slot.
    pub fn apply_to_clumps(&self, palette: &Palette, clumps: &mut [HairClump]) -> Result<(), HairShadingError> {
        let lift = narrow(1.0 - self.inner_darkening);
        for clump in clumps.iter_mut() {
            // At the default `1` the added term is exactly zero, so the shade stays bit-identical, not merely close.
            clump.shade += lift * (1.0 - clump.shade);
        }
        let groups: Vec<&GroupId> = clumps.iter().map(|c| &c.group).collect();
        let mut tints: Vec<Option<Colour>> = clumps.iter().map(|c| c.tint).collect();
        self.paint_streaks(palette, &groups, &mut tints)?;
        for (clump, tint) in clumps.iter_mut().zip(tints) {
            clump.tint = tint;
        }
        Ok(())
    }

    /// The tint [`apply_to_clumps`](Self::apply_to_clumps) gives each clump, for clumps that lie in `groups` (one entry
    /// per clump, in clump order): `None` where no streak touches the clump. These are the very numbers it writes into
    /// [`HairClump::tint`], computed from the groups alone, so built hair can be re-tinted when the palette changes
    /// without compiling its clumps again ([`PartKind::recolour`](crate::PartKind::recolour)).
    ///
    /// # Errors
    /// [`HairShadingError`] for an undefined palette slot.
    pub fn streak_tints(
        &self,
        palette: &Palette,
        groups: &[&GroupId],
    ) -> Result<Vec<Option<Colour>>, HairShadingError> {
        let mut tints = vec![None; groups.len()];
        self.paint_streaks(palette, groups, &mut tints)?;
        Ok(tints)
    }

    /// Lays the streaks, in order, over `tints` (one per clump, `groups[i]` being clump `i`'s group).
    fn paint_streaks(
        &self,
        palette: &Palette,
        groups: &[&GroupId],
        tints: &mut [Option<Colour>],
    ) -> Result<(), HairShadingError> {
        let mid = palette.resolve(&self.mid)?.to_linear();
        for streak in &self.streaks {
            let colour = palette.resolve(&streak.colour)?.to_linear();
            let eligible = groups
                .iter()
                .zip(tints.iter_mut())
                .filter(|(group, _)| streak.groups.is_empty() || streak.groups.contains(group));
            for (index, (_, tint)) in eligible.enumerate() {
                if unit(streak.seed, index) < streak.fraction {
                    let current = tint.map_or(mid, Colour::to_linear);
                    *tint = Some(Colour::from_linear(streak.paint(current, colour), 1.0));
                }
            }
        }
        Ok(())
    }
}

impl Validate for HairShading {
    fn check(&self, c: &mut Check) {
        c.range("ring_strength", self.ring_strength, 0.0, 1.0);
        c.range("strand_lines", self.strand_lines, 0.0, 1.0);
        c.range("edge_darkening", self.edge_darkening, 0.0, 1.0);
        c.range("keep_identity", self.keep_identity, 0.0, 1.0);
        c.range("outline_width", self.outline_width, 0.0, 0.01);
        c.range("inner_darkening", self.inner_darkening, 0.0, 1.0);
        for (i, streak) in self.streaks.iter().enumerate() {
            c.nested(&format!("streaks/{i}"), streak);
        }
    }
}

/// A sphere that hair must not enter (head, neck, shoulders, chest, back).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SphereCollider {
    /// The bone it follows.
    pub bone: JointRef,
    /// Centre, bind space.
    pub centre: [f64; 3],
    /// Radius.
    pub radius: f64,
}

/// Hair physics (CB-33): Verlet chains with a spring back to the styled shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HairPhysics {
    /// Fixed simulation rate in hertz (spec §8: 90 to 120).
    pub fixed_hz: f64,
    /// Velocity kept each step, `0.5..=1` (1 never settles).
    pub damping: f64,
    /// Gravity multiplier.
    pub gravity_scale: f64,
    /// Constraint passes per step.
    pub iterations: u8,
    /// Extra clearance around colliders, in metres.
    pub collider_margin: f64,
    /// How strongly wind pushes the hair.
    pub wind_scale: f64,
    /// Colliders of its own. Empty means "use the rig's colliders" (`omuzaki-anim-api`).
    pub colliders: Vec<SphereCollider>,
}

impl Default for HairPhysics {
    fn default() -> Self {
        Self {
            fixed_hz: 90.0,
            damping: 0.965,
            gravity_scale: 0.35,
            iterations: 2,
            collider_margin: 0.008,
            wind_scale: 1.0,
            colliders: Vec::new(),
        }
    }
}

impl Validate for HairPhysics {
    fn check(&self, c: &mut Check) {
        c.range("fixed_hz", self.fixed_hz, 30.0, 240.0);
        c.range("damping", self.damping, 0.5, 1.0);
        c.range("gravity_scale", self.gravity_scale, 0.0, 3.0);
        c.range("collider_margin", self.collider_margin, 0.0, 0.05);
        c.range("wind_scale", self.wind_scale, 0.0, 5.0);
        if !(1..=8).contains(&self.iterations) {
            c.fail("iterations", format!("{} is out of range: use 1 to 8", self.iterations));
        }
        for (i, col) in self.colliders.iter().enumerate() {
            c.range(&format!("colliders/{i}/radius"), col.radius, 0.005, 0.5);
        }
    }
}

// ------------------------------------------------------------------------------------------------ the part

/// Where a hairstyle's random jitter comes from (decision G6 of the phase-1 wave-3 plan).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HairRng {
    /// Each part draws from its own forked stream (D11), so a rebuild is identical and adding a part never shifts another
    /// part's hair. New characters use this.
    #[default]
    Forked,
    /// Parity mode only: reproduces the prototype's stream for the golden suite. Every group draws from one shared
    /// [`LegacyLcg`], in id order. New characters use [`Forked`](Self::Forked).
    LegacyLcg {
        /// The stream's starting state, `1..=2147483646` (the prototype starts at 7).
        seed: u32,
    },
}

impl Validate for HairRng {
    fn check(&self, c: &mut Check) {
        match *self {
            Self::LegacyLcg { seed } if !LegacyLcg::is_valid_seed(seed) => {
                c.fail("seed", format!("{seed} is out of range: use 1 to {}", LegacyLcg::MAX_SEED));
            }
            _ => {}
        }
    }
}

/// The prototype's random stream (`hair.js`, `rnd`): Park-Miller, `seed = seed * 16807 mod 2147483647`, each value
/// `(seed - 1) / 2147483646` in `0..1`. Only [`HairRng::LegacyLcg`] uses it, so goldens from the prototype still match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyLcg {
    state: u64,
}

impl LegacyLcg {
    /// The modulus of the generator: a prime, so the state never reaches 0 from a valid seed.
    const MODULUS: u64 = 2_147_483_647;
    /// The largest valid starting state (the modulus minus one).
    pub const MAX_SEED: u32 = 2_147_483_646;

    /// Whether `seed` can start the stream: 0 would stay 0 forever.
    pub fn is_valid_seed(seed: u32) -> bool {
        (1..=Self::MAX_SEED).contains(&seed)
    }

    /// The stream started at `seed`, or `None` if [`is_valid_seed`](Self::is_valid_seed) is false.
    pub fn new(seed: u32) -> Option<Self> {
        Self::is_valid_seed(seed).then(|| Self { state: u64::from(seed) })
    }

    /// The next value in `0..1`, exactly as the prototype computes it in double precision.
    #[allow(clippy::cast_precision_loss)] // the state is below 2^31, so it fits an f64 exactly
    pub fn next_unit(&mut self) -> f64 {
        self.state = self.state * 16_807 % Self::MODULUS;
        (self.state - 1) as f64 / f64::from(Self::MAX_SEED)
    }
}

/// The parameters of the `hair` part kind: one hairstyle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HairParams {
    /// Colours and shading.
    pub shading: HairShading,
    /// Simulation.
    pub physics: HairPhysics,
    /// The groups, by id. Groups build in id order; later groups draw over earlier ones.
    pub groups: BTreeMap<GroupId, HairGroup>,
    /// Where the random jitter comes from. The default is the per-part forked stream; `LegacyLcg` is parity mode.
    pub rng: HairRng,
}

impl Default for HairParams {
    /// A small, readable style: a scalp shell, a fringe and a simulated curtain.
    fn default() -> Self {
        let group = |name: &str, simulate: bool, follow: Follow, generator: Generator| {
            let dynamics = Dynamics { simulate, follow, stiffness: Falloff::default() };
            GroupId::new(name).map(|id| (id, HairGroup { enabled: true, mirror: false, dynamics, generator }))
        };
        let groups = [
            group("a-cap", false, Follow::Head, Generator::Cap(CapGen::default())),
            group("b-bangs", false, Follow::Head, Generator::Bangs(BangsGen::default())),
            group("c-curtain", true, Follow::Body, Generator::Curtain(CurtainGen::default())),
        ]
        .into_iter()
        .filter_map(Result::ok)
        .collect();
        Self { shading: HairShading::default(), physics: HairPhysics::default(), groups, rng: HairRng::default() }
    }
}

impl Validate for HairParams {
    fn check(&self, c: &mut Check) {
        c.nested("shading", &self.shading);
        c.nested("physics", &self.physics);
        c.nested("rng", &self.rng);
        for (id, g) in &self.groups {
            let name = format!("groups/{id}");
            c.nested(&name, g);
        }
        for (i, streak) in self.shading.streaks.iter().enumerate() {
            for id in streak.groups.iter().filter(|id| !self.groups.contains_key(*id)) {
                c.fail(&format!("shading/streaks/{i}/groups"), format!("there is no group `{id}` to streak"));
            }
        }
    }
}

impl Validate for HairGroup {
    fn check(&self, c: &mut Check) {
        c.nested("dynamics/stiffness", &self.dynamics.stiffness);
        match &self.generator {
            Generator::Cap(g) => {
                c.range("generator/hairline_front_deg", g.hairline_front_deg, 20.0, 120.0);
                c.range("generator/hairline_back_deg", g.hairline_back_deg, 60.0, 150.0);
                c.range("generator/ear_notch_deg", g.ear_notch_deg, 0.0, 40.0);
                c.range("generator/thickness", g.thickness, 0.002, 0.06);
            }
            Generator::PartSweep(g) => {
                count("generator/count", c, u32::from(g.count), 1, 80);
                c.nested("generator/width", &g.width);
                c.nested("generator/thickness", &g.thickness);
            }
            Generator::Framing(g) => {
                count("generator/count", c, u32::from(g.count), 1, 12);
                if g.edge.len() < 2 {
                    c.fail("generator/edge", "needs at least two [y, x] points");
                }
                if let Some(locks) = &g.locks {
                    c.nested("generator/locks", locks);
                }
                for (i, p) in g.tail.iter().enumerate() {
                    c.nested(&format!("generator/tail/{i}"), p);
                }
                c.nested("generator/wave", &g.wave);
                c.nested("generator/width", &g.width);
                c.nested("generator/thickness", &g.thickness);
            }
            Generator::Braid(g) => {
                if !(2..=5).contains(&g.strands) {
                    c.fail("generator/strands", format!("{} is out of range: use 2 to 5", g.strands));
                }
                c.range("generator/lobes", g.lobes, 1.0, 30.0);
                c.positive("generator/strand_width", g.strand_width);
                c.positive("generator/strand_thickness", g.strand_thickness);
            }
            Generator::Curtain(g) => {
                count("generator/count", c, u32::from(g.count), 1, 120);
                count("generator/columns", c, u32::from(g.columns), 1, 9);
                c.range("generator/spread_deg", g.spread_deg, 20.0, 180.0);
                c.range("generator/length_front", g.length_front, 0.05, 1.5);
                c.range("generator/length_back", g.length_back, 0.05, 1.5);
                if g.envelope.len() < 2 {
                    c.fail("generator/envelope", "needs at least two [y, distance] points");
                }
                c.nested("generator/wave", &g.wave);
                c.nested("generator/width", &g.width);
                c.nested("generator/thickness", &g.thickness);
            }
            Generator::Bangs(g) => {
                if g.layers.is_empty() {
                    c.fail("generator/layers", "needs at least one layer");
                }
                for (i, l) in g.layers.iter().enumerate() {
                    count(&format!("generator/layers/{i}/count"), c, u32::from(l.count), 1, 30);
                    c.range(&format!("generator/layers/{i}/spread_deg"), l.spread_deg, 5.0, 90.0);
                    c.range(&format!("generator/layers/{i}/shade"), l.shade, 0.5, 1.0);
                    if l.tip_heights.is_empty() {
                        c.fail(&format!("generator/layers/{i}/tip_heights"), "needs at least one height");
                    }
                }
            }
            Generator::Tail(g) => {
                count("generator/count", c, u32::from(g.count), 1, 30);
                c.range("generator/length", g.length, 0.05, 1.5);
                c.range("generator/droop", g.droop, 0.0, 1.0);
                c.nested("generator/wave", &g.wave);
            }
            Generator::Bun(g) => {
                count("generator/strands", c, u32::from(g.strands), 1, 20);
                c.range("generator/radius", g.radius, 0.01, 0.2);
            }
            Generator::Guides(g) => {
                for (i, clump) in g.clumps.iter().enumerate() {
                    if clump.points.len() < 2 {
                        c.fail(&format!("generator/clumps/{i}/points"), "a clump needs at least two points");
                    }
                    for (k, p) in clump.points.iter().enumerate() {
                        c.nested(&format!("generator/clumps/{i}/points/{k}"), p);
                    }
                    c.nested(&format!("generator/clumps/{i}/width"), &clump.width);
                    c.nested(&format!("generator/clumps/{i}/thickness"), &clump.thickness);
                }
            }
        }
        check_generator_values(&self.generator, c);
    }
}

/// The range checks that the per-variant arms above do not make: every `f64` of a generator, and every point of its
/// lists, is finite and within a range where the field has one, so a value typed into a slider or a file can never
/// reach a generator as `NaN`, infinite or absurd (a class sweep: #1775). Angles are degrees within +-360, heights
/// and lengths metres within the head's scale (+-1.5), offsets as [`GuidePoint`]'s. Each generator's own counts and
/// shapes are the arms' business.
fn check_generator_values(generator: &Generator, c: &mut Check) {
    let g = |name: &str| format!("generator/{name}");
    match generator {
        Generator::Cap(x) => {
            for (i, v) in x.front_blend.iter().enumerate() {
                c.range(&format!("generator/front_blend/{i}"), *v, -1.0, 1.0);
            }
            for (i, v) in x.notch_edges.iter().enumerate() {
                c.range(&format!("generator/notch_edges/{i}"), *v, -1.0, 1.0);
            }
            c.range(&g("notch_width"), x.notch_width, 0.01, 1.0);
            c.range(&g("crown_rise"), x.crown_rise, 0.0, 3.0);
            c.range(&g("strand_seed"), x.strand_seed, 0.0, 1.0);
            c.range(&g("shade"), x.shade, 0.5, 1.0);
        }
        Generator::PartSweep(x) => {
            c.range(&g("part_front_deg"), x.part_front_deg, -180.0, 180.0);
            c.range(&g("part_back_deg"), x.part_back_deg, -180.0, 180.0);
            c.range(&g("end_from_deg"), x.end_from_deg, -360.0, 360.0);
            c.range(&g("end_to_deg"), x.end_to_deg, -360.0, 360.0);
            c.range(&g("end_height_front"), x.end_height_front, -HEAD_SCALE, HEAD_SCALE);
            c.range(&g("end_height_back"), x.end_height_back, -HEAD_SCALE, HEAD_SCALE);
            c.range(&g("part_start"), x.part_start, 0.0, 0.5);
            c.range(&g("part_spread"), x.part_spread, 0.0, 0.2);
            c.range(&g("end_spread"), x.end_spread, 0.1, 4.0);
            c.range(&g("sweep_ease"), x.sweep_ease, 0.1, 4.0);
            for (i, v) in x.height_blend_deg.iter().enumerate() {
                c.range(&format!("generator/height_blend_deg/{i}"), *v, 0.0, 180.0);
            }
            c.range(&g("lift"), x.lift, 0.0, 0.3);
            c.range(&g("belly"), x.belly, 0.0, 0.2);
            c.range(&g("alternate_step"), x.alternate_step, 0.0, 0.05);
            for (i, v) in x.shades.iter().enumerate() {
                c.range(&format!("generator/shades/{i}"), *v, 0.5, 1.0);
            }
        }
        Generator::Framing(x) => {
            c.range(&g("spread"), x.spread, 0.0, 0.2);
            for (i, p) in x.edge.iter().enumerate() {
                for (axis, v) in ["y", "x"].iter().zip(p) {
                    c.range(&format!("generator/edge/{i}/{axis}"), *v, -HEAD_SCALE, HEAD_SCALE);
                }
            }
        }
        Generator::Braid(x) => {
            c.range(&g("path_from_deg"), x.path_from_deg, -360.0, 360.0);
            c.range(&g("path_to_deg"), x.path_to_deg, -360.0, 360.0);
            c.range(&g("height_front"), x.height_front, -HEAD_SCALE, HEAD_SCALE);
            c.range(&g("height_back"), x.height_back, -HEAD_SCALE, HEAD_SCALE);
            c.range(&g("across"), x.across, -0.2, 0.2);
            c.range(&g("out"), x.out, -0.2, 0.2);
            c.range(&g("strand_width"), x.strand_width, 0.001, 0.3);
            c.range(&g("strand_thickness"), x.strand_thickness, 0.0005, 0.05);
        }
        Generator::Curtain(x) => {
            c.range(&g("root_height_front"), x.root_height_front, -HEAD_SCALE, HEAD_SCALE);
            c.range(&g("root_height_back"), x.root_height_back, -HEAD_SCALE, HEAD_SCALE);
            for (i, p) in x.envelope.iter().enumerate() {
                c.range(&format!("generator/envelope/{i}/y"), p[0], -3.0, 3.0);
                c.range(&format!("generator/envelope/{i}/distance"), p[1], 0.0, 2.0);
            }
        }
        Generator::Bangs(x) => {
            c.range(&g("sweep_deg"), x.sweep_deg, -90.0, 90.0);
            for (i, l) in x.layers.iter().enumerate() {
                for (k, v) in l.tip_heights.iter().enumerate() {
                    c.range(&format!("generator/layers/{i}/tip_heights/{k}"), *v, -HEAD_SCALE, HEAD_SCALE);
                }
                c.range(&format!("generator/layers/{i}/width"), l.width, 0.001, 0.3);
                c.range(&format!("generator/layers/{i}/hook_deg"), l.hook_deg, -90.0, 90.0);
                c.range(&format!("generator/layers/{i}/root_offset"), l.root_offset, -0.05, 0.3);
                c.range(&format!("generator/layers/{i}/tip_offset"), l.tip_offset, -0.05, 0.3);
            }
            for (i, w) in x.wisps.iter().enumerate() {
                c.range(&format!("generator/wisps/{i}/phi_deg"), w.phi_deg, -360.0, 360.0);
                c.range(&format!("generator/wisps/{i}/tip_height"), w.tip_height, -HEAD_SCALE, HEAD_SCALE);
                c.range(&format!("generator/wisps/{i}/width"), w.width, 0.001, 0.3);
            }
        }
        Generator::Tail(x) => {
            c.nested(&g("root"), &GuidePoint::Surface(x.root));
            c.range(&g("root/phi_deg"), x.root[1], -360.0, 360.0);
            c.range(&g("spread_deg"), x.spread_deg, 0.0, 120.0);
            c.range(&g("tie_radius"), x.tie_radius, 0.002, 0.05);
            count(&g("chain_bones"), c, u32::from(x.chain_bones), 4, 6);
            c.nested(&g("width"), &x.width);
            c.nested(&g("thickness"), &x.thickness);
        }
        Generator::Bun(x) => {
            c.nested(&g("centre"), &GuidePoint::Surface(x.centre));
            c.range(&g("centre/phi_deg"), x.centre[1], -360.0, 360.0);
            c.range(&g("wraps"), x.wraps, 0.0, 6.0);
            c.nested(&g("width"), &x.width);
            c.nested(&g("thickness"), &x.thickness);
        }
        Generator::Guides(x) => {
            for (i, clump) in x.clumps.iter().enumerate() {
                c.range(&format!("generator/clumps/{i}/shade"), clump.shade, 0.5, 1.0);
            }
        }
    }
}

/// The scale of a head in metres: heights and offsets of a generator are within this of the head's centre.
const HEAD_SCALE: f64 = 1.5;

fn count(name: &str, c: &mut Check, value: u32, lo: u32, hi: u32) {
    if !(lo..=hi).contains(&value) {
        c.fail(name, format!("{value} is out of range: use {lo} to {hi}"));
    }
}

// ------------------------------------------------------------------------------------------------ compiled clumps

/// A compiled clump: what every generator produces and every later stage (physics, tubes, export) consumes.
#[derive(Clone, Debug, PartialEq)]
pub struct HairClump {
    /// The group that made it.
    pub group: GroupId,
    /// Rest shape: bind-space points, root first, evenly spaced by arc length.
    pub rest: Vec<Vec3>,
    /// Width along the clump.
    pub width: Taper,
    /// Thickness along the clump.
    pub thickness: Bulge,
    /// Pull back to the rest shape along the clump (physics).
    pub stiffness: Falloff,
    /// Shade multiplier.
    pub shade: f32,
    /// Simulated, or rigid to the head.
    pub simulate: bool,
    /// What it hangs from.
    pub follow: Follow,
    /// The compiled streak colour (sRGB), or `None` for the hair's own colour. Set by
    /// [`HairShading::apply_to_clumps`]; the mesh carries it as vertex colours.
    pub tint: Option<Colour>,
}

impl HairClump {
    /// Total length of the rest curve.
    pub fn length(&self) -> f32 {
        self.rest.windows(2).map(|w| w[0].distance(w[1])).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_hit_their_control_values() {
        let t = Taper { start: 0.04, mid: 0.06, tip: 0.01, tip_at: 0.75 };
        assert!((t.at(0.0) - 0.04).abs() < 1e-6 && (t.at(0.75) - 0.06).abs() < 1e-6 && (t.at(1.0) - 0.01).abs() < 1e-6);
        assert_eq!(t.at(-1.0), t.at(0.0), "clamped");
        let b = Bulge { base: 0.006, swell: 0.004 };
        assert!(
            (b.at(0.0) - 0.006).abs() < 1e-6 && (b.at(0.5) - 0.01).abs() < 1e-6 && (b.at(1.0) - 0.006).abs() < 1e-5
        );
        let f = Falloff::default();
        assert_eq!(f.at(0.0), 1.0);
        assert!((f.at(1.0) - 0.07).abs() < 1e-6);
        assert!(f.at(0.5) < 1.0 && f.at(0.5) > 0.07, "monotone in between");
    }

    #[test]
    fn mirroring_flips_the_side_of_every_point_kind() {
        assert_eq!(GuidePoint::Surface([40.0, 30.0, 0.01]).mirrored(), GuidePoint::Surface([40.0, -30.0, 0.01]));
        assert_eq!(GuidePoint::Head([0.1, -0.2, 0.0]).mirrored(), GuidePoint::Head([-0.1, -0.2, 0.0]));
        let p = GuidePoint::Body([0.2, 0.3, -0.1]);
        assert_eq!(p.mirrored().mirrored(), p);
    }

    #[derive(Debug)]
    struct Ball;

    impl HeadSurface for Ball {
        fn centre(&self) -> Vec3 {
            Vec3::new(0.0, 1.0, 0.0)
        }

        fn radius(&self, _: Vec3) -> f32 {
            0.2
        }
    }

    #[test]
    fn surface_angles_start_at_the_top_and_the_front_and_go_left() {
        let at = |t: f64, p: f64, o: f64| GuidePoint::Surface([t, p, o]).resolve(&Ball);
        assert!(at(0.0, 0.0, 0.0).abs_diff_eq(Vec3::new(0.0, 1.2, 0.0), 1e-6), "theta 0 is the top");
        assert!(at(90.0, 0.0, 0.0).abs_diff_eq(Vec3::new(0.0, 1.0, 0.2), 1e-6), "phi 0 is the front (+z)");
        assert!(at(90.0, 90.0, 0.0).abs_diff_eq(Vec3::new(0.2, 1.0, 0.0), 1e-6), "phi 90 is the character's left (+x)");
        assert!(
            at(90.0, 180.0, 0.05).abs_diff_eq(Vec3::new(0.0, 1.0, -0.25), 1e-5),
            "phi 180 is the back, offset outward"
        );
        assert!(GuidePoint::Head([0.1, -0.2, 0.0]).resolve(&Ball).abs_diff_eq(Vec3::new(0.1, 0.8, 0.0), 1e-6));
        assert_eq!(GuidePoint::Body([0.1, 0.2, 0.3]).resolve(&Ball), Vec3::new(0.1, 0.2, 0.3));
    }

    #[test]
    fn hair_shading_becomes_the_renderers_hair_program() {
        let shading = HairShading::default().shading(&Palette::default()).unwrap();
        let Shading::Program { name, params } = &shading else { panic!("a program") };
        assert_eq!(name.as_str(), HAIR_PROGRAM);
        assert_eq!(params.len(), 10);
        assert!(
            matches!(params[&Slug::new("root").unwrap()], ParamValue::Colour(c) if c == omuzaki_core::colour::Colour::rgb(0xc8, 0xb0, 0x9a))
        );
        assert_eq!(params[&Slug::new("ring-strength").unwrap()], ParamValue::Float(0.22));
        let slotted = HairShading { root: ColourRef::parse("@hair_root").unwrap(), ..HairShading::default() };
        assert!(matches!(slotted.shading(&Palette::default()), Err(HairShadingError::Colour(_))));
        let mut palette = Palette::default();
        palette.0.insert(
            omuzaki_face_api::PaletteSlot::new("hair_root").unwrap(),
            omuzaki_core::colour::Colour::rgb(1, 2, 3),
        );
        let Shading::Program { params, .. } = slotted.shading(&palette).unwrap() else { panic!("a program") };
        assert_eq!(params[&Slug::new("root").unwrap()], ParamValue::Colour(omuzaki_core::colour::Colour::rgb(1, 2, 3)));
    }

    #[test]
    fn the_default_hairstyle_is_valid_and_round_trips() {
        let p = HairParams::default();
        p.validate().unwrap();
        assert_eq!(p.groups.len(), 3);
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<HairParams>(&json).unwrap(), p);
        assert!(json.contains(r#""type":"curtain""#), "generators are tagged by `type`: {json}");
    }

    #[test]
    fn generators_are_chosen_by_type_and_typos_fail() {
        let g: HairGroup = serde_json::from_str(r#"{"generator":{"type":"braid","strands":4}}"#).unwrap();
        let Generator::Braid(b) = &g.generator else { panic!("a braid") };
        assert_eq!((b.strands, b.lobes), (4, BraidGen::default().lobes));
        assert!(g.enabled && !g.dynamics.simulate);
        assert!(serde_json::from_str::<HairGroup>(r#"{"generator":{"type":"braid","strandz":4}}"#).is_err());
        assert!(serde_json::from_str::<HairGroup>(r#"{"generator":{"type":"ribbon"}}"#).is_err());
    }

    #[test]
    fn validation_reports_paths_inside_groups() {
        let mut p = HairParams::default();
        let g = p.groups.values_mut().find(|g| matches!(g.generator, Generator::Curtain(_))).unwrap();
        if let Generator::Curtain(c) = &mut g.generator {
            c.count = 0;
            c.envelope.truncate(1);
        }
        p.physics.iterations = 20;
        let e = p.validate().unwrap_err();
        let paths: Vec<_> = e.0.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(
            paths,
            ["physics/iterations", "groups/c-curtain/generator/count", "groups/c-curtain/generator/envelope"]
        );
    }

    #[test]
    fn guide_clumps_need_two_points_on_the_head() {
        let g = HairGroup {
            enabled: true,
            mirror: true,
            dynamics: Dynamics::default(),
            generator: Generator::Guides(GuidesGen {
                clumps: vec![GuideClump {
                    points: vec![GuidePoint::Surface([200.0, 0.0, 0.0])],
                    ..GuideClump::default()
                }],
            }),
        };
        let e = g.validate().unwrap_err();
        assert_eq!(e.0.len(), 2, "{e}");
    }

    #[test]
    fn a_clump_knows_its_length() {
        let c = HairClump {
            group: GroupId::new("g").unwrap(),
            rest: vec![Vec3::ZERO, Vec3::Y, Vec3::new(0.0, 1.0, 2.0)],
            width: Taper::default(),
            thickness: Bulge::default(),
            stiffness: Falloff::default(),
            shade: 1.0,
            simulate: true,
            follow: Follow::Body,
            tint: None,
        };
        assert!((c.length() - 3.0).abs() < 1e-6);
    }

    #[test]
    fn the_prototype_stream_is_park_miller_from_seed_seven() {
        // hair.js: seed = seed * 16807 % 2147483647 from 7, value = (seed - 1) / 2147483646.
        let mut rng = LegacyLcg::new(7).unwrap();
        assert_eq!(rng.next_unit(), 117_648.0 / 2_147_483_646.0);
        assert_eq!(rng.next_unit(), 1_977_326_742.0 / 2_147_483_646.0);
        assert!(LegacyLcg::new(0).is_none() && LegacyLcg::new(LegacyLcg::MAX_SEED + 1).is_none());
    }

    #[test]
    fn the_default_shading_keeps_the_program_and_adds_no_streaks() {
        let shading = HairShading::default();
        assert_eq!(shading.streaks, Vec::new());
        assert_eq!(shading.inner_darkening, 1.0);
        let Shading::Program { params, .. } = shading.shading(&Palette::default()).unwrap() else {
            panic!("a program")
        };
        assert_eq!(params[&Slug::new("keep-identity").unwrap()], ParamValue::Float(0.36));
        assert_eq!(params[&Slug::new("edge-darkening").unwrap()], ParamValue::Float(0.14));
    }

    #[test]
    fn the_rng_defaults_to_the_forked_stream_and_validates_its_seed() {
        assert_eq!(HairParams::default().rng, HairRng::Forked);
        let p = HairParams { rng: HairRng::LegacyLcg { seed: 0 }, ..HairParams::default() };
        let e = p.validate().unwrap_err();
        assert_eq!(e.0.iter().map(|x| x.path.as_str()).collect::<Vec<_>>(), ["rng/seed"], "{e}");
    }
}
