//! From a [`CharacterDef`] to something the renderer can draw: the build pipeline and its result.
//!
//! The pipeline is the same for every implementation, so its rules live here once, in three public pieces:
//! 1. [`plan`]: validate the face profile and the weight brushes, then order the active parts
//!    ([`CharacterDef::build_order`]);
//! 2. for each part in that order, [`build_part`]: normalise its parameters through its kind and build it with a
//!    context that carries what earlier parts [`Published`], the [`BuildServices`] the app injected and the part's own
//!    random stream ([`part_rng`]). A builder that already has the part's [`PartOutput`] (a cache) skips this step;
//! 3. [`Assembler`]: [`add`](Assembler::add) each part's output in order (validate every mesh and texture, apply the
//!    character's weight brushes, collect meshes, materials and their classes, textures, runtimes and the report,
//!    absorb what the part publishes and contributes to the rig), then [`finish`](Assembler::finish) (check every
//!    mesh's material, assemble the [`CharacterRig`] from what the parts published and contributed, check every
//!    skinned bone).
//!
//! A built character is in the parts' own look (soft SD). The caller draws it in the character's look (LK-06) with
//! [`BuiltCharacter::apply_look`], a materials-only step that never rebuilds geometry and can be repeated with another
//! look at any time (the stage on a look switch, the exports before they gather): see [`crate::look`].
//!
//! A builder that keeps an earlier part's output may also [`recolour_part`] it: when only the palette changed, the kind
//! brings the output up to the new colours without rebuilding geometry ([`PartKind::recolour`](crate::PartKind::recolour)).
//!
//! [`RegistryBuilder`] is the reference sequential implementation: those three pieces in a loop. Another builder (the
//! caching, parallel one in `omuzaki-char`) differs only in where a part's [`PartOutput`] comes from, and because it
//! merges through the same [`Assembler`] it gives identical results (same meshes, same order, same errors).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use omuzaki_core::rng::Rng;
use omuzaki_face_api::{
    FaceLayerId, FaceLayerSpec, FaceProfile, FaceProjection, Palette, ParamError, ParamErrors, Validate,
};
use omuzaki_render_api::TextureDesc;

use crate::character::CharacterDef;
use crate::error::BuildError;
use crate::geometry::{MaterialSpec, PartMesh, TextureData, face_texture_desc};
use crate::ids::{AnchorId, CharId, MaterialName, PartId, PartKindId, TextureName, VariantId};
use crate::kind::{
    Anchor, AnchorsFact, BuildCtx, CollidersFact, FaceFact, FitFact, PartCategory, PartKindRegistration, PartOutput,
    Published, Quality, RecolourCtx, Recoloured, StatureFact, find_kind, requires_of,
};
use crate::look::{LookError, LookParams, MaterialClass};
use crate::rig::{self, CharacterRig, RigContribution};
use crate::runtime::PartRuntime;
use crate::services::BuildServices;
use crate::skin::{apply_brushes, check_brushes};
use crate::stature::{Stature, scale_output};

/// How to build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildOptions {
    /// Which variant was applied to `def` (recorded in the result; the caller resolves it with
    /// [`resolve_variant`](crate::resolve_variant) first so the builder never needs the variant files).
    pub variant: Option<VariantId>,
    /// Level of detail.
    pub quality: Quality,
    /// Build for export: simulated parts (hair, cloth, tails) also add their spring chains and skinned bake meshes
    /// ([`BuildCtx::rig_export`]). Off for the live stage, which simulates them itself.
    pub rig_export: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self { variant: None, quality: Quality::Live, rig_export: false }
    }
}

/// A mesh with its owner.
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltMesh {
    /// The part that made it.
    pub part: PartId,
    /// Its index in that part's own mesh list (what [`DynamicMesh::mesh`](crate::DynamicMesh) refers to).
    pub local: u16,
    /// The mesh.
    pub mesh: PartMesh,
}

/// A part's simulation.
pub struct RuntimeSlot {
    /// The part it belongs to.
    pub part: PartId,
    /// The simulation.
    pub runtime: Box<dyn PartRuntime>,
}

/// How the painted face attaches to the character.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceBinding {
    /// Where layers land on the head, in the painter's head-local units: the template's, not scaled to the character's
    /// stature, so the face textures keep their pixel size at any stature (materials' overlay windows are in metres).
    pub projection: FaceProjection,
    /// The face design, with the variant applied. It may name palette slots (`@eyes`): the stage resolves them with
    /// [`FaceProfile::resolve_colours`] before a painter reads it, and again whenever the palette changes, so a painter
    /// receives a slot-free profile.
    pub profile: FaceProfile,
}

