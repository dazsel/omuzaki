//! Wings (CB-43): `wings`, four kinds (feathered, bat, fairy, mechanical) on a three-bone frame per wing that folds and
//! spreads, with a spring-driven flutter at the tip.
//!
//! **Build.** The wings stand behind the upper back in one plane, parallel to it and just clear of it (the plane is
//! `thickness / 2 + 3 mm` behind the most backward point of the torso, so the wings stay at least 2 mm off the back in every
//! pose). The default frame is the chest bone's; an `attach` moves the whole piece. Lengths are for a standard-height body
//! and scale with the body and the attach scale.
//!
//! **Rig, per wing.** Three bones, `shoulder`, `mid` (the elbow) and `tip` (the wrist), each the child of the one before,
//! the first hanging from the attach joint, and two flutter bones `flap_0` and `flap_1` under the tip. Every vertex is
//! skinned by its place along the arm to the two bones either side of it, so the wing bends smoothly at the joints.
//!
//! **Fold.** `fold_channel` is one of the `customNN` channels of the animation bus. The three frame bones are driven by
//! it ([`BoneDriver`]): at `1` the wing is spread as built, at `0` the shoulder, elbow and wrist turn about the axis out of
//! the back (`Z`) into a Z-fold hanging down the back. Every bone turns about that one axis, so no vertex ever leaves its
//! plane: the folded and the spread wings are equally clear of the back, and the shells keep their topology. A channel
//! nobody drives is `0`, so a wing with nothing driving the channel is folded.
//!
//! **Flutter.** `flap_0` and `flap_1` form a spring chain ([`SpringChain`](omuzaki_anim_api::SpringChain)) in world space:
//! the outer part of the wing trails the arm as it moves and settles. Only the flutter bones are simulated; the frame
//! bones belong to the fold driver. The chain declares the colliders earlier parts published and exports as a VRM
//! spring chain.
//!
//! **Both sides.** Each wing is generated with its own sign `s` (`+1` the character's left, `-1` the right), so the pair
//! is the exact mirror image of one another.

use std::f64::consts::{PI, TAU};

use omuzaki_anim_api::{Channel, SpringParams, Transform};
use omuzaki_char_api::{
    BoneDriver, BoneName, BuildCtx, BuildError, ExtraBone, JointRef, PartCategory, PartKind, PartMeta, PartOutput,
    RecolourCtx, Recoloured, RigContribution,
};
use omuzaki_core::glam::{DQuat, DVec3, Vec3};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::rigid::{Detail, kit_error};
use super::shells::{
    Chain, Shells, Slot, blend_along, body_scale, closed_tube, ellipsoid, frames_on, recolour, spring_chain, torso_back,
};
use crate::kit::KitError;
use crate::kit::curve::{clamp, fraction, lerp};
use crate::kit::grid::{Grid, Skin};

/// The kind of wing.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WingKind {
    /// A fan of feathers along the arm.
    #[default]
    Feathered,
    /// A scalloped membrane on three fingers.
    Bat,
    /// Four petal-shaped lobes.
    Fairy,
    /// A fan of rigid blades.
    Mechanical,
}

/// What a kind is when the parameters leave it open (metres at a standard body).
struct Preset {
    span: f64,
    chord: f64,
    feathers: u8,
}

impl WingKind {
    fn preset(self) -> Preset {
        match self {
            Self::Feathered => Preset { span: 0.55, chord: 0.3, feathers: 9 },
            Self::Bat => Preset { span: 0.5, chord: 0.28, feathers: 0 },
            Self::Fairy => Preset { span: 0.34, chord: 0.2, feathers: 0 },
            Self::Mechanical => Preset { span: 0.5, chord: 0.3, feathers: 7 },
        }
    }
}

