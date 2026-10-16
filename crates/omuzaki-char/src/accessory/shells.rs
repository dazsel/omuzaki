//! Shared plumbing of the accessory kinds that fit the head or the back (horns, headphones, hats, glasses, wings,
//! tails): any number of material slots, shells placed by [`ResolvedAttach`], per-vertex skinning, the rig and the
//! materials.
//!
//! [`rigid`](super::rigid) does the same for the two-slot jewellery; this module takes the slot list as data so a kind
//! with a lens, a glow and a tip says so in one table.
//!
//! **Frames.** A *surface kind* (one that reads the head's surface) builds in bind space, because that is where the
//! published surfaces are. [`Shells::bind`] turns bind-space points into the accessory's local frame first (about the
//! default frame the kind passes), then places them with the part's `attach`: with no `attach` that is the identity, and
//! an `attach` on a surface kind moves the whole accessory as one rigid piece.

use omuzaki_anim_api::{BoneId, SpringChain, SpringParams, Transform};
use omuzaki_char_api::{
    BodyRegion, BoneName, BuildCtx, BuildError, ExtraBone, FitFact, JointFit, JointRef, MaterialName, PartMesh,
    PartOutput, RecolourCtx, Recoloured, RigContribution,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DMat4, DVec3};
use omuzaki_face_api::ColourRef;
use omuzaki_render_api::AlphaMode;

use super::attach::ResolvedAttach;
use super::rigid::{kit_error, material};
use crate::kit::KitError;
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::sweep::{Sweep, SweepOptions, sweep};

/// One material of an accessory: its palette colour and how it is shaded.
#[derive(Clone)]
pub(super) struct Slot {
    /// The suffix of the material and mesh name (`<part>-<suffix>`).
    pub suffix: &'static str,
    /// The palette colour.
    pub colour: ColourRef,
    /// How much of its colour the material gives off, `0..=1`.
    pub glow: f32,
    /// Coverage of a see-through material (a lens), or `None` for an opaque one.
    pub alpha: Option<u8>,
}

impl Slot {
    /// An opaque, matte slot.
    pub fn opaque(suffix: &'static str, colour: &ColourRef) -> Self {
        Self { suffix, colour: colour.clone(), glow: 0.0, alpha: None }
    }

    fn spec(&self, base: Colour) -> omuzaki_char_api::MaterialSpec {
        let mut spec = material(base, self.glow);
        if let Some(a) = self.alpha {
            spec.base.a = a;
            spec.alpha = AlphaMode::Blend;
            spec.receives_shadows = false;
        }
        spec
    }
}

/// The shells of one accessory, one builder per slot, and what places them.
pub(super) struct Shells<'a> {
    attach: &'a ResolvedAttach,
    /// Bind space to the accessory's local frame (the inverse of the default frame).
    to_local: DMat4,
    builders: Vec<MeshBuilder>,
}

impl<'a> Shells<'a> {
    /// Shells for `slots` slots, placed by `attach`; `default` is the frame a surface kind builds about (see the module
    /// docs).
    pub fn new(attach: &'a ResolvedAttach, default: &ResolvedAttach, slots: usize) -> Self {
        let builders = (0..slots).map(|_| MeshBuilder::new()).collect();
        Self { attach, to_local: default.to_bind.inverse(), builders }
    }

    /// Adds a grid of **bind-space** points to slot `slot`, rigidly on the attach joint. `inside` gives the interior
    /// reference of vertex `(i, j)` in bind space, which decides the winding. `wrap_i` and `wrap_j` close the grid.
    ///
    /// # Errors
    /// [`KitError`] from the grid.
    pub fn bind(
        &mut self,
        slot: usize,
        grid: &Grid,
        (wrap_i, wrap_j): (bool, bool),
        inside: &dyn Fn(usize, usize) -> DVec3,
    ) -> Result<(), KitError> {
        let skin = Skin::Bone(self.attach.joint.clone());
        self.bind_skinned(slot, grid, (wrap_i, wrap_j), inside, skin)
    }

    /// As [`bind`](Self::bind), with the skin given.
    ///
    /// # Errors
    /// [`KitError`] from the grid.
    pub fn bind_skinned(
        &mut self,
        slot: usize,
        grid: &Grid,
        wraps: (bool, bool),
        inside: &dyn Fn(usize, usize) -> DVec3,
        skin: Skin<'_>,
    ) -> Result<(), KitError> {
        let place = self.attach.to_bind * self.to_local;
        self.add(slot, place, grid, wraps, inside, skin)
    }