/// What one part cost, for the report card (CB-92) and the frame-budget scheduler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartReport {
    /// The part.
    pub part: PartId,
    /// Its kind.
    pub kind: PartKindId,
    /// Meshes it made.
    pub meshes: usize,
    /// Vertices across them.
    pub vertices: usize,
    /// Triangles across them.
    pub triangles: usize,
    /// Notes from the generator.
    pub warnings: Vec<String>,
}

/// The build report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildReport {
    /// One entry per built part, in build order.
    pub parts: Vec<PartReport>,
}

impl BuildReport {
    /// Total triangles.
    pub fn triangles(&self) -> usize {
        self.parts.iter().map(|p| p.triangles).sum()
    }

    /// Total vertices.
    pub fn vertices(&self) -> usize {
        self.parts.iter().map(|p| p.vertices).sum()
    }
}

/// A built character: static data for the renderer plus the simulations the frame loop steps.
///
/// `#[non_exhaustive]`: other crates read the public fields and make one with [`BuiltCharacter::new`], never a struct
/// literal, so a field added here edits only this crate.
#[non_exhaustive]
pub struct BuiltCharacter {
    /// Which character.
    pub id: CharId,
    /// The variant applied, if any.
    pub variant: Option<VariantId>,
    /// The character as built (variant applied).
    pub def: CharacterDef,
    /// The palette the materials, textures and vertex colours were made with, and what stages resolve the slots of the
    /// face profile with. Materials hold plain colours, so changing this field alone recolours nothing: a builder
    /// applies a new palette (the caching builder's `rebuild_materials`), recolouring each part without rebuilding
    /// its geometry where its kind allows ([`PartKind::recolour`](crate::PartKind::recolour)).
    pub palette: Palette,
    /// Every mesh, in build order.
    pub meshes: Vec<BuiltMesh>,
    /// Materials by id, as they are drawn: [`part_materials`](Self::part_materials) in [`look`](Self::look).
    pub materials: BTreeMap<MaterialName, MaterialSpec>,
    /// Materials by id as the parts built them, before any look: what [`apply_look`](Self::apply_look) starts from, so
    /// looks never compound.
    pub part_materials: BTreeMap<MaterialName, MaterialSpec>,
    /// The class of every material (LK-06): declared by its part
    /// ([`PartOutput::material_classes`](crate::PartOutput::material_classes)) or inferred ([`MaterialClass::infer`]).
    pub material_classes: BTreeMap<MaterialName, MaterialClass>,
    /// The part that defined each material, whether or not any mesh uses it: where [`inherit_look`](Self::inherit_look)
    /// reports a material it could not draw.
    material_owners: BTreeMap<MaterialName, PartId>,
    /// The look `materials` are drawn in ([`look`](Self::look)). Private, so it is always one that validated: only
    /// [`apply_look`](Self::apply_look) sets it, which is what makes [`inherit_look`](Self::inherit_look) infallible.
    look: LookParams,
    /// Procedural textures by id.
    pub textures: BTreeMap<TextureName, TextureData>,
    /// How the painted face attaches; `None` if the character has no head part.
    pub face: Option<FaceBinding>,
    /// Attachment points by `(part, name)`.
    pub anchors: BTreeMap<(PartId, AnchorId), Anchor>,
    /// Simulations, in build order.
    pub runtimes: Vec<RuntimeSlot>,
    /// The skeleton and spring set, assembled from what the parts published and contributed. Every
    /// joint a mesh is skinned to resolves in it ([`CharacterRig::bone`]).
    pub rig: CharacterRig,
    /// The character's real-world stature, as the body part published it; `None` if the body is authored in metres.
    /// Every length above (meshes, joints, anchors, springs, colliders) is already in metres at this size.
    pub stature: Option<Stature>,
    /// What it cost.
    pub report: BuildReport,
    /// Growth probe (feature `growth-probe`): a field no other crate knows about. See `tests/growth_probe.rs`.
    #[cfg(feature = "growth-probe")]
    pub growth_probe: u8,
}