/// Wings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WingsParams {
    /// The kind.
    pub kind: WingKind,
    /// Length of one wing along the arm, metres at a standard body. Left out: the kind's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<f64>,
    /// How far the feathers, membrane or blades reach back from the arm, metres at a standard body. Left out: the kind's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chord: Option<f64>,
    /// Feathers or blades per wing, `3..=16`. Left out: the kind's own. Ignored by a bat's and a fairy's wings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feathers: Option<u8>,
    /// The feathers' or blades' size, `0.5..=1.5` times the kind's own.
    pub feather_size: f64,
    /// Thickness of the wing, metres at a standard body.
    pub thickness: f64,
    /// The `customNN` channel that folds the wings: `0` folded, `1` spread. `0..=15`.
    pub fold_channel: u8,
    /// How firmly the flutter bones are pulled back to their rest (VRM's `stiffness`).
    pub stiffness: f64,
    /// How much of its speed a flutter bone sheds each step, `0..=1` (VRM's `dragForce`).
    pub drag: f64,
    /// The pull of gravity on the flutter bones (VRM's `gravityPower`).
    pub gravity: f64,
    /// The feathers', membrane's, lobes' or blades' colour.
    pub wing: ColourRef,
    /// The arm's, fingers' and joints' colour.
    pub edge: ColourRef,
}

impl Default for WingsParams {
    fn default() -> Self {
        Self {
            kind: WingKind::Feathered,
            span: None,
            chord: None,
            feathers: None,
            feather_size: 1.0,
            thickness: 0.008,
            fold_channel: 0,
            stiffness: 0.9,
            drag: 0.4,
            gravity: 0.05,
            wing: ColourRef::rgb(0xf4, 0xf0, 0xf8),
            edge: ColourRef::rgb(0x9a, 0x8a, 0xb8),
        }
    }
}

impl Validate for WingsParams {
    fn check(&self, c: &mut Check) {
        if let Some(span) = self.span {
            c.range("span", span, 0.1, 1.5);
        }
        if let Some(chord) = self.chord {
            c.range("chord", chord, 0.05, 0.8);
        }
        if let Some(feathers) = self.feathers {
            c.range("feathers", f64::from(feathers), 3.0, 16.0);
        }
        c.range("feather_size", self.feather_size, 0.5, 1.5);
        c.range("thickness", self.thickness, 0.003, 0.03);
        if Channel::custom(usize::from(self.fold_channel)).is_none() {
            c.fail("fold_channel", format!("the fold channel is one of custom00..custom15, not {}", self.fold_channel));
        }
        c.range("stiffness", self.stiffness, 0.0, 4.0);
        c.range("drag", self.drag, 0.0, 1.0);
        c.range("gravity", self.gravity, 0.0, 2.0);
    }
}

/// Where the shoulder stands over the chest joint, metres at a standard body.
const SHOULDER_UP: f64 = 0.03;
/// How far each shoulder is from the middle of the back, metres at a standard body.
const SHOULDER_GAP: f64 = 0.03;
/// The clearance of the wing plane's front face behind the back, metres: the acceptance is 2 mm.
const BACK_CLEARANCE: f64 = 0.003;
/// The radius of the arm's bar at the shoulder, metres at a standard body.
const BAR_RADIUS: f64 = 0.011;
/// The radius of a mechanical wing's joint caps, metres at a standard body.
const JOINT_RADIUS: f64 = 0.018;
/// The directions of the three arm segments in the wing's plane (`x` outward, `y` up), at the spread pose, and each
/// segment's share of the span.
const ARM: [([f64; 2], f64); 3] = [([0.85, 0.52], 0.4), ([0.96, 0.28], 0.33), ([0.9, -0.1], 0.27)];
/// How far the three frame bones turn about the back axis to fold, degrees (shoulder, elbow, wrist), positive down: the
/// arm ends up hanging down the back, a little out, in a shallow V.
const FOLD_DEG: [f64; 3] = [110.0, -31.0, 2.0];

/// The `wings` kind.
pub struct Wings;

/// One wing: its arm and where the bones sit, in bind space (unplaced).
struct Wing {
    /// `+1` the character's left, `-1` the right.
    s: f64,
    /// The scale of the body.
    k: f64,
    /// The five bone points: shoulder, elbow, wrist, flutter 0 and flutter 1.
    bones: [DVec3; 5],
    /// The arc fraction of each bone along the arm.
    knots: [f64; 5],
}

