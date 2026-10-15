//! Rigid jewellery (CB-43): `choker`, `bell`, `hairpin`, `halo` and the generic `charm`.
//!
//! Each kind is a small generator: it builds closed shells in the accessory's local frame (origin at the attach point,
//! `+Y` up, `+Z` front; see [`super::attach`]), places them with [`ResolvedAttach`] and skins them to the attach joint.
//! The shells are lathes, rings and ellipsoids from the geometry kit, so every one is closed and smooth.
//!
//! **Where each sits when `attach` is `None`.** The choker wraps the body's neck section; the hairpin sits on the head
//! surface in `direction`; the halo floats over the first published `top` anchor (the head's); the bell and the charm
//! hang from the chest joint. Give a part an `attach` to put any of them elsewhere.
//!
//! **Dangling.** A bell or charm with `dangle` hangs on a two-bone spring chain: bone 0 sits at the attach point, bone 1
//! at the far end of the hanging part, and everything below the attach point follows bone 0, fading in down the cord. The
//! reach (attach point to the farthest vertex) is capped at [`MAX_REACH`], so even a full 25 degree swing keeps every
//! vertex within 3 cm of where it rests ([`MAX_SWING_DEG`]). The halo's `bob` is the same chain standing upright: the
//! ring tilts a little as the head moves and settles back.

use std::f64::consts::{PI, TAU};

use omuzaki_anim_api::{SpringChain, SpringParams, Transform};
use omuzaki_char_api::{
    AnchorsFact, Attach, AttachOffset, BodyRegion, BoneName, BuildCtx, BuildError, ExtraBone, FitFact, JointFit,
    JointRef, MaterialName, MaterialSpec, PartCategory, PartKind, PartMesh, PartMeta, PartOutput, Quality, RecolourCtx,
    Recoloured, RigContribution, humanoid_id,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DMat3, DMat4, DQuat, DVec3, Vec3};
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::{Shading, Toon};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::attach::{self, ResolvedAttach, attachment, on_bone};
use crate::kit::KitError;
use crate::kit::curve::fraction;
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::loft::lathe_axis;

/// The largest swing the dangling parts are built for, degrees: a head yaw of this much must not throw them far.
pub const MAX_SWING_DEG: f64 = 25.0;
/// The farthest a dangling part may reach from its attach point, metres. A bone swung by [`MAX_SWING_DEG`] moves a
/// point at distance `d` by `2 d sin(12.5 degrees)`, which is 3 cm at this reach.
pub const MAX_REACH: f64 = 0.069;

// ------------------------------------------------------------------------------------------------ shared parameters

/// The two colours of a piece of jewellery, by palette slot.
fn primary_colour() -> ColourRef {
    ColourRef::rgb(0xe0, 0xb8, 0x58)
}

fn accent_colour() -> ColourRef {
    ColourRef::rgb(0xc8, 0x3c, 0x5a)
}

/// The shape of a pendant (a choker's, a charm's).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PendantShape {
    /// A ball.
    #[default]
    Orb,
    /// A teardrop, point up.
    Drop,
    /// A six-sided faceted stone.
    Gem,
}

impl PendantShape {
    /// The lathe profile `(y, radius)` of the shape at `size`, centred on the origin, from the top pole to the bottom.
    fn profile(self, size: f64, steps: usize) -> Vec<(f64, f64)> {
        match self {
            Self::Orb => (0..=steps)
                .map(|k| {
                    let t = PI * fraction(k, steps);
                    (size * t.cos(), size * t.sin())
                })
                .collect(),
            Self::Drop => (0..=steps)
                .map(|k| {
                    let s = fraction(k, steps);
                    (size * (1.5 - 2.5 * s), size * (PI * s).sin() * (0.3 + 0.8 * s).min(1.0))
                })
                .collect(),
            Self::Gem => {
                [(1.0, 0.0), (0.55, 0.62), (0.1, 1.0), (-1.0, 0.0)].map(|(y, r)| (size * y, size * r)).to_vec()
            }
        }
    }

    /// Segments round the shape: a gem is hexagonal.
    fn segments(self, smooth: usize) -> usize {
        if self == Self::Gem { 6 } else { smooth }
    }
}

/// A pendant on a choker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Pendant {
    /// Its shape.
    pub shape: PendantShape,
    /// Radius in metres.
    pub size: f64,
}

impl Default for Pendant {
    fn default() -> Self {
        Self { shape: PendantShape::Orb, size: 0.008 }
    }
}

impl Validate for Pendant {
    fn check(&self, c: &mut Check) {
        c.range("size", self.size, 0.003, 0.03);
    }
}

// ------------------------------------------------------------------------------------------------ geometry helpers

/// How finely the shells are cut.
#[derive(Copy, Clone)]
struct Detail {
    /// Segments round a ring's major circle.
    ring: usize,
    /// Segments round a ring's wire.
    cross: usize,
    /// Segments round a lathe.
    lathe: usize,
    /// Steps along a lathe's profile.
    rows: usize,
}