impl BuiltCharacter {
    /// A character with no parts: `def` as built, its palette, `rig` as the skeleton, and nothing else. The builder fills
    /// the rest; a test double sets the public fields it needs, or uses the `with_*` methods.
    pub fn new(id: CharId, def: CharacterDef, rig: CharacterRig) -> Self {
        Self {
            id,
            variant: None,
            palette: def.palette.clone(),
            def,
            meshes: Vec::new(),
            materials: BTreeMap::new(),
            part_materials: BTreeMap::new(),
            material_classes: BTreeMap::new(),
            material_owners: BTreeMap::new(),
            look: LookParams::default(),
            textures: BTreeMap::new(),
            face: None,
            anchors: BTreeMap::new(),
            runtimes: Vec::new(),
            rig,
            stature: None,
            report: BuildReport::default(),
            #[cfg(feature = "growth-probe")]
            growth_probe: 0,
        }
    }

    /// Sets the variant that was applied.
    #[must_use]
    pub fn with_variant(mut self, variant: Option<VariantId>) -> Self {
        self.variant = variant;
        self
    }

    /// Sets how the painted face attaches.
    #[must_use]
    pub fn with_face(mut self, face: Option<FaceBinding>) -> Self {
        self.face = face;
        self
    }

    /// Sets the real-world stature.
    #[must_use]
    pub fn with_stature(mut self, stature: Option<Stature>) -> Self {
        self.stature = stature;
        self
    }

    /// Sets what the build cost.
    #[must_use]
    pub fn with_report(mut self, report: BuildReport) -> Self {
        self.report = report;
        self
    }

    /// Draws the character in `look` (LK-06, [`crate::look`]): every material becomes its part's material with its
    /// class's settings applied ([`LookParams::class`], [`ClassLook::apply`](crate::ClassLook::apply)), and `look` is
    /// kept in [`look`](Self::look). Meshes, textures, the rig and the runtimes are not touched.
    ///
    /// It always starts from [`part_materials`](Self::part_materials), so applying one look after another gives exactly
    /// what applying the last one alone gives, and the empty look gives back the parts' materials byte for byte. Pure in
    /// the parts' materials, their classes and `look`.
    ///
    /// Returns the names of the materials whose spec changed, in name order: what a stage must update on the renderer.
    ///
    /// # Errors
    /// [`LookError::Invalid`] if `look` does not validate, or if it makes a material the renderer would reject (a
    /// finite width times a finite scale can overflow); the character is then unchanged.
    pub fn apply_look(&mut self, look: &LookParams) -> Result<Vec<MaterialName>, LookError> {
        look.validate()?;
        let drawn =
            self.draw(look).into_iter().map(|(name, d)| d.map(|spec| (name, spec))).collect::<Result<Vec<_>, _>>()?;
        self.look = look.clone();
        Ok(self.commit(drawn))
    }

    /// The look the materials are drawn in. Empty after a build: the parts' own materials, the soft-SD look.
    pub fn look(&self) -> &LookParams {
        &self.look
    }

    /// Draws this character in the look `previous` is drawn in, for a rebuild of the same character (a palette change
    /// rebuilds it whole), so the look survives the rebuild. Returns the materials that changed, as
    /// [`apply_look`](Self::apply_look) does. It can't fail: a material the look would make invalid on the rebuilt parts keeps its part's own spec, and a warning
    /// naming the part, the material and the reason goes to that part's [`PartReport::warnings`].
    pub fn inherit_look(&mut self, previous: &Self) -> Vec<MaterialName> {
        self.look.clone_from(&previous.look);
        // The rebuilt parts can differ from the ones the look was checked on; a material the look would make invalid
        // keeps its part's own spec rather than failing the rebuild.
        let mut warnings = Vec::new();
        let drawn = self
            .draw(&self.look)
            .into_iter()
            .map(|(name, d)| match d {
                Ok(spec) => (name, spec),
                Err(e) => {
                    warnings.push((name.clone(), e.to_string()));
                    let part = self.part_materials[&name].clone();
                    (name, part)
                }
            })
            .collect();
        for (material, reason) in warnings {
            let owner = self.material_owners.get(&material);
            if let Some(report) = owner.and_then(|o| self.report.parts.iter_mut().find(|p| p.part == *o)) {
                report.warnings.push(format!(
                    "part '{}', material '{}': the inherited look was not applied, the part's own material is kept: {reason}",
                    report.part, material.as_str()
                ));
            }
        }
        self.commit(drawn)
    }