impl Wing {
    #[allow(clippy::many_single_char_names)] // the arm's joints: root, elbow (m), wrist (w), tip (t)
    fn new(s: f64, root: DVec3, span: f64, k: f64) -> Self {
        let step = |d: [f64; 2], len: f64| DVec3::new(s * d[0], d[1], 0.0).normalize_or_zero() * len * span;
        let (m, w, t) = {
            let m = root + step(ARM[0].0, ARM[0].1);
            let w = m + step(ARM[1].0, ARM[1].1);
            (m, w, w + step(ARM[2].0, ARM[2].1))
        };
        let f0 = w.lerp(t, 0.5);
        let bones = [root, m, w, f0, t];
        let total: f64 = bones.windows(2).map(|p| p[0].distance(p[1])).sum();
        let mut knots = [0.0; 5];
        for i in 1..5 {
            knots[i] = knots[i - 1] + bones[i - 1].distance(bones[i]) / total;
        }
        Self { s, k, bones, knots }
    }

    /// The arm's polyline: shoulder, elbow, wrist, tip.
    fn arm(&self) -> [DVec3; 4] {
        [self.bones[0], self.bones[1], self.bones[2], self.bones[4]]
    }

    /// The point at arc fraction `u` of the arm and the unit tangent there.
    fn at(&self, u: f64) -> (DVec3, DVec3) {
        let arm = self.arm();
        let lengths: Vec<f64> = arm.windows(2).map(|p| p[0].distance(p[1])).collect();
        let total: f64 = lengths.iter().sum();
        let mut left = clamp(u, 0.0, 1.0) * total;
        for (i, len) in lengths.iter().enumerate() {
            if left <= *len || i == lengths.len() - 1 {
                let tangent = (arm[i + 1] - arm[i]).normalize_or_zero();
                return (arm[i] + tangent * left.min(*len), tangent);
            }
            left -= len;
        }
        (arm[3], DVec3::X)
    }

    /// The direction trailing back from the arm at tangent `t`: perpendicular to it in the wing's plane, pointing down.
    fn trailing(&self, tangent: DVec3) -> DVec3 {
        DVec3::new(tangent.y, -tangent.x, 0.0) * self.s
    }

    /// The arc fraction of the arm nearest `p`, from the placed bone points `bones` (so an offset attach is followed).
    fn arc_of(bones: &[DVec3; 5], p: DVec3) -> f64 {
        let arm = [bones[0], bones[1], bones[2], bones[4]];
        let total: f64 = arm.windows(2).map(|q| q[0].distance(q[1])).sum();
        let mut best = (f64::MAX, 0.0);
        let mut before = 0.0;
        for q in arm.windows(2) {
            let seg = q[1] - q[0];
            let len = seg.length();
            let along = if len > 0.0 { clamp((p - q[0]).dot(seg) / (len * len), 0.0, 1.0) } else { 0.0 };
            let dist = (q[0] + seg * along).distance(p);
            if dist < best.0 {
                best = (dist, (before + along * len) / total);
            }
            before += len;
        }
        best.1
    }

    /// The arm as a tube: the bar the wing hangs from.
    fn bar(&self, seg: usize) -> Result<crate::kit::sweep::Sweep, KitError> {
        let path: Vec<DVec3> = (0..=14).map(|i| self.at(fraction(i, 14)).0).collect();
        let r = BAR_RADIUS * self.k;
        closed_tube(&path, |t| (r * lerp(1.0, 0.55, t), r * lerp(1.0, 0.55, t)), seg, DVec3::Z)
    }
}

