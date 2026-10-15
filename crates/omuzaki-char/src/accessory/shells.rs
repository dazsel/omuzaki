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

use omuzaki_char_api::{
    BuildCtx, BuildError, FitFact, JointFit, JointRef, MaterialName, PartMesh, PartOutput, RecolourCtx, Recoloured,
    RigContribution,
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
        (wrap_i, wrap_j): (bool, bool),
        inside: &dyn Fn(usize, usize) -> DVec3,
        skin: Skin<'_>,
    ) -> Result<(), KitError> {
        let place = self.attach.to_bind * self.to_local;
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

/// The frame a surface kind builds about (the head joint, no offset) and where the part is actually placed: its `attach`
/// when it has one, else that same frame.
///
/// # Errors
/// [`BuildError`] if the head joint is not placed, or the `attach` does not resolve.
pub(super) fn head_frames(ctx: &BuildCtx<'_>) -> Result<(ResolvedAttach, ResolvedAttach), BuildError> {
    let default = super::attach::on_bone(ctx, &JointRef::HEAD)?;
    let placed = super::attach::attachment(ctx, |_| Ok(default.clone()))?;
    Ok((placed, default))
}