    fn add(
        &mut self,
        slot: usize,
        place: DMat4,
        grid: &Grid,
        (wrap_i, wrap_j): (bool, bool),
        inside: &dyn Fn(usize, usize) -> DVec3,
        skin: Skin<'_>,
    ) -> Result<(), KitError> {
        let placed =
            Grid::new(grid.rows(), grid.cols(), grid.points().iter().map(|&p| place.transform_point3(p)).collect())?;
        let inside = |i: usize, j: usize| place.transform_point3(inside(i, j));
        let options = GridOptions { wrap_i, wrap_j, inside: Some(&inside), skin, ..GridOptions::default() };
        self.builders[slot].add_grid(&placed, &options)
    }

    /// Finishes the shells into meshes and materials, with the rig the kind contributes.
    ///
    /// # Errors
    /// [`BuildError`] from the mesh, a colour or a name.
    pub fn output(self, ctx: &BuildCtx<'_>, slots: &[Slot], rig: RigContribution) -> Result<PartOutput, BuildError> {
        let mut out = PartOutput::new().with_rig(rig);
        // Publish the bones this part contributes so another part can attach to one: a joint-only fit never hides the
        // body from garments, and the rig still gets the bones from the contribution.
        let bones: Vec<_> =
            out.rig.bones.iter().map(|b| (JointRef::Named(b.name.clone()), b.rest.translation)).collect();
        if !bones.is_empty() {
            out.publishes.put::<FitFact>(std::sync::Arc::new(JointFit::new(bones)));
        }
        for (slot, builder) in slots.iter().zip(self.builders) {
            if builder.vertex_count() == 0 {
                continue;
            }
            let name = material_name(ctx.part, slot)?;
            let mut mesh: PartMesh =
                builder.finish(format!("{}-{}", ctx.part, slot.suffix), name.clone()).map_err(kit_error(ctx))?;
            mesh.casts_shadow = slot.alpha.is_none();
            out.meshes.push(mesh);
            out.materials.push((name, slot.spec(ctx.colour(&slot.colour)?)));
        }
        Ok(out)
    }
}

fn material_name(part: &omuzaki_char_api::PartId, slot: &Slot) -> Result<MaterialName, BuildError> {
    MaterialName::new(format!("{part}-{}", slot.suffix))
        .map_err(|e| BuildError::Part { part: part.clone(), message: format!("material name: {e}") })
}

/// Recolours `out` for the new palette: each material is rebuilt from its slot's colour, nothing else changes. A
/// material no slot names (a part built by another version) asks for a rebuild.
///
/// # Errors
/// [`BuildError`] if a colour does not resolve.
pub(super) fn recolour(out: &mut PartOutput, ctx: &RecolourCtx<'_>, slots: &[Slot]) -> Result<Recoloured, BuildError> {
    for (name, spec) in &mut out.materials {
        let Some(slot) = slots.iter().find(|s| name.as_str().ends_with(&format!("-{}", s.suffix))) else {
            return Ok(Recoloured::NeedsRebuild);
        };
        *spec = slot.spec(ctx.colour(&slot.colour)?);
    }
    Ok(Recoloured::Done)
}

/// A round tube along `path`, closed at both ends by a pole, as a sweep (rows along the path, columns round it, `centres`
/// the interior reference of its normals).
/// `radii(t)` gives the semi-axes along the carried normal and the binormal at fraction `t` of the path; `up` seeds the
/// normal. Pass the end points a hair beyond the visible ends so the poles are hidden.
///
/// # Errors
/// [`KitError`] from the sweep.
pub(super) fn closed_tube(
    path: &[DVec3],
    radii: impl Fn(f64) -> (f64, f64),
    seg: usize,
    up: DVec3,
) -> Result<Sweep, KitError> {
    let rad = |t: f64| if t <= 0.0 || t >= 1.0 { (0.0, 0.0) } else { radii(t) };
    let options = SweepOptions { up, ..SweepOptions::default() };
    sweep(path, rad, seg, &options)
}