impl Wings {
    /// The feathers or blades: `count` leaf tubes fanned along the arm.
    fn feathers(
        wing: &Wing,
        p: &WingsParams,
        r: &Resolved,
        seg: usize,
    ) -> Result<Vec<crate::kit::sweep::Sweep>, KitError> {
        let rhombus = p.kind == WingKind::Mechanical;
        let spacing = 0.9 * r.span / usize_f(r.feathers);
        let half_width = 0.62 * spacing * p.feather_size;
        (0..r.feathers)
            .map(|k| {
                let u = lerp(0.1, 0.99, fraction(k, r.feathers - 1));
                let (root, tangent) = wing.at(u);
                let sweep = 0.5 * (1.0 - u);
                let dir = (wing.trailing(tangent) * sweep.cos() + tangent * sweep.sin()).normalize_or_zero();
                let length = r.chord * p.feather_size * (0.55 + 0.45 * (PI * (0.15 + 0.8 * u)).sin());
                let rows = 5;
                let path: Vec<DVec3> = (0..=rows)
                    .map(|i| {
                        let t = fraction(i, rows);
                        root - dir * 0.008 * wing.k + dir * (length * t) + tangent * (0.12 * length * t * t)
                    })
                    .collect();
                let thick = 0.5 * p.thickness * wing.k;
                let leaf = move |t: f64| {
                    let body = (PI * t.powf(0.7)).sin().max(0.0);
                    if rhombus {
                        (thick * 0.8, half_width * clamp(1.6 * (1.0 - t), 0.0, 1.0).sqrt() * 1.1)
                    } else {
                        (thick * body.sqrt(), half_width * body.powf(0.6))
                    }
                };
                closed_tube(&path, leaf, if rhombus { 4 } else { seg.min(6) }, DVec3::Z)
            })
            .collect()
    }

    /// The bat's membrane: a closed lens-thin solid between the arm and a scalloped trailing edge.
    fn membrane(wing: &Wing, p: &WingsParams, r: &Resolved, seg: usize) -> Result<(Grid, Vec<DVec3>), KitError> {
        let (cols, rows) = (seg * 3 / 2, 4);
        let chord = |u: f64| r.chord * (1.0 - 0.45 * u) * (0.8 + 0.2 * (TAU * 3.0 * u).cos());
        let half = 0.5 * p.thickness * wing.k;
        let point = |side: f64, v: usize, j: usize| {
            let u = lerp(0.04, 1.0, fraction(j, cols));
            let (lead, tangent) = wing.at(u);
            let t = fraction(v, rows);
            // Thin to nothing at every edge, so the front and back meet and the solid is closed.
            let thin = ((PI * t).sin() * (PI * fraction(j, cols)).sin()).max(0.0).sqrt();
            lead + wing.trailing(tangent) * (chord(u) * t) + DVec3::Z * (side * half * thin)
        };
        let source = |i: usize| if i <= rows { (1.0, i) } else { (-1.0, 2 * rows + 1 - i) };
        let grid = Grid::from_fn(2 * (rows + 1), cols + 1, |i, j| {
            let (side, v) = source(i);
            point(side, v, j)
        })?;
        let mids = (0..2 * (rows + 1))
            .flat_map(|i| (0..=cols).map(move |j| (i, j)))
            .map(|(i, j)| point(0.0, source(i).1, j))
            .collect();
        Ok((grid, mids))
    }

    /// A fairy's lobe: a flat ellipsoid from the shoulder at `degrees` above the arm's outward direction.
    fn lobe(
        wing: &Wing,
        degrees: f64,
        length: f64,
        width: f64,
        p: &WingsParams,
        rows: usize,
    ) -> Result<Grid, KitError> {
        let angle = wing.s * degrees.to_radians();
        let outward = DVec3::new(wing.s, 0.0, 0.0);
        let dir = DQuat::from_rotation_z(angle) * outward;
        let centre = wing.bones[0] + dir * (0.5 * length * 0.95);
        let radii = DVec3::new(0.5 * length, 0.5 * width, 0.5 * p.thickness * wing.k);
        let frame =
            DQuat::from_rotation_z(angle) * if wing.s > 0.0 { DQuat::IDENTITY } else { DQuat::from_rotation_y(PI) };
        let unit = ellipsoid(DVec3::ZERO, radii, rows, rows * 2)?;
        Grid::new(unit.rows(), unit.cols(), unit.points().iter().map(|q| centre + frame * *q).collect())
    }
}

/// The parameters with the kind's own filled in, at the body's scale.
struct Resolved {
    span: f64,
    chord: f64,
    feathers: usize,
}

fn usize_f(n: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)] // a count of a handful
    {
        n as f64
    }
}

#[allow(clippy::cast_possible_truncation)] // spring parameters are f32
fn narrow(v: f64) -> f32 {
    v as f32
}