impl Detail {
    fn of(quality: Quality) -> Self {
        match quality {
            Quality::Live => Self { ring: 48, cross: 8, lathe: 16, rows: 20 },
            Quality::Offline => Self { ring: 96, cross: 12, lathe: 32, rows: 40 },
        }
    }
}

/// Which material a shell takes.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Slot {
    Primary,
    Accent,
}

impl Slot {
    const ALL: [Self; 2] = [Self::Primary, Self::Accent];

    fn suffix(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Accent => "accent",
        }
    }
}

/// What a vertex follows. Dangling parts blend from the attach joint to the swinging bone down the cord.
enum Follow {
    /// Entirely the attach joint.
    Fixed(JointRef),
    /// The attach joint at the attach point, fading into `swing` over `ramp` metres below it. A `ramp` of 0 follows
    /// `swing` everywhere.
    Swing { fixed: JointRef, swing: JointRef, ramp: f64, to_local: DMat4 },
}

impl Follow {
    fn weights(&self, bind: DVec3) -> Vec<(JointRef, f32)> {
        match self {
            Self::Fixed(joint) => vec![(joint.clone(), 1.0)],
            Self::Swing { fixed, swing, ramp, to_local } => {
                let depth = -to_local.transform_point3(bind).y;
                let s = if *ramp <= 0.0 { 1.0 } else { (depth / ramp).clamp(0.0, 1.0) };
                #[allow(clippy::cast_possible_truncation)] // weights are in 0..=1
                let (to_fixed, to_swing) = ((1.0 - s) as f32, s as f32);
                vec![(fixed.clone(), to_fixed), (swing.clone(), to_swing)]
            }
        }
    }
}

/// A ring in local space: the centre line is an ellipse of semi-axes `rx` along `ex` and `rz` along `ez`, and the wire
/// has semi-axes `half_t` outward and `half_w` along `up`.
struct Ring {
    centre: DVec3,
    ex: DVec3,
    ez: DVec3,
    up: DVec3,
    rx: f64,
    rz: f64,
    half_w: f64,
    half_t: f64,
}

/// The shells of one piece, one builder per material, and what places and skins them.
struct Shells<'a> {
    attach: &'a ResolvedAttach,
    follow: &'a Follow,
    detail: Detail,
    meshes: [MeshBuilder; 2],
}

impl<'a> Shells<'a> {
    fn new(attach: &'a ResolvedAttach, follow: &'a Follow, detail: Detail) -> Self {
        Self { attach, follow, detail, meshes: [MeshBuilder::new(), MeshBuilder::new()] }
    }

    fn builder(&mut self, slot: Slot) -> &mut MeshBuilder {
        &mut self.meshes[usize::from(slot == Slot::Accent)]
    }

    /// Adds `local` (points in the accessory frame) as a closed-in-`j` grid, `inside` giving the interior reference of
    /// row `i` in local space.
    fn add(&mut self, slot: Slot, local: &Grid, wrap_i: bool, inside: &dyn Fn(usize) -> DVec3) -> Result<(), KitError> {
        let attach = self.attach;
        let placed = Grid::new(local.rows(), local.cols(), local.points().iter().map(|&p| attach.point(p)).collect())?;
        let inside = |i: usize, _: usize| attach.point(inside(i));
        let follow = self.follow;
        let skin = |p: DVec3, _: usize, _: usize| follow.weights(p);
        let options = GridOptions {
            wrap_i,
            wrap_j: true,
            inside: Some(&inside),
            skin: Skin::Per(&skin),
            ..GridOptions::default()
        };
        self.builder(slot).add_grid(&placed, &options)
    }

    /// A body of revolution about local `+Y` through `origin`; `profile` is `(distance along Y, radius)` and should
    /// start and end on the axis so the shell is closed.
    fn lathe(&mut self, slot: Slot, origin: DVec3, profile: &[(f64, f64)], seg: usize) -> Result<(), KitError> {
        let grid = lathe_axis(origin, DVec3::Y, profile, seg)?;
        self.add(slot, &grid, false, &|i| origin + DVec3::Y * profile[i].0)
    }