    /// Every part material drawn in `look`, in name order; a material the look makes invalid carries the error.
    fn draw(&self, look: &LookParams) -> Vec<(MaterialName, Result<MaterialSpec, LookError>)> {
        self.part_materials
            .iter()
            .map(|(name, part)| {
                let class = self.material_classes.get(name).copied().unwrap_or(MaterialClass::Cloth);
                let drawn = look.class(class).try_apply(part, &format!("materials.{}", name.as_str()));
                (name.clone(), drawn)
            })
            .collect()
    }

    /// Stores `drawn`; returns the names whose drawn material changed.
    fn commit(&mut self, drawn: Vec<(MaterialName, MaterialSpec)>) -> Vec<MaterialName> {
        let mut changed = Vec::new();
        for (name, spec) in drawn {
            if self.materials.get(&name) != Some(&spec) {
                changed.push(name.clone());
                self.materials.insert(name, spec);
            }
        }
        changed
    }

    /// The painted-face textures the stage must create: one per face layer that some material composites (`Base` for
    /// the head, `Brows` when hair shows them through), with the spec the face painter fills and the renderer texture
    /// description. Empty if the character has no face. `base_px_per_m` is the quality knob
    /// ([`DEFAULT_PX_PER_M`](omuzaki_face_api::DEFAULT_PX_PER_M) unless the scheduler lowered it).
    pub fn face_textures(&self, base_px_per_m: f32) -> Vec<(FaceLayerSpec, TextureDesc)> {
        let Some(face) = &self.face else { return Vec::new() };
        let used: BTreeSet<FaceLayerId> =
            self.materials.values().flat_map(|m| m.overlays.iter().map(|o| o.layer)).collect();
        used.into_iter()
            .map(|layer| {
                let spec = FaceLayerSpec::for_layer(layer, &face.projection, base_px_per_m);
                (spec, face_texture_desc(&spec))
            })
            .collect()
    }
}

impl fmt::Debug for BuiltCharacter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuiltCharacter")
            .field("id", &self.id)
            .field("variant", &self.variant)
            .field("meshes", &self.meshes.len())
            .field("materials", &self.materials.keys().map(MaterialName::as_str).collect::<Vec<_>>())
            .field("runtimes", &self.runtimes.len())
            .field("bones", &self.rig.skeleton.len())
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

/// Builds characters. Implemented by `omuzaki-char` (caching, incremental) and by [`RegistryBuilder`].
pub trait CharacterBuilder: Send + Sync {
    /// Builds `def`. All or nothing: on error no partial character exists. `services` is what the app injects
    /// (assets, a rasteriser); it is not part of the build's identity, so a cache keys on `def` and `options` only.
    ///
    /// # Errors
    /// A [`BuildError`] naming the part and the problem.
    fn build(
        &self,
        id: &CharId,
        def: &CharacterDef,
        options: &BuildOptions,
        services: &BuildServices,
    ) -> Result<BuiltCharacter, BuildError>;
}

/// The reference builder: sequential, no caching, over the self-registered part kinds. It is [`plan`], then
/// [`build_part`] and [`Assembler::add`] for each part, then [`Assembler::finish`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RegistryBuilder;