impl WingsParams {
    fn resolved(&self, scale: f64) -> Resolved {
        let d = self.kind.preset();
        Resolved {
            span: self.span.unwrap_or(d.span) * scale,
            chord: self.chord.unwrap_or(d.chord) * scale,
            feathers: usize::from(self.feathers.unwrap_or(d.feathers)).max(1),
        }
    }

    fn slots(&self) -> [Slot; 2] {
        [Slot::opaque("wing", &self.wing), Slot::opaque("edge", &self.edge)]
    }
}

impl Wings {
    /// How far the wing's shells reach either side of their plane, metres at a standard body: the thickest of the wing, the
    /// arm and the mechanical joints.
    fn depth(p: &WingsParams) -> f64 {
        let joints = if p.kind == WingKind::Mechanical { JOINT_RADIUS } else { 0.0 };
        (0.5 * p.thickness).max(BAR_RADIUS).max(joints)
    }

    /// The rig of one wing: the three frame bones with the fold driver on each, and the flutter chain under the wrist.
    /// Returns the five joints, root first. `placed` are the bone points in the placed frame.
    fn rig(
        ctx: &BuildCtx<'_>,
        p: &WingsParams,
        attach_joint: &JointRef,
        rig: &mut RigContribution,
        name: &str,
        s: f64,
        placed: &[DVec3; 5],
    ) -> Result<Vec<JointRef>, BuildError> {
        let channel = Channel::custom(usize::from(p.fold_channel)).ok_or_else(|| {
            ctx.fail(format!("the fold channel is one of custom00..custom15, not {}", p.fold_channel))
        })?;
        let params = SpringParams {
            hit_radius: 0.0,
            stiffness: narrow(p.stiffness),
            gravity_power: narrow(p.gravity),
            gravity_dir: Vec3::NEG_Y,
            drag_force: narrow(p.drag),
        };
        let mut before = ctx
            .joint(attach_joint)
            .map(DVec3::from)
            .ok_or_else(|| ctx.fail("the wings' attach joint has no position"))?;
        let mut parent = attach_joint.clone();
        let mut frame = Vec::with_capacity(5);
        for (i, role) in ["shoulder", "elbow", "wrist"].into_iter().enumerate() {
            let bone = BoneName::new(format!("wings_{}_{name}_{role}", ctx.part))
                .map_err(|e| ctx.fail(format!("wing bone name: {e}")))?;
            let rest = Transform::from_translation(placed[i].as_vec3());
            let id = rig.add_bone(ExtraBone { name: bone.clone(), parent: parent.clone(), rest });
            // The channel runs from the folded pose (0) to the spread one (1): the same bone, turned about the back axis.
            let to = Transform::from_translation((placed[i] - before).as_vec3());
            let from = Transform { rotation: DQuat::from_rotation_z((-s * FOLD_DEG[i]).to_radians()).as_quat(), ..to };
            rig.drivers.push(BoneDriver::channel(id, channel, from, to));
            parent = JointRef::Named(bone);
            before = placed[i];
            frame.push(parent.clone());
        }
        let chain = Chain {
            tag: &format!("{name}_flap"),
            parent,
            points: &placed[3..],
            root: params,
            tip: params,
            centre: None,
        };
        frame.extend(spring_chain(ctx, "wings", rig, &chain)?);
        Ok(frame)
    }