    /// An ellipsoid of semi-axes `radii` turned by `rotation`, centred on `centre`.
    fn ellipsoid(&mut self, slot: Slot, centre: DVec3, radii: DVec3, rotation: DQuat) -> Result<(), KitError> {
        let (rows, cols) = (self.detail.rows, self.detail.lathe);
        let grid = Grid::from_fn(rows + 1, cols + 1, |i, j| {
            let (theta, phi) = (PI * fraction(i, rows), TAU * fraction(j, cols));
            let unit = DVec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin());
            centre + rotation * (unit * radii)
        })?;
        self.add(slot, &grid, false, &|_| centre)
    }

    /// A torus-like band (see [`Ring`]).
    fn ring(&mut self, slot: Slot, r: &Ring) -> Result<(), KitError> {
        let (rows, cols) = (self.detail.ring, self.detail.cross);
        let line = |theta: f64| r.centre + r.ex * (r.rx * theta.cos()) + r.ez * (r.rz * theta.sin());
        let grid = Grid::from_fn(rows + 1, cols + 1, |i, j| {
            let (theta, phi) = (TAU * fraction(i, rows), TAU * fraction(j, cols));
            // The outward normal of the ellipse, so the wire stands off the centre line the same way all round.
            let out = (r.ex * (theta.cos() / r.rx) + r.ez * (theta.sin() / r.rz)).normalize_or_zero();
            line(theta) + out * (r.half_t * phi.cos()) + r.up * (r.half_w * phi.sin())
        })?;
        self.add(slot, &grid, true, &|i| line(TAU * fraction(i, rows)))
    }

    /// A bell's or charm's body, hanging from the attach point: `cord` metres of cord, a ring, then the body.
    fn pendant(&mut self, spec: &PendantSpec) -> Result<(), KitError> {
        let (cord, ring_r) = (spec.cord, spec.ring_radius());
        let wire = spec.wire();
        if cord > 1e-3 {
            let profile = [(0.0, 0.0), (-5e-4, wire), (-cord + 5e-4, wire), (-cord, 0.0)];
            self.lathe(Slot::Accent, DVec3::ZERO, &profile, 8)?;
        }
        // The ring faces forward, its top where the cord ends.
        let ring = Ring {
            centre: DVec3::new(0.0, -cord - ring_r + wire, 0.0),
            ex: DVec3::X,
            ez: DVec3::Y,
            up: DVec3::Z,
            rx: ring_r,
            rz: ring_r,
            half_w: wire,
            half_t: wire,
        };
        self.ring(Slot::Primary, &ring)?;
        let top = -cord - 2.0 * ring_r + 2.0 * wire;
        let (profile, seg, body_slot) = match spec.body {
            PendantBody::Bell { radius } => {
                (bell_profile(radius, self.detail.rows * 2), self.detail.lathe, Slot::Primary)
            }
            PendantBody::Shape { shape, size } => {
                (shape.profile(size, self.detail.rows), shape.segments(self.detail.lathe), Slot::Accent)
            }
        };
        let centre = DVec3::new(0.0, top - spec.body.half_height(), 0.0);
        self.lathe(body_slot, centre, &profile, seg)
    }
}

/// A sphere of `radius` with a slit: a groove round it a little below the equator, the line a bell is cut along.
fn bell_profile(radius: f64, steps: usize) -> Vec<(f64, f64)> {
    let slit_at = (-0.3_f64).acos();
    (0..=steps)
        .map(|k| {
            let t = PI * fraction(k, steps);
            let groove = 1.0 - 0.45 * (-((t - slit_at) / 0.09).powi(2)).exp();
            (radius * t.cos(), radius * t.sin() * groove)
        })
        .collect()
}

/// What hangs from the cord.
#[derive(Copy, Clone)]
enum PendantBody {
    Bell { radius: f64 },
    Shape { shape: PendantShape, size: f64 },
}

impl PendantBody {
    /// Half the body's height: its centre sits this far below its top.
    fn half_height(self) -> f64 {
        match self {
            Self::Bell { radius } => radius,
            Self::Shape { shape: PendantShape::Drop, size } => size * 1.5,
            Self::Shape { size, .. } => size,
        }
    }
}

/// Everything that sizes a hanging piece.
#[derive(Copy, Clone)]
struct PendantSpec {
    body: PendantBody,
    cord: f64,
}

impl PendantSpec {
    fn size(&self) -> f64 {
        match self.body {
            PendantBody::Bell { radius } => radius,
            PendantBody::Shape { size, .. } => size,
        }
    }

    fn ring_radius(&self) -> f64 {
        0.4 * self.size()
    }

    fn wire(&self) -> f64 {
        0.12 * self.size()
    }

    /// Distance from the attach point to the farthest vertex (from the bottom pole of the body).
    fn reach(&self) -> f64 {
        self.cord + 2.0 * self.ring_radius() - 2.0 * self.wire() + 2.0 * self.body.half_height()
    }

    /// The point the second spring bone sits at: the middle of the body, in local space.
    fn tail(&self) -> DVec3 {
        let top = -self.cord - 2.0 * self.ring_radius() + 2.0 * self.wire();
        DVec3::new(0.0, top - self.body.half_height(), 0.0)
    }
}

// ------------------------------------------------------------------------------------------------ building blocks

fn kit_error<'a>(ctx: &'a BuildCtx<'_>) -> impl Fn(KitError) -> BuildError + 'a {
    move |e| ctx.fail(format!("geometry: {e}"))
}

fn material_name(part: &omuzaki_char_api::PartId, slot: Slot) -> Result<MaterialName, BuildError> {
    MaterialName::new(format!("{part}-{}", slot.suffix()))
        .map_err(|e| BuildError::Part { part: part.clone(), message: format!("material name: {e}") })
}