/// The frame a kind that builds in bind space builds about (`joint`, no offset) and where the part is actually placed: its
/// `attach` when it has one, else that same frame.
///
/// # Errors
/// [`BuildError`] if the joint is not placed, or the `attach` does not resolve.
pub(super) fn frames_on(ctx: &BuildCtx<'_>, joint: &JointRef) -> Result<(ResolvedAttach, ResolvedAttach), BuildError> {
    let default = super::attach::on_bone(ctx, joint)?;
    let placed = super::attach::attachment(ctx, |_| Ok(default.clone()))?;
    Ok((placed, default))
}

/// [`frames_on`] the head joint.
///
/// # Errors
/// As [`frames_on`].
pub(super) fn head_frames(ctx: &BuildCtx<'_>) -> Result<(ResolvedAttach, ResolvedAttach), BuildError> {
    frames_on(ctx, &JointRef::HEAD)
}

/// A closed ellipsoid of semi-axes `radii` centred on `centre`, as a grid (rows pole to pole, columns round).
///
/// # Errors
/// [`KitError`] from the grid.
pub(super) fn ellipsoid(centre: DVec3, radii: DVec3, rows: usize, cols: usize) -> Result<Grid, KitError> {
    use crate::kit::curve::{TAU, fraction};
    Grid::from_fn(rows + 1, cols + 1, |i, j| {
        let (theta, phi) = (std::f64::consts::PI * fraction(i, rows), TAU * fraction(j, cols));
        centre + DVec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin()) * radii
    })
}

/// The direction from the head centre at `theta` from straight up and `phi` round it (`0` is the front, `+Z`, and
/// `pi / 2` the character's left, `+X`): the convention of [`loft_y`](crate::kit::loft::loft_y).
pub(super) fn polar(theta: f64, phi: f64) -> DVec3 {
    DVec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos())
}

/// The point of the head's surface in direction `dir`, pushed `offset` metres out.
pub(super) fn on_head(head: &dyn omuzaki_char_api::HeadSurface, dir: DVec3, offset: f64) -> DVec3 {
    #[allow(clippy::cast_possible_truncation)] // the surface is in f32 metres
    DVec3::from(head.point(dir.as_vec3(), offset as f32))
}

// ------------------------------------------------------------------------------------------------ the body

/// The neck's height on a standard template, metres: the body size the wings' and tails' lengths are authored for.
const STANDARD_NECK_HEIGHT: f64 = 1.328;

/// How big the body is next to a standard one, from the height of its neck: lengths of wings and tails scale with it, so
/// one parameter set fits the chibi and the tall template.
///
/// # Errors
/// [`BuildError`] if no part placed the neck.
pub(super) fn body_scale(ctx: &BuildCtx<'_>) -> Result<f64, BuildError> {
    let neck =
        ctx.joint(&JointRef::NECK).ok_or_else(|| ctx.fail("the body has no neck joint to size the accessory by"))?;
    Ok(f64::from(neck.y) / STANDARD_NECK_HEIGHT)
}

/// How many heights [`torso_back`] and [`torso_widths`] sample the torso at, hips to neck.
const TORSO_SAMPLES: usize = 21;

/// The back of the torso at each height, bind space: `(y, z)` of the most backward point of the torso's cross-section,
/// from the hips up to the base of the neck.
///
/// # Errors
/// [`BuildError`] if the body publishes no torso.
pub(super) fn torso_back(ctx: &BuildCtx<'_>) -> Result<Vec<(f64, f64)>, BuildError> {
    let fit = ctx.fit()?;
    (0..TORSO_SAMPLES)
        .map(|k| {
            let section = fit
                .section(BodyRegion::Torso, fraction_f32(k, TORSO_SAMPLES - 1))
                .ok_or_else(|| ctx.fail("the body has no torso section to fit the accessory to"))?;
            Ok((f64::from(section.centre.y), f64::from(section.centre.z) - f64::from(section.radii.y)))
        })
        .collect()
}