    /// The shells of one wing: the arm and what the kind hangs from it.
    fn shells(
        ctx: &BuildCtx<'_>,
        p: &WingsParams,
        r: &Resolved,
        wing: &Wing,
        placed: &[DVec3; 5],
        joints: &[JointRef],
        shells: &mut Shells<'_>,
    ) -> Result<(), BuildError> {
        let err = kit_error(ctx);
        let seg = Detail::of(ctx.quality).lathe * 3 / 4;
        let k = wing.k;
        let knots = wing.knots;
        let weights = |q: DVec3, _: usize, _: usize| blend_along(joints, &knots, Wing::arc_of(placed, q));
        let mut add = |slot: usize, grid: &Grid, wraps: (bool, bool), inside: &dyn Fn(usize, usize) -> DVec3| {
            shells.bind_skinned(slot, grid, wraps, inside, Skin::Per(&weights)).map_err(&err)
        };
        let bar = wing.bar(seg.min(8)).map_err(&err)?;
        add(1, &bar.grid, (false, true), &|i, _| bar.centres[i])?;
        match p.kind {
            WingKind::Feathered | WingKind::Mechanical => {
                for feather in Self::feathers(wing, p, r, seg).map_err(&err)? {
                    add(0, &feather.grid, (false, true), &|i, _| feather.centres[i])?;
                }
            }
            WingKind::Bat => {
                let (grid, mids) = Self::membrane(wing, p, r, seg).map_err(&err)?;
                add(0, &grid, (false, true), &|i, j| mids[i * grid.cols() + j])?;
                // Three fingers from the wrist to the trailing edge, in the arm's colour.
                for u in [0.72, 0.86, 1.0] {
                    let (edge, tangent) = wing.at(u);
                    let end = edge + wing.trailing(tangent) * (0.92 * r.chord * (1.0 - 0.45 * u));
                    let path: Vec<DVec3> = (0..=8).map(|i| wing.bones[2].lerp(end, fraction(i, 8))).collect();
                    let radius = 0.005 * k;
                    let taper = |t: f64| (radius * lerp(1.0, 0.4, t), radius * lerp(1.0, 0.4, t));
                    let finger = closed_tube(&path, taper, 4, DVec3::Z).map_err(&err)?;
                    add(1, &finger.grid, (false, true), &|i, _| finger.centres[i])?;
                }
            }
            WingKind::Fairy => {
                for (degrees, length, width) in
                    [(58.0, 0.8, 0.34), (18.0, 0.7, 0.26), (-22.0, 0.5, 0.22), (-50.0, 0.38, 0.16)]
                {
                    let lobe = Self::lobe(wing, degrees, length * r.span, width * r.span, p, 6).map_err(&err)?;
                    let centre = lobe.points().iter().sum::<DVec3>() / usize_f(lobe.points().len());
                    add(0, &lobe, (false, true), &|_, _| centre)?;
                }
            }
        }
        if p.kind == WingKind::Mechanical {
            // Joint caps on the elbow and the wrist.
            for at in [wing.bones[1], wing.bones[2]] {
                let cap = ellipsoid(at, DVec3::splat(JOINT_RADIUS * k), 8, 12).map_err(&err)?;
                add(1, &cap, (false, true), &|_, _| at)?;
            }
        }
        Ok(())
    }
}

impl PartKind for Wings {
    type Params = WingsParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "wings",
            summary: "Build a pair of wings (feathered, bat, fairy or mechanical) on a foldable frame with spring flutter.",
            category: PartCategory::Accessory,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &WingsParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let (attach, default) = frames_on(ctx, &JointRef::CHEST)?;
        let k = body_scale(ctx)? * attach.scale();
        let r = p.resolved(k);
        let chest = ctx
            .joint(&JointRef::CHEST)
            .ok_or_else(|| ctx.fail("the body has no chest joint to hang the wings from"))?;
        let chest = DVec3::from(chest);
        // The wings' plane: behind the most backward point of the torso, so no pose of the wing touches the back.
        let back = torso_back(ctx)?.into_iter().map(|(_, z)| z).fold(f64::MAX, f64::min);
        let z = back - BACK_CLEARANCE - Self::depth(p) * k;
        let place = attach.to_bind * default.to_bind.inverse();
        let mut rig = RigContribution::default();
        let mut shells = Shells::new(&attach, &default, 2);
        for (name, s) in [("l", 1.0), ("r", -1.0)] {
            let root = DVec3::new(chest.x + s * SHOULDER_GAP * k, chest.y + SHOULDER_UP * k, z);
            let wing = Wing::new(s, root, r.span, k);
            let placed: [DVec3; 5] = wing.bones.map(|b| place.transform_point3(b));
            let joints = Self::rig(ctx, p, &attach.joint, &mut rig, name, s, &placed)?;
            Self::shells(ctx, p, &r, &wing, &placed, &joints, &mut shells)?;
        }
        shells.output(ctx, &p.slots(), rig)
    }

    fn recolour(p: &WingsParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour(out, ctx, &p.slots())
    }
}

omuzaki_char_api::register_part_kind!(Wings);