/// FNV-1a (64-bit) of the id's UTF-8 bytes: the stream id [`part_rng`] forks with.
fn stream_of(part: &PartId) -> u64 {
    part.as_str().bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// Rejects weight brushes that name a part the character lacks or carry unusable numbers, before anything builds.
fn check_weight_brushes(def: &CharacterDef) -> Result<(), BuildError> {
    for (part, brushes) in &def.weight_brushes {
        let errors = if def.parts.contains_key(part) {
            check_brushes(brushes)
        } else {
            Err(ParamErrors(vec![ParamError {
                path: format!("weight_brushes/{part}"),
                message: "the character has no part with this id: remove the brushes or fix the id".to_owned(),
            }]))
        };
        errors.map_err(|errors| BuildError::Brush { part: part.clone(), errors })?;
    }
    Ok(())
}

/// The first half of the pipeline: what must hold before any part builds, and the order to build the parts in.
///
/// Validates the face profile, checks the weight brushes (each names a part the character has and carries usable
/// numbers), then orders the active parts ([`CharacterDef::build_order`]: parents, layers and kind requirements decide
/// who builds first; disabled parts and their children are left out). Build every part of the result, in this order,
/// and no other: a part's [`Published`] view is whatever the parts before it published.
///
/// It does not look up the kinds: an unregistered kind is reported by [`build_part`] when its turn comes, so the first
/// error of a build is the same whichever builder runs it.
///
/// # Errors
/// [`BuildError::Face`], [`BuildError::Brush`], or [`BuildError::Tree`] for a dangling parent or a dependency loop.
pub fn plan(def: &CharacterDef) -> Result<Vec<PartId>, BuildError> {
    def.face.validate().map_err(BuildError::Face)?;
    check_weight_brushes(def)?;
    Ok(def.build_order(&requires_of)?)
}

/// The random stream `part` draws from while it builds ([`BuildCtx::rng`]).
///
/// Every builder gives a part this stream, so a part built alone, in a parallel batch, after an edit to another part or
/// served from a cache is bit-identical to the same part in a full [`RegistryBuilder`] build. The derivation is part of
/// the contract (a test pins its first values):
/// 1. the stream id is the FNV-1a hash (64 bits: offset basis `0xcbf29ce484222325`, prime `0x100000001b3`) of the UTF-8
///    bytes of the part's id;
/// 2. the stream is `Rng::new(u64::from(seed)).fork(stream id)`: a fresh `SplitMix64` seeded with the character's seed,
///    forked once ([`Rng::fork`]).
///
/// So it depends on the character seed and the part's id and on nothing else: not on the other parts, the build order,
/// the quality, the options or whether anything was cached. Adding a part never shifts another part's numbers;
/// renaming a part does change its stream; changing `seed` changes every stream. Each call returns a fresh generator at
/// the start of the stream, and a part must draw only from [`BuildCtx::rng`] (not from a clock or a global) for a
/// rebuild to repeat.
pub fn part_rng(seed: u32, part: &PartId) -> Rng {
    Rng::new(u64::from(seed)).fork(stream_of(part))
}

/// The middle of the pipeline: builds one part through its kind.
///
/// Finds `part` in `def` and its registered kind (`find_kind`), normalises the stored parameters through the kind, and
/// calls its generator with a [`BuildCtx`] made of the character's palette and face, the part's own stream
/// ([`part_rng`]), `published` (what the parts before it in the [`plan`] published: [`Assembler::published`]), the
/// quality and `rig_export` of `options`, and `services`. The result is not validated or merged; hand it to
/// [`Assembler::add`].
///
/// Pure in its arguments: the same `def`, `part`, `options`, the same `published` and equivalent `services` give the same
/// output, which is what lets a cache return an earlier output instead of calling this again.
///
/// # Errors
/// [`BuildError::Part`] if `def` has no part `part`, [`BuildError::UnknownKind`] if its kind isn't registered,
/// [`BuildError::Params`] for invalid parameters, otherwise whatever the generator reports.
pub fn build_part(
    def: &CharacterDef,
    part: &PartId,
    options: &BuildOptions,
    services: &BuildServices,
    published: &Published,
) -> Result<PartOutput, BuildError> {
    let part_def = def.parts.get(part).ok_or_else(|| no_such_part(part))?;
    let reg = find_kind(part_def.kind.as_str())
        .ok_or_else(|| BuildError::UnknownKind { part: part.clone(), kind: part_def.kind.clone() })?;
    let mut ctx = BuildCtx::new(
        part,
        &def.template,
        &def.palette,
        &def.face,
        part_rng(def.seed, part),
        published,
        options.quality,
        services,
    )
    .with_rig_export(options.rig_export)
    .with_attach(part_def.attach.as_ref());
    let mut output = (reg.build)(&part_def.params, &mut ctx)?;
    if let Some(scale) = authored_scale(reg, &output, published) {
        scale_output(part, &mut output, scale)?;
    }
    Ok(output)
}

/// The factor that takes a part's output from the template's space to real size, if it needs one. The body and the head
/// are authored against the template; every other kind builds against what they published, already in metres.
fn authored_scale(reg: &PartKindRegistration, output: &PartOutput, published: &Published) -> Option<f32> {
    match (reg.meta)().category {
        PartCategory::Body => output.publishes.values::<StatureFact>().next().map(|s| s.scale),
        PartCategory::Head => published.get::<StatureFact>().map(|s| s.scale),
        _ => None,
    }
}

/// Brings the `output` of an earlier [`build_part`] up to the palette of `def`, without building the part again.
///
/// The counterpart of [`build_part`] for a cache: when `def` differs from the character `output` was built from in its
/// palette only (the same part, parameters, seed, face, quality and published view), this finds the part's registered
/// kind, normalises the stored parameters and calls [`PartKind::recolour`](crate::PartKind::recolour) with a
/// [`RecolourCtx`] of `def`'s palette and `services`. On [`Recoloured::Done`], `output` equals what [`build_part`] would
/// return for `def`. Nothing here validates or merges it; hand it to [`Assembler::add`] as usual.
///
/// On [`Recoloured::NeedsRebuild`], and on any error, `output` may be half recoloured: discard it and call
/// [`build_part`], which also gives the error a build reports (a recolour's error is never final).
///
/// # Errors
/// [`BuildError::Part`] if `def` has no part `part`, [`BuildError::UnknownKind`] if its kind isn't registered,
/// [`BuildError::Params`] for invalid parameters, otherwise whatever the kind's recolour reports.
pub fn recolour_part(
    def: &CharacterDef,
    part: &PartId,
    services: &BuildServices,
    output: &mut PartOutput,
) -> Result<Recoloured, BuildError> {
    let part_def = def.parts.get(part).ok_or_else(|| no_such_part(part))?;
    let reg = find_kind(part_def.kind.as_str())
        .ok_or_else(|| BuildError::UnknownKind { part: part.clone(), kind: part_def.kind.clone() })?;
    (reg.recolour)(&part_def.params, output, &RecolourCtx { part, palette: &def.palette, services })
}

fn no_such_part(part: &PartId) -> BuildError {
    BuildError::Part {
        part: part.clone(),
        message: "the character has no part with this id: build only the parts `plan` returned".to_owned(),
    }
}

/// How many meshes, vertices and triangles one part added.
struct MeshCounts {
    meshes: usize,
    vertices: usize,
    triangles: usize,
}

/// The last half of the pipeline: merges the parts' outputs, in build order, into a [`BuiltCharacter`].
///
/// Feed it every part of [`plan`], in order, with [`add`](Self::add), and call [`finish`](Self::finish). Everything a
/// builder does with a part's [`PartOutput`] once it has one happens here, in one order, so errors have one precedence
/// and a cached output merges exactly like a fresh one. [`published`](Self::published) is what the next part builds
/// against.
///
/// All or nothing, like the build: after an error from `add` or `finish` the assembler is unusable (it may hold half a
/// part), so drop it.
pub struct Assembler<'a> {
    id: &'a CharId,
    def: &'a CharacterDef,
    options: &'a BuildOptions,
    meshes: Vec<BuiltMesh>,
    runtimes: Vec<RuntimeSlot>,
    materials: BTreeMap<MaterialName, MaterialSpec>,
    material_classes: BTreeMap<MaterialName, MaterialClass>,
    material_owners: BTreeMap<MaterialName, PartId>,
    textures: BTreeMap<TextureName, TextureData>,
    report: BuildReport,
    rigs: Vec<(PartId, RigContribution)>,
    published: Published,
    /// The first head part added before the body published a stature, which therefore stayed at the template's size.
    unscaled_head: Option<PartId>,
}