/// A toon material of `base`, its shade a darker `base`, giving off `glow` of its colour.
fn material(base: Colour, glow: f32) -> MaterialSpec {
    let scale = |v: u8, k: f32| -> u8 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to a byte
        {
            (f32::from(v) * k).round().clamp(0.0, 255.0) as u8
        }
    };
    let tint = |k: f32| Colour::rgb(scale(base.r, k), scale(base.g, k), scale(base.b, k));
    let mut spec = MaterialSpec::toon(base);
    spec.shading = Shading::Toon(Toon { shade: tint(0.68), emissive: tint(glow), ..Toon::default() });
    spec
}

/// Finishes the shells into meshes and materials. `colour` resolves a slot's colour; `glow` is how much of its colour
/// the primary material gives off.
fn output(
    ctx: &BuildCtx<'_>,
    shells: Shells<'_>,
    colours: [&ColourRef; 2],
    glow: f32,
    rig: RigContribution,
) -> Result<PartOutput, BuildError> {
    let mut out = PartOutput { rig, ..PartOutput::default() };
    // Publish the bones this part contributes, so another part can attach to one (`halo_acc_0`): a joint-only fit never
    // hides the body from garments, and the rig still gets the bones from the contribution.
    let bones: Vec<_> = out.rig.bones.iter().map(|b| (JointRef::Named(b.name.clone()), b.rest.translation)).collect();
    if !bones.is_empty() {
        out.publishes.put::<FitFact>(std::sync::Arc::new(JointFit::new(bones)));
    }
    for (slot, builder) in Slot::ALL.into_iter().zip(shells.meshes) {
        if builder.vertex_count() == 0 {
            continue;
        }
        let name = material_name(ctx.part, slot)?;
        let mut mesh: PartMesh =
            builder.finish(format!("{}-{}", ctx.part, slot.suffix()), name.clone()).map_err(kit_error(ctx))?;
        mesh.casts_shadow = true;
        out.meshes.push(mesh);
        let colour = ctx.colour(colours[usize::from(slot == Slot::Accent)])?;
        out.materials.push((name, material(colour, if slot == Slot::Primary { glow } else { 0.0 })));
    }
    Ok(out)
}

/// Recolours `out` for the new palette: every material is rebuilt from its slot's colour, nothing else changes.
fn recolour_output(
    out: &mut PartOutput,
    ctx: &RecolourCtx<'_>,
    colours: [&ColourRef; 2],
    glow: f32,
) -> Result<Recoloured, BuildError> {
    for (name, spec) in &mut out.materials {
        let Some(slot) = Slot::ALL.into_iter().find(|s| name.as_str().ends_with(&format!("-{}", s.suffix()))) else {
            return Ok(Recoloured::NeedsRebuild);
        };
        let colour = ctx.colour(colours[usize::from(slot == Slot::Accent)])?;
        *spec = material(colour, if slot == Slot::Primary { glow } else { 0.0 });
    }
    Ok(Recoloured::Done)
}

/// The two spring bones and the chain of a dangling or bobbing piece, which opts into every published collider group. Bone 0 sits at the attach point and bone 1 at
/// `tail` (local space), both resting in bind space; the chain swings bone 0 towards bone 1.
fn spring_pair(
    ctx: &BuildCtx<'_>,
    kind: &str,
    attach: &ResolvedAttach,
    tail: DVec3,
    params: SpringParams,
) -> Result<(JointRef, RigContribution), BuildError> {
    let name = |i: usize| {
        BoneName::new(format!("{kind}_{}_{i}", ctx.part)).map_err(|e| ctx.fail(format!("spring bone name: {e}")))
    };
    let (root, tip) = (name(0)?, name(1)?);
    let at = |p: DVec3| Transform::from_translation(p.as_vec3());
    let mut rig = RigContribution::default();
    let b0 = rig.add_bone(ExtraBone { name: root.clone(), parent: attach.joint.clone(), rest: at(attach.origin()) });
    let b1 = rig.add_bone(ExtraBone { name: tip, parent: JointRef::Named(root.clone()), rest: at(attach.point(tail)) });
    let mut chain = SpringChain::new(format!("{kind}_{}", ctx.part), &[b0, b1], params);
    // The chain declares the colliders earlier parts published (head, shoulders) by name; the rig's simulation, not
    // this crate, keeps the swing out of them.
    chain.collider_groups = ctx.colliders().iter().map(|group| group.name.clone()).collect();
    if let JointRef::Humanoid(bone) = attach.joint {
        chain.center = Some(humanoid_id(bone));
    }
    rig.chains.push(chain);
    Ok((JointRef::Named(root), rig))
}

/// How a hanging piece swings: loose, with a little gravity.
const DANGLE: SpringParams =
    SpringParams { hit_radius: 0.0, stiffness: 0.9, gravity_power: 0.4, gravity_dir: Vec3::NEG_Y, drag_force: 0.35 };