/// The width of the torso at each height, bind space: `(y, half width)` of the torso's cross-section across the body,
/// from the hips up to the base of the neck, at the heights [`torso_back`] samples.
///
/// # Errors
/// [`BuildError`] if the body publishes no torso.
pub(super) fn torso_widths(ctx: &BuildCtx<'_>) -> Result<Vec<(f64, f64)>, BuildError> {
    let fit = ctx.fit()?;
    (0..TORSO_SAMPLES)
        .map(|k| {
            let section = fit
                .section(BodyRegion::Torso, fraction_f32(k, TORSO_SAMPLES - 1))
                .ok_or_else(|| ctx.fail("the body has no torso section to fit the accessory to"))?;
            Ok((f64::from(section.centre.y), f64::from(section.radii.x)))
        })
        .collect()
}

#[allow(clippy::cast_possible_truncation)] // a fraction
fn fraction_f32(k: usize, last: usize) -> f32 {
    crate::kit::curve::fraction(k, last) as f32
}

/// How a chain of spring bones is laid out.
pub(super) struct Chain<'a> {
    /// Appended to the part's name in the names of the chain and its bones (`<kind>_<part>_<tag>`), `""` for none.
    pub tag: &'a str,
    /// The bone the chain's first bone hangs from.
    pub parent: JointRef,
    /// Where each bone rests, bind space, root first.
    pub points: &'a [DVec3],
    /// The spring parameters at the root and at the tip ([`SpringChain::tapered`]).
    pub root: SpringParams,
    /// The parameters at the last bone.
    pub tip: SpringParams,
    /// The bone whose space the chain is simulated in: `None` for the world.
    pub centre: Option<BoneId>,
}

/// Adds the bones of `chain` and a spring chain over them to `rig`, named `<kind>_<part>[_<tag>]`; the chain declares every
/// collider group an earlier part published. Returns the bones' joints, root first.
///
/// # Errors
/// [`BuildError`] if a name is not valid.
pub(super) fn spring_chain(
    ctx: &BuildCtx<'_>,
    kind: &str,
    rig: &mut RigContribution,
    chain: &Chain<'_>,
) -> Result<Vec<JointRef>, BuildError> {
    let tag = if chain.tag.is_empty() { String::new() } else { format!("_{}", chain.tag) };
    let mut ids = Vec::with_capacity(chain.points.len());
    let mut joints: Vec<JointRef> = Vec::with_capacity(chain.points.len());
    let mut parent = chain.parent.clone();
    for (i, point) in chain.points.iter().enumerate() {
        let name = BoneName::new(format!("{kind}_{}{tag}_{i}", ctx.part))
            .map_err(|e| ctx.fail(format!("spring bone name: {e}")))?;
        let rest = Transform::from_translation(point.as_vec3());
        ids.push(rig.add_bone(ExtraBone { name: name.clone(), parent: parent.clone(), rest }));
        parent = JointRef::Named(name);
        joints.push(parent.clone());
    }
    let mut spring = SpringChain::tapered(format!("{kind}_{}{tag}", ctx.part), &ids, chain.root, chain.tip);
    spring.collider_groups = ctx.colliders().iter().map(|group| group.name.clone()).collect();
    spring.center = chain.centre;
    rig.chains.push(spring);
    Ok(joints)
}

/// The weights of a point at `position` along a chain of joints at `knots` (increasing, the first below and the last at
/// or above any position used): a linear blend of the two joints either side of it, each joint's weight falling to
/// nothing at its neighbours.
pub(super) fn blend_along(joints: &[JointRef], knots: &[f64], position: f64) -> Vec<(JointRef, f32)> {
    debug_assert_eq!(joints.len(), knots.len());
    let last = joints.len() - 1;
    let above = knots.iter().position(|&k| k >= position).unwrap_or(last);
    if above == 0 {
        return vec![(joints[0].clone(), 1.0)];
    }
    let below = above - 1;
    let span = knots[above] - knots[below];
    #[allow(clippy::cast_possible_truncation)] // a weight in 0..=1
    let along = if span > 0.0 { ((position - knots[below]) / span).clamp(0.0, 1.0) as f32 } else { 1.0 };
    vec![(joints[below].clone(), 1.0 - along), (joints[above].clone(), along)]
}

/// [`blend_along`] with the joints evenly spaced over `0..=1`.
pub(super) fn chain_weights(joints: &[JointRef], position: f64) -> Vec<(JointRef, f32)> {
    let last = joints.len() - 1;
    let knots: Vec<f64> = (0..=last).map(|k| crate::kit::curve::fraction(k, last)).collect();
    blend_along(joints, &knots, position)
}