impl<'a> Assembler<'a> {
    /// Starts an empty assembly of `def` as character `id`. `options.variant` is recorded in the result.
    pub fn new(id: &'a CharId, def: &'a CharacterDef, options: &'a BuildOptions) -> Self {
        Self {
            id,
            def,
            options,
            meshes: Vec::new(),
            runtimes: Vec::new(),
            materials: BTreeMap::new(),
            material_classes: BTreeMap::new(),
            material_owners: BTreeMap::new(),
            textures: BTreeMap::new(),
            report: BuildReport::default(),
            rigs: Vec::new(),
            published: Published::default(),
            unscaled_head: None,
        }
    }

    /// What the parts added so far published: the [`BuildCtx::published`] of the next part.
    pub fn published(&self) -> &Published {
        &self.published
    }

    /// Merges the output of `part`, which must be the next part of the [`plan`].
    ///
    /// In this order: validates each mesh and applies the character's weight brushes to it, collects the materials
    /// (ids are unique across parts) and their classes (declared, else inferred from the kind's category and the
    /// material), validates and collects the textures, keeps the runtime, adds the part to the
    /// report, checks what the part publishes (finite joints, colliders on humanoid bones), records what it contributes
    /// to the rig and absorbs what it publishes into [`published`](Self::published). The first failure is returned.
    ///
    /// # Errors
    /// [`BuildError::Part`] if `part` isn't in the character, was added already, published something unusable or
    /// declared a class for a material it doesn't build;
    /// [`BuildError::Mesh`] for a broken mesh; [`BuildError::Duplicate`] for a material or texture id another part
    /// defined.
    pub fn add(&mut self, part: &PartId, output: PartOutput) -> Result<(), BuildError> {
        self.merge(part, output, true)
    }