/// How a halo bobs: stiff, no gravity, so it tilts with the head and settles back.
const BOB: SpringParams =
    SpringParams { hit_radius: 0.0, stiffness: 1.0, gravity_power: 0.0, gravity_dir: Vec3::NEG_Y, drag_force: 0.25 };

/// The frame whose origin is `origin` and whose `+Y` is `up`.
fn frame_up(origin: DVec3, up: DVec3) -> DMat4 {
    DMat4::from_rotation_translation(DQuat::from_rotation_arc(DVec3::Y, up.normalize_or_zero()), origin)
}

// ------------------------------------------------------------------------------------------------ choker

/// A choker: a band round the neck, with an optional pendant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ChokerParams {
    /// Where along the neck, `0` at its base to `1` under the head.
    pub along: f64,
    /// Band height in metres.
    pub width: f64,
    /// Band thickness in metres.
    pub thickness: f64,
    /// Gap between the band and the neck, metres.
    pub ease: f64,
    /// A pendant hanging at the front, or none. Left out when none: stored parameters never hold a `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pendant: Option<Pendant>,
    /// The band colour.
    pub primary: ColourRef,
    /// The pendant colour.
    pub accent: ColourRef,
}

impl Default for ChokerParams {
    fn default() -> Self {
        Self {
            along: 0.5,
            width: 0.012,
            thickness: 0.0035,
            ease: 0.0015,
            pendant: None,
            primary: primary_colour(),
            accent: accent_colour(),
        }
    }
}

impl Validate for ChokerParams {
    fn check(&self, c: &mut Check) {
        c.range("along", self.along, 0.0, 1.0);
        c.range("width", self.width, 0.004, 0.04);
        c.range("thickness", self.thickness, 0.001, 0.01);
        c.range("ease", self.ease, 0.0, 0.02);
        if let Some(pendant) = &self.pendant {
            c.nested("pendant", pendant);
        }
    }
}

/// The `choker` kind.
pub struct Choker;

impl Choker {
    /// The frame of the neck's cross-section at `along`: origin at its centre, `+Y` up the neck, `+X` across, `+Z` front.
    fn neck_frame(ctx: &BuildCtx<'_>, along: f64) -> Result<(ResolvedAttach, [f64; 2]), BuildError> {
        #[allow(clippy::cast_possible_truncation)] // a fraction of a neck
        let section = ctx
            .fit()?
            .section(BodyRegion::Neck, along as f32)
            .ok_or_else(|| ctx.fail("the body has no neck section to wrap"))?;
        let up = DVec3::from(section.axis).normalize_or_zero();
        let across = (DVec3::X - up * DVec3::X.dot(up)).normalize_or_zero();
        let front = across.cross(up);
        let frame = DMat4::from_mat3_translation(DMat3::from_cols(across, up, front), DVec3::from(section.centre));
        let radii = [f64::from(section.radii.x), f64::from(section.radii.y)];
        Ok((ResolvedAttach::new(JointRef::NECK, frame), radii))
    }
}

impl PartKind for Choker {
    type Params = ChokerParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "choker",
            summary: "Build a band round the neck, with an optional pendant.",
            category: PartCategory::Accessory,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &ChokerParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let (section_frame, [across, front]) = Self::neck_frame(ctx, p.along)?;
        let attach = attachment(ctx, |_| Ok(section_frame))?;
        let follow = Follow::Fixed(attach.joint.clone());
        let mut shells = Shells::new(&attach, &follow, Detail::of(ctx.quality));
        let half_t = p.thickness;
        let band = Ring {
            centre: DVec3::ZERO,
            ex: DVec3::X,
            ez: DVec3::Z,
            up: DVec3::Y,
            rx: across + p.ease + half_t,
            rz: front + p.ease + half_t,
            half_w: 0.5 * p.width,
            half_t,
        };
        let err = kit_error(ctx);
        shells.ring(Slot::Primary, &band).map_err(&err)?;
        if let Some(pendant) = &p.pendant {
            let (shape, size) = (pendant.shape, pendant.size);
            let centre = DVec3::new(0.0, -0.5 * p.width - 0.7 * size, band.rz);
            let seg = shape.segments(shells.detail.lathe);
            let profile = shape.profile(size, shells.detail.rows);
            shells.lathe(Slot::Accent, centre, &profile, seg).map_err(&err)?;
        }
        output(ctx, shells, [&p.primary, &p.accent], 0.0, RigContribution::default())
    }

    fn recolour(p: &ChokerParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour_output(out, ctx, [&p.primary, &p.accent], 0.0)
    }
}

omuzaki_char_api::register_part_kind!(Choker);

// ------------------------------------------------------------------------------------------------ bell and charm