    /// [`add`](Self::add) for an output whose meshes already passed `add`'s mesh check and are unchanged since: a
    /// cached part. (A recoloured one goes through `add`: a recolour may rewrite baked vertex tints.)
    /// Everything else is done as `add` does it, the textures' check and the weight brushes included; only the meshes'
    /// validation, about a quarter of an all-cached rebuild, is skipped (#646).
    ///
    /// The caller guarantees the meshes are those of an output an earlier `add` (or `add_validated`) accepted: a mesh
    /// that never passed the check reaches the renderer unchecked.
    ///
    /// # Errors
    /// As [`add`](Self::add), except [`BuildError::Mesh`].
    pub fn add_validated(&mut self, part: &PartId, output: PartOutput) -> Result<(), BuildError> {
        self.merge(part, output, false)
    }

    fn merge(&mut self, part: &PartId, output: PartOutput, check_meshes: bool) -> Result<(), BuildError> {
        let kind = self.def.parts.get(part).ok_or_else(|| no_such_part(part))?.kind.clone();
        if self.rigs.iter().any(|(added, _)| added == part) {
            return Err(BuildError::Part {
                part: part.clone(),
                message: "the part was added twice: add each part of the plan once".to_owned(),
            });
        }
        self.check_stature_order(part, &kind, output.publishes.values::<StatureFact>().next().is_some())?;
        let counts = self.add_meshes(part, output.meshes, check_meshes)?;
        self.add_materials(part, &kind, output.materials, &output.material_classes)?;
        self.add_textures(part, output.textures)?;
        if let Some(runtime) = output.runtime {
            self.runtimes.push(RuntimeSlot { part: part.clone(), runtime });
        }
        self.report.parts.push(PartReport {
            part: part.clone(),
            kind,
            meshes: counts.meshes,
            vertices: counts.vertices,
            triangles: counts.triangles,
            warnings: output.warnings,
        });
        // Published colliders are also rig colliders: one declaration serves later parts and the spring set.
        for fit in output.publishes.values::<FitFact>() {
            rig::check_published_joints(part, Some(fit.as_ref()))?;
        }
        let mut collider_groups: Vec<_> = output.publishes.values::<CollidersFact>().cloned().collect();
        rig::check_published_colliders(part, &collider_groups)?;
        collider_groups.extend(output.rig.collider_groups);
        self.rigs.push((part.clone(), RigContribution { collider_groups, ..output.rig }));
        self.published.absorb(part, output.publishes)?;
        Ok(())
    }

    /// Refuses a character whose head was built before its body published the stature: the head would stay at the
    /// template's size while the body is scaled. Real head kinds list the body in their `requires`, which orders them.
    fn check_stature_order(
        &mut self,
        part: &PartId,
        kind: &PartKindId,
        publishes_stature: bool,
    ) -> Result<(), BuildError> {
        let is_head = find_kind(kind.as_str()).is_some_and(|reg| (reg.meta)().category == PartCategory::Head);
        if is_head && self.published.get::<StatureFact>().is_none() && self.unscaled_head.is_none() {
            self.unscaled_head = Some(part.clone());
        }
        match (&self.unscaled_head, publishes_stature) {
            (Some(head), true) => Err(BuildError::Part {
                part: part.clone(),
                message: format!(
                    "it publishes the character's stature after head part '{head}' was built, which left the head at \
                     the template's size: make the head part require the body so the body builds first"
                ),
            }),
            _ => Ok(()),
        }
    }

    /// Validates `meshes`, applies the part's weight brushes and collects them.
    fn add_meshes(&mut self, part: &PartId, meshes: Vec<PartMesh>, check: bool) -> Result<MeshCounts, BuildError> {
        let brushes = self.def.weight_brushes.get(part).map_or(&[][..], Vec::as_slice);
        let mut counts = MeshCounts { meshes: 0, vertices: 0, triangles: 0 };
        for (local, mut mesh) in meshes.into_iter().enumerate() {
            if check {
                let bad = |source| BuildError::Mesh { part: part.clone(), mesh: mesh.name.clone(), source };
                mesh.validate().map_err(bad)?;
            }
            apply_brushes(&mut mesh, brushes);
            counts.meshes += 1;
            counts.vertices += mesh.geometry.positions.len();
            counts.triangles += mesh.geometry.triangle_count();
            let local = u16::try_from(local)
                .map_err(|_| BuildError::Part { part: part.clone(), message: "more than 65535 meshes".to_owned() })?;
            self.meshes.push(BuiltMesh { part: part.clone(), local, mesh });
        }
        Ok(counts)
    }

    /// Collects a part's materials and their classes. A kind that isn't registered (only a hand-fed assembly has one)
    /// counts as an accessory, so its unmarked materials are cloth.
    fn add_materials(
        &mut self,
        part: &PartId,
        kind: &PartKindId,
        materials: Vec<(MaterialName, MaterialSpec)>,
        declared: &[(MaterialName, MaterialClass)],
    ) -> Result<(), BuildError> {
        if let Some((name, _)) = declared.iter().find(|(name, _)| !materials.iter().any(|(own, _)| own == name)) {
            return Err(BuildError::Part {
                part: part.clone(),
                message: format!("it declares a class for material '{name}', which it does not build"),
            });
        }
        let category = find_kind(kind.as_str()).map_or(PartCategory::Accessory, |reg| (reg.meta)().category);
        for (id, spec) in materials {
            let class = declared
                .iter()
                .find(|(name, _)| *name == id)
                .map_or_else(|| MaterialClass::infer(category, &spec), |(_, class)| *class);
            if self.materials.insert(id.clone(), spec).is_some() {
                return Err(BuildError::Duplicate { what: "material", id: id.to_string() });
            }
            self.material_owners.insert(id.clone(), part.clone());
            self.material_classes.insert(id, class);
        }
        Ok(())
    }

    fn add_textures(&mut self, part: &PartId, textures: Vec<(TextureName, TextureData)>) -> Result<(), BuildError> {
        for (id, data) in textures {
            data.validate().map_err(|message| BuildError::Part {
                part: part.clone(),
                message: format!("texture '{id}': {message}"),
            })?;
            if self.textures.insert(id.clone(), data).is_some() {
                return Err(BuildError::Duplicate { what: "texture", id: id.to_string() });
            }
        }
        Ok(())
    }

    /// Ends the assembly: checks every mesh names a defined material, assembles the [`CharacterRig`] from what the
    /// parts published and contributed, checks every bone a mesh is skinned to exists in it, and returns the character.
    ///
    /// # Errors
    /// [`BuildError::UnknownMaterial`], any rig error ([`BuildError::Skeleton`], [`BuildError::MissingParentBone`],
    /// [`BuildError::DuplicateBone`], [`BuildError::UnknownRigBone`], [`BuildError::DuplicateSpringName`],
    /// [`BuildError::Springs`]) or [`BuildError::UnknownMeshBone`].
    pub fn finish(self) -> Result<BuiltCharacter, BuildError> {
        if let Some(m) = self.meshes.iter().find(|m| !self.materials.contains_key(&m.mesh.material)) {
            return Err(BuildError::UnknownMaterial {
                part: m.part.clone(),
                mesh: m.mesh.name.clone(),
                material: m.mesh.material.clone(),
            });
        }
        let rig = rig::assemble(&self.published, &self.rigs)?;
        rig::check_mesh_bones(&rig, &self.meshes)?;
        Ok(BuiltCharacter {
            id: self.id.clone(),
            variant: self.options.variant.clone(),
            def: self.def.clone(),
            palette: self.def.palette.clone(),
            meshes: self.meshes,
            part_materials: self.materials.clone(),
            materials: self.materials,
            material_classes: self.material_classes,
            material_owners: self.material_owners,
            look: LookParams::default(),
            textures: self.textures,
            face: self
                .published
                .get::<FaceFact>()
                .map(|projection| FaceBinding { projection, profile: self.def.face.clone() }),
            anchors: self.published.get::<AnchorsFact>().clone(),
            runtimes: self.runtimes,
            rig,
            stature: *self.published.get::<StatureFact>(),
            report: self.report,
            #[cfg(feature = "growth-probe")]
            growth_probe: 0,
        })
    }
}

impl CharacterBuilder for RegistryBuilder {
    fn build(
        &self,
        id: &CharId,
        def: &CharacterDef,
        options: &BuildOptions,
        services: &BuildServices,
    ) -> Result<BuiltCharacter, BuildError> {
        let mut assembler = Assembler::new(id, def, options);
        for part in plan(def)? {
            let output = build_part(def, &part, options, services, assembler.published())?;
            assembler.add(&part, output)?;
        }
        assembler.finish()
    }
}