/// Checks the length of a hanging piece against [`MAX_REACH`].
fn check_reach(c: &mut Check, spec: &PendantSpec, dangle: bool) {
    if dangle && spec.reach() > MAX_REACH {
        c.fail(
            "cord",
            format!(
                "the piece reaches {:.1} cm from where it is fixed, more than {:.1} cm: shorten the cord or the \
                 size, or turn dangle off (a longer piece would swing more than 3 cm at a {MAX_SWING_DEG} degree turn)",
                spec.reach() * 100.0,
                MAX_REACH * 100.0
            ),
        );
    }
}

/// Builds a hanging piece: skins it to the attach joint, or to a spring pair when it dangles.
fn build_pendant(
    ctx: &mut BuildCtx<'_>,
    kind: &str,
    spec: &PendantSpec,
    dangle: bool,
    colours: [&ColourRef; 2],
) -> Result<PartOutput, BuildError> {
    let attach = attachment(ctx, |c| on_bone(c, &JointRef::CHEST))?;
    let scale = attach.scale();
    let (follow, rig) = if dangle {
        // The offset's scale scales the piece, and with it the swing: the cap on the reach is for the scaled piece.
        if spec.reach() * scale > MAX_REACH {
            return Err(ctx.fail(format!(
                "with the attach scale {scale} the piece reaches {:.1} cm from where it is fixed, more than {:.1} cm: \
                 lower the scale, shorten the cord or the size, or turn dangle off (a longer piece would swing more \
                 than 3 cm at a {MAX_SWING_DEG} degree turn)",
                spec.reach() * scale * 100.0,
                MAX_REACH * 100.0
            )));
        }
        // The joint collides as a ball at the middle of the body, big enough to hold the whole scaled body.
        #[allow(clippy::cast_possible_truncation)] // metres
        let hit_radius = (spec.body.half_height() * scale) as f32;
        let (swing, rig) = spring_pair(ctx, kind, &attach, spec.tail(), SpringParams { hit_radius, ..DANGLE })?;
        let to_local = attach.to_bind.inverse();
        (Follow::Swing { fixed: attach.joint.clone(), swing, ramp: spec.cord, to_local }, rig)
    } else {
        (Follow::Fixed(attach.joint.clone()), RigContribution::default())
    };
    let mut shells = Shells::new(&attach, &follow, Detail::of(ctx.quality));
    shells.pendant(spec).map_err(kit_error(ctx))?;
    output(ctx, shells, colours, 0.0, rig)
}

/// A bell: a sphere with a slit and a ring, on a cord, optionally dangling on a two-bone spring chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BellParams {
    /// Radius of the bell, metres.
    pub radius: f64,
    /// Length of the cord, metres.
    pub cord: f64,
    /// Hang on a spring chain so the bell swings as the body moves.
    pub dangle: bool,
    /// The bell and ring colour.
    pub primary: ColourRef,
    /// The cord colour.
    pub accent: ColourRef,
}

impl Default for BellParams {
    fn default() -> Self {
        Self { radius: 0.008, cord: 0.02, dangle: true, primary: primary_colour(), accent: accent_colour() }
    }
}

impl BellParams {
    fn spec(&self) -> PendantSpec {
        PendantSpec { body: PendantBody::Bell { radius: self.radius }, cord: self.cord }
    }
}

impl Validate for BellParams {
    fn check(&self, c: &mut Check) {
        c.range("radius", self.radius, 0.003, 0.03);
        c.range("cord", self.cord, 0.0, 0.06);
        check_reach(c, &self.spec(), self.dangle);
    }
}

/// The `bell` kind.
pub struct Bell;

impl PartKind for Bell {
    type Params = BellParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "bell",
            summary: "Build a bell with a slit and a ring on a cord, optionally dangling on a spring chain.",
            category: PartCategory::Accessory,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &BellParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        build_pendant(ctx, "bell", &p.spec(), p.dangle, [&p.primary, &p.accent])
    }

    fn recolour(p: &BellParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour_output(out, ctx, [&p.primary, &p.accent], 0.0)
    }
}

omuzaki_char_api::register_part_kind!(Bell);

/// A generic charm: a small orb, drop or gem on a ring and cord, optionally dangling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CharmParams {
    /// The shape.
    pub shape: PendantShape,
    /// Radius in metres.
    pub size: f64,
    /// Length of the cord, metres.
    pub cord: f64,
    /// Hang on a spring chain so the charm swings as the body moves.
    pub dangle: bool,
    /// The ring and cord colour.
    pub primary: ColourRef,
    /// The charm colour.
    pub accent: ColourRef,
}

impl Default for CharmParams {
    fn default() -> Self {
        Self {
            shape: PendantShape::Orb,
            size: 0.008,
            cord: 0.015,
            dangle: true,
            primary: primary_colour(),
            accent: accent_colour(),
        }
    }
}

impl CharmParams {
    fn spec(&self) -> PendantSpec {
        PendantSpec { body: PendantBody::Shape { shape: self.shape, size: self.size }, cord: self.cord }
    }
}

impl Validate for CharmParams {
    fn check(&self, c: &mut Check) {
        c.range("size", self.size, 0.003, 0.03);
        c.range("cord", self.cord, 0.0, 0.06);
        check_reach(c, &self.spec(), self.dangle);
    }
}

/// The `charm` kind.
pub struct Charm;

impl PartKind for Charm {
    type Params = CharmParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "charm",
            summary: "Build a small orb, drop or gem on a ring and cord, optionally dangling on a spring chain.",
            category: PartCategory::Accessory,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &CharmParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        build_pendant(ctx, "charm", &p.spec(), p.dangle, [&p.primary, &p.accent])
    }

    fn recolour(p: &CharmParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour_output(out, ctx, [&p.primary, &p.accent], 0.0)
    }
}

omuzaki_char_api::register_part_kind!(Charm);

// ------------------------------------------------------------------------------------------------ hairpin

/// The kind of hairpin.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HairpinStyle {
    /// A flat bar with three studs.
    #[default]
    Barrette,
    /// Two bars meeting in a V, with a bead at the join.
    Clip,
    /// Five petals round a centre.
    Flower,
}

/// A hairpin on the head.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HairpinParams {
    /// Barrette, clip or flower.
    pub style: HairpinStyle,
    /// Overall size in metres (a barrette's half length, a flower's petal reach).
    pub size: f64,
    /// Where on the head it sits when the part has no `attach`: a direction from the head centre (x is the character's
    /// left, y up, z forward).
    pub direction: [f64; 3],
    /// How far above the head surface it sits when the part has no `attach`, metres. The default clears a layer of hair.
    pub lift: f64,
    /// The pin's colour.
    pub primary: ColourRef,
    /// The studs', bead's or flower centre's colour.
    pub accent: ColourRef,
}

impl Default for HairpinParams {
    fn default() -> Self {
        Self {
            style: HairpinStyle::Barrette,
            size: 0.02,
            direction: [0.7, 0.55, 0.45],
            lift: 0.012,
            primary: primary_colour(),
            accent: accent_colour(),
        }
    }
}

impl Validate for HairpinParams {
    fn check(&self, c: &mut Check) {
        c.range("size", self.size, 0.008, 0.06);
        c.range("lift", self.lift, 0.0, 0.05);
        if !self.direction.iter().all(|v| v.is_finite()) || DVec3::from_array(self.direction).length() < 1e-6 {
            c.fail("direction", "give a direction with a length: three numbers that are not all 0");
        }
    }
}

/// The `hairpin` kind.
pub struct Hairpin;

impl Hairpin {
    /// On the head surface in `direction`, its `+Y` along the surface direction.
    fn on_head(ctx: &BuildCtx<'_>, p: &HairpinParams) -> Result<ResolvedAttach, BuildError> {
        let head = ctx.head()?;
        let dir = DVec3::from_array(p.direction).normalize_or_zero();
        #[allow(clippy::cast_possible_truncation)] // metres
        let at = head.point(dir.as_vec3(), p.lift as f32);
        Ok(ResolvedAttach::new(JointRef::HEAD, frame_up(DVec3::from(at), dir)))
    }

    fn shells(p: &HairpinParams, shells: &mut Shells<'_>) -> Result<(), KitError> {
        let s = p.size;
        let lift = DVec3::Y * (0.15 * s);
        match p.style {
            HairpinStyle::Barrette => {
                shells.ellipsoid(Slot::Primary, lift, DVec3::new(s, 0.18 * s, 0.28 * s), DQuat::IDENTITY)?;
                for k in [-0.5, 0.0, 0.5] {
                    let stud = lift + DVec3::new(k * s, 0.16 * s, 0.0);
                    shells.ellipsoid(Slot::Accent, stud, DVec3::splat(0.11 * s), DQuat::IDENTITY)?;
                }
            }
            HairpinStyle::Clip => {
                for turn in [12f64, -12.0] {
                    let q = DQuat::from_rotation_y(turn.to_radians());
                    shells.ellipsoid(
                        Slot::Primary,
                        lift + q * (DVec3::X * (0.55 * s)),
                        DVec3::new(0.62 * s, 0.12 * s, 0.17 * s),
                        q,
                    )?;
                    let q = DQuat::from_rotation_y((180.0 - turn).to_radians());
                    shells.ellipsoid(
                        Slot::Primary,
                        lift + q * (DVec3::X * (0.55 * s)),
                        DVec3::new(0.62 * s, 0.12 * s, 0.17 * s),
                        q,
                    )?;
                }
                shells.ellipsoid(Slot::Accent, lift + DVec3::Y * (0.12 * s), DVec3::splat(0.2 * s), DQuat::IDENTITY)?;
            }
            HairpinStyle::Flower => {
                for k in 0..5 {
                    let q = DQuat::from_rotation_y(TAU * fraction(k, 5));
                    shells.ellipsoid(
                        Slot::Primary,
                        lift + q * (DVec3::X * (0.5 * s)),
                        DVec3::new(0.5 * s, 0.12 * s, 0.27 * s),
                        q,
                    )?;
                }
                shells.ellipsoid(
                    Slot::Accent,
                    lift + DVec3::Y * (0.07 * s),
                    DVec3::splat(0.26 * s),
                    DQuat::IDENTITY,
                )?;
            }
        }
        Ok(())
    }
}

impl PartKind for Hairpin {
    type Params = HairpinParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "hairpin",
            summary: "Build a barrette, clip or flower pinned to the head or to an anchor.",
            category: PartCategory::Accessory,
            requires: &["head"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &HairpinParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let attach = attachment(ctx, |c| Self::on_head(c, p))?;
        let follow = Follow::Fixed(attach.joint.clone());
        let mut shells = Shells::new(&attach, &follow, Detail::of(ctx.quality));
        Self::shells(p, &mut shells).map_err(kit_error(ctx))?;
        output(ctx, shells, [&p.primary, &p.accent], 0.0, RigContribution::default())
    }

    fn recolour(p: &HairpinParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour_output(out, ctx, [&p.primary, &p.accent], 0.0)
    }
}

omuzaki_char_api::register_part_kind!(Hairpin);

// ------------------------------------------------------------------------------------------------ halo

/// A halo: a ring floating above the head.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HaloParams {
    /// Radius of the ring's centre line, metres.
    pub radius: f64,
    /// Radius of the ring's wire, metres.
    pub tube: f64,
    /// Height of the ring's centre above the attach point, metres.
    pub height: f64,
    /// Tilt a little as the head moves, on a spring chain.
    pub bob: bool,
    /// How much of its colour the ring gives off, `0..=1`.
    pub glow: f64,
    /// The ring colour.
    pub primary: ColourRef,
}

impl Default for HaloParams {
    fn default() -> Self {
        Self {
            radius: 0.09,
            tube: 0.005,
            height: 0.07,
            bob: true,
            glow: 0.6,
            primary: ColourRef::rgb(0xff, 0xe9, 0x9a),
        }
    }
}

impl Validate for HaloParams {
    fn check(&self, c: &mut Check) {
        c.range("radius", self.radius, 0.03, 0.3);
        c.range("tube", self.tube, 0.001, 0.02);
        c.range("height", self.height, 0.0, 0.3);
        c.range("glow", self.glow, 0.0, 1.0);
        if self.tube >= self.radius {
            c.fail("tube", "the wire must be thinner than the ring's radius");
        }
    }
}

/// The `halo` kind.
pub struct Halo;

impl Halo {
    /// Over the first `top` anchor a part published (the head's).
    fn over_top(ctx: &BuildCtx<'_>) -> Result<ResolvedAttach, BuildError> {
        let ((part, anchor), _) = ctx
            .published
            .get::<AnchorsFact>()
            .iter()
            .find(|((_, name), _)| name.as_str() == "top")
            .ok_or_else(|| ctx.fail("a halo floats over the `top` anchor of the head, and no part published one: add a head part, or give the halo an `attach`"))?;
        let attach = Attach::Anchor { part: part.clone(), anchor: anchor.clone(), offset: AttachOffset::default() };
        attach::resolve(ctx.part, &attach, ctx.published)
    }
}

impl PartKind for Halo {
    type Params = HaloParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "halo",
            summary: "Build a ring floating above the head, with a gentle bob.",
            category: PartCategory::Accessory,
            requires: &["head"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &HaloParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let attach = attachment(ctx, Self::over_top)?;
        let centre = DVec3::Y * p.height;
        let (follow, rig) = if p.bob {
            let (swing, rig) = spring_pair(ctx, "halo", &attach, centre, BOB)?;
            let to_local = attach.to_bind.inverse();
            (Follow::Swing { fixed: attach.joint.clone(), swing, ramp: 0.0, to_local }, rig)
        } else {
            (Follow::Fixed(attach.joint.clone()), RigContribution::default())
        };
        let mut shells = Shells::new(&attach, &follow, Detail::of(ctx.quality));
        let band = Ring {
            centre,
            ex: DVec3::X,
            ez: DVec3::Z,
            up: DVec3::Y,
            rx: p.radius,
            rz: p.radius,
            half_w: p.tube,
            half_t: p.tube,
        };
        shells.ring(Slot::Primary, &band).map_err(kit_error(ctx))?;
        #[allow(clippy::cast_possible_truncation)] // a fraction
        output(ctx, shells, [&p.primary, &p.primary], p.glow as f32, rig)
    }

    fn recolour(p: &HaloParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        #[allow(clippy::cast_possible_truncation)] // a fraction
        recolour_output(out, ctx, [&p.primary, &p.primary], p.glow as f32)
    }
}

omuzaki_char_api::register_part_kind!(Halo);
