//! Part kinds: how generators plug in (spec §18, §39 rule 2).
//!
//! A part kind is a generator with a typed parameter struct. It **registers itself** with [`register_part_kind!`], the
//! same way ops do, so adding a garment generator means adding one file and nothing edits a central list. The
//! parameter struct is the single source of truth: its serde form is what the project file stores, its JSON Schema is
//! what ops, the MCP server and the UI sliders read, and [`Validate`] is what rejects bad edits.
//!
//! Parts depend on each other only through what they **publish**: a head surface, the face projection, a body
//! fit and named anchors ([`Publishes`]). A part never reads another part's parameters.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;

use omuzaki_anim_api::{ColliderGroup, HumanoidBone, HumanoidPose};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Quat, Vec2, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_doc_api::Value;
use omuzaki_face_api::{
    ColourRef, FaceLayerId, FaceProfile, FaceProjection, FaceRect, Palette, ParamError, ParamErrors, Validate,
};
use schemars::{JsonSchema, Schema};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::character::Attach;
use crate::error::BuildError;
use crate::fact::{Append, Cell, Fact, FactId, Keyed, Last, Merged, Slot, default_of};
use crate::geometry::{FaceOverlay, MaterialSpec, PartMesh, TextureData};
use crate::ids::{AnchorId, BodyTemplateId, JointRef, MaterialName, PartId, PartKindId, TextureName};
use crate::rig::RigContribution;
use crate::runtime::PartRuntime;
use crate::services::BuildServices;
use crate::stature::Stature;

/// The broad role of a part kind; groups the UI palette and the report card, and picks default layering.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PartCategory {
    /// Body base: torso, limbs, hands, feet.
    Body,
    /// The head: cranium shape, ears, neck.
    Head,
    /// Hair.
    Hair,
    /// Clothing worn over the body: shirts, skirts, robes.
    Garment,
    /// Boots and shoes.
    Footwear,
    /// Hats, glasses, wings, tails, halos, jewellery.
    Accessory,
}

/// Who holds the rights to what a part kind builds. A kind is code, so this is a fact about its generator, not about
/// the character it is used in.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum KindProvenance {
    /// A procedural generator of this project (the built-in library): what it builds belongs to the project, so a
    /// character made only of such parts has nothing to attribute and no third-party terms.
    ProjectOwned,
    /// Anything else, or a kind that says nothing. Nothing is guessed: such a part needs a recorded source.
    #[default]
    External,
}

/// Static facts about a part kind.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PartMeta {
    /// The kind id (a slug): `head`, `hair`, `skirt`. It is the `kind` field of a part.
    pub id: &'static str,
    /// One line: what it builds, in the imperative.
    pub summary: &'static str,
    /// Its role.
    pub category: PartCategory,
    /// Kind ids that must build before it (hair needs `head`, garments need `body`).
    pub requires: &'static [&'static str],
}

/// A generator of one kind of part.
pub trait PartKind: 'static {
    /// The typed parameters. `Default` is what a freshly added part gets; [`Validate`] rejects out-of-range values;
    /// `deny_unknown_fields` on the struct makes typos fail.
    type Params: Validate + Serialize + DeserializeOwned + JsonSchema + Default + Send + Sync + 'static;

    /// The kind's static facts.
    fn meta() -> PartMeta;

    /// Who holds the rights to what the kind builds. Defaults to [`KindProvenance::External`]; the project's own
    /// generators say [`KindProvenance::ProjectOwned`].
    fn provenance() -> KindProvenance {
        KindProvenance::External
    }

    /// Builds the part. Must be a pure function of `params` and the context: same inputs, same output, on every
    /// platform (use `ctx.rng`, never global state or the clock).
    ///
    /// # Errors
    /// A [`BuildError`] naming the part, for example [`BuildError::Missing`] when a required surface wasn't published.
    fn build(params: &Self::Params, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError>;

    /// Brings `output`, which [`build`](Self::build) returned for an earlier palette, up to the palette in `ctx`
    /// without rebuilding its geometry (decision Q6: a palette change never rebuilds geometry).
    ///
    /// Everything a part takes from the palette is resolved while it builds: material colours, and for some kinds
    /// colours baked into the output (hair writes its streak tints into the vertex colours; a garment paints its
    /// pattern into a procedural texture). This hook redoes exactly that and nothing else: re-resolve the material
    /// colours, rewrite the baked tints, repaint the textures through [`RecolourCtx::paint_texture`]. It gets no
    /// published view and no random stream because it must not rebuild geometry.
    ///
    /// The contract, which the caching builder's differential test checks for every kind that overrides this:
    /// - **Exact.** When it returns [`Recoloured::Done`], `output` equals, byte for byte, what `build` returns for the
    ///   same parameters and the new palette (an `output` from `build` for the same parameters, quality, seed, face
    ///   and published view; only palette values differ).
    /// - **Colour only.** It changes materials, textures and vertex colours. Positions, indices, joints, rig, anchors
    ///   and published surfaces stay as they are: they don't depend on colour.
    /// - **Declines safely.** Anything it doesn't recognise in `output` (a mesh or material it expected is missing) is
    ///   [`Recoloured::NeedsRebuild`], never a guess. The caller then builds the part, and it discards `output`
    ///   whatever state this left it in, also after an error. So an error is never final: the part is built, and the
    ///   build's own error (or success) is what the caller reports.
    ///
    /// The default is [`Recoloured::NeedsRebuild`], so a kind that bakes colours in a way this hook doesn't cover stays
    /// correct. A kind whose colours reach the output only through its materials implements this by re-resolving
    /// them; share one function between `build` and `recolour` so they cannot drift apart.
    ///
    /// # Errors
    /// A [`BuildError`] naming the part, for example [`BuildError::Colour`] for a slot the new palette lacks.
    fn recolour(
        params: &Self::Params,
        output: &mut PartOutput,
        ctx: &RecolourCtx<'_>,
    ) -> Result<Recoloured, BuildError> {
        let _ = (params, output, ctx);
        Ok(Recoloured::NeedsRebuild)
    }
}

/// What [`PartKind::recolour`] did to a part's output.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Recoloured {
    /// The output is now what `build` would return for the new palette, byte for byte.
    Done,
    /// The output can't be brought up to the new palette without building the part again. The default of
    /// [`PartKind::recolour`].
    NeedsRebuild,
}

/// What a part sees while it is recoloured: the new palette and the build services. Nothing a build would use to make
/// geometry (what the parts before it published, a random stream), so a recolour cannot rebuild any.
pub struct RecolourCtx<'a> {
    /// The part being recoloured.
    pub part: &'a PartId,
    /// The character's new colours. Resolve a part's [`ColourRef`]s with [`RecolourCtx::colour`].
    pub palette: &'a Palette,
    /// What the app injected: reach it through [`RecolourCtx::paint_texture`] and [`RecolourCtx::asset_or_fail`].
    pub services: &'a BuildServices,
}

impl RecolourCtx<'_> {
    /// The colour `r` names, resolved through the new palette (as [`BuildCtx::colour`] does for a build).
    ///
    /// # Errors
    /// [`BuildError::Colour`] if `r` is a slot the palette doesn't define.
    pub fn colour(&self, r: &ColourRef) -> Result<Colour, BuildError> {
        self.palette.resolve(r).map_err(|error| BuildError::Colour { part: self.part.clone(), error })
    }

    /// A [`BuildError::Part`] for this part.
    pub fn fail(&self, message: impl Into<String>) -> BuildError {
        BuildError::Part { part: self.part.clone(), message: message.into() }
    }
}

/// Everything the builder and the five surfaces need to know about one part kind.
pub struct PartKindRegistration {
    /// The kind's static facts.
    pub meta: fn() -> PartMeta,
    /// Who holds the rights to what the kind builds.
    pub provenance: fn() -> KindProvenance,
    /// The JSON Schema of its parameters (fully inlined; this is the MCP tool schema for edits).
    pub schema: fn() -> Schema,
    /// The parameters of a new part, as stored.
    pub defaults: fn() -> Value,
    /// Decodes and validates stored parameters, returning them *normalised* (defaults filled, order fixed). Ops call
    /// this on an edited copy before they emit a patch.
    pub check: fn(&Value) -> Result<Value, ParamErrors>,
    /// Decodes stored parameters and builds the part.
    pub build: fn(&Value, &mut BuildCtx<'_>) -> Result<PartOutput, BuildError>,
    /// Decodes stored parameters and recolours an earlier build of the part ([`PartKind::recolour`]).
    pub recolour: fn(&Value, &mut PartOutput, &RecolourCtx<'_>) -> Result<Recoloured, BuildError>,
}

impl PartKindRegistration {
    /// The registration for kind `K`; `const` so [`register_part_kind!`] works at link time.
    pub const fn of<K: PartKind>() -> Self {
        Self {
            meta: K::meta,
            provenance: K::provenance,
            schema: schema_of::<K::Params>,
            defaults: defaults_of::<K::Params>,
            check: check_as::<K>,
            build: build_as::<K>,
            recolour: recolour_as::<K>,
        }
    }
}

/// Inlined, like op schemas: agents handle flat schemas far more reliably than `$ref`s.
fn schema_of<P: JsonSchema>() -> Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<P>()
}

fn defaults_of<P: Default + Serialize>() -> Value {
    serde_json::to_value(P::default()).unwrap_or(Value::Null)
}

fn decode<K: PartKind>(stored: &Value) -> Result<K::Params, ParamErrors> {
    let params: K::Params = serde_json::from_value(stored.clone())
        .map_err(|e| ParamErrors(vec![ParamError { path: "params".to_owned(), message: e.to_string() }]))?;
    params.validate()?;
    Ok(params)
}

fn check_as<K: PartKind>(stored: &Value) -> Result<Value, ParamErrors> {
    let params = decode::<K>(stored)?;
    serde_json::to_value(params)
        .map_err(|e| ParamErrors(vec![ParamError { path: "params".to_owned(), message: e.to_string() }]))
}

fn build_as<K: PartKind>(stored: &Value, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
    let params = decode::<K>(stored).map_err(|errors| BuildError::Params { part: ctx.part.clone(), errors })?;
    K::build(&params, ctx)
}

fn recolour_as<K: PartKind>(
    stored: &Value,
    output: &mut PartOutput,
    ctx: &RecolourCtx<'_>,
) -> Result<Recoloured, BuildError> {
    let params = decode::<K>(stored).map_err(|errors| BuildError::Params { part: ctx.part.clone(), errors })?;
    K::recolour(&params, output, ctx)
}

inventory::collect!(PartKindRegistration);

/// Registers a part kind so the builder, ops, the MCP server, the UI and docs pick it up. Put it next to the kind.
///
/// ```ignore
/// omuzaki_char_api::register_part_kind!(Hair);
/// ```
#[macro_export]
macro_rules! register_part_kind {
    ($ty:ty) => {
        $crate::__inventory::submit! { $crate::PartKindRegistration::of::<$ty>() }
        $crate::__record_registration!("part_kind", $ty);
    };
}

/// Every registered part kind, in link order. Surfaces sort by id before showing them.
pub fn kinds() -> impl Iterator<Item = &'static PartKindRegistration> {
    inventory::iter::<PartKindRegistration>.into_iter()
}

/// The registration for the kind called `id`. Linear: the builder builds an index once.
pub fn find_kind(id: &str) -> Option<&'static PartKindRegistration> {
    kinds().find(|r| (r.meta)().id == id)
}

/// The kinds a part of `kind` needs built before it ([`PartMeta::requires`]): the rule the builder orders by, so an op
/// that checks a character's build order ([`CharacterDef::build_order`](crate::CharacterDef::build_order)) agrees with
/// the build. An unregistered kind needs nothing here; the builder reports it when it builds.
pub fn requires_of(kind: &PartKindId) -> Vec<PartKindId> {
    find_kind(kind.as_str())
        .map(|r| (r.meta)().requires.iter().filter_map(|s| PartKindId::new(*s).ok()).collect())
        .unwrap_or_default()
}

// ------------------------------------------------------------------------------------------------ published surfaces

/// The head's outer surface, published by the head part. Hair, hats, glasses and ears sit on it.
pub trait HeadSurface: Debug + Send + Sync {
    /// The head centre, bind space. Surface directions are measured from here.
    fn centre(&self) -> Vec3;

    /// Distance from the centre to the surface along the unit direction `dir`.
    fn radius(&self, dir: Vec3) -> f32;

    /// The point on the surface in direction `dir`, pushed `offset` metres outward.
    fn point(&self, dir: Vec3, offset: f32) -> Vec3 {
        self.centre() + dir * (self.radius(dir) + offset)
    }
}

/// A region of the body that garments wrap.
///
/// Non-exhaustive: non-human templates (CB-05: wings, tails, extra limbs) arrive as new variants, so a `match` outside
/// this crate needs a wildcard arm.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum BodyRegion {
    /// Hips to neck base.
    Torso,
    /// Neck base to head.
    Neck,
    /// Left shoulder to wrist (the character's left, +x).
    ArmL,
    /// Right shoulder to wrist.
    ArmR,
    /// Left hip to ankle.
    LegL,
    /// Right hip to ankle.
    LegR,
    /// Left foot, heel to toe, so boots and shoes can wrap it. The section's axis points heel to toe, `radii.x` is
    /// across the foot and `radii.y` its height.
    FootL,
    /// Right foot, heel to toe (the same frame as [`BodyRegion::FootL`]).
    FootR,
}

/// An elliptical cross-section of a body region.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Section {
    /// Centre of the ellipse.
    pub centre: Vec3,
    /// The region's axis at this point (unit): the ellipse lies in the plane perpendicular to it.
    pub axis: Vec3,
    /// Half-extents: `x` across the body (left-right), `y` through it (front-back). For a foot, `x` is across the
    /// foot and `y` its height above the sole.
    pub radii: Vec2,
}

/// What garments need to know about whatever lies inside them: the bare body at first, then each garment's outer
/// envelope in layering order (CB-40: "fitted to the body template with layering order"; CB-42 fit checking).
pub trait BodyFit: Debug + Send + Sync {
    /// The cross-section of `region` at `t` in `0..=1` along its axis (0 at the start named in [`BodyRegion`]).
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section>;

    /// The bind-space position of a bone's joint.
    fn joint(&self, joint: &JointRef) -> Option<Vec3>;

    /// Whether garments built later should wrap this fit. `false` for a fit that only reports joints
    /// ([`JointFit`](crate::JointFit)), so publishing one never hides the body from the garments over it. The rig asks
    /// non-envelope fits for a joint first ([`Published::joint`]), because an envelope delegates its joints to the fit
    /// under it and would repeat the body's undropped ones.
    fn envelope(&self) -> bool {
        true
    }

    /// The pose this fit's joints and the body's meshes are in, in the normalised space of [`HumanoidPose`]. The
    /// default is the T-pose (identity). A template sculpted in an A-pose returns the arms' hang here: the rig then
    /// rests in the T and is bound in this pose ([`CharacterRig::bind`](crate::CharacterRig::bind), decision 0014), and
    /// [`BodyFit::joint`] keeps reporting where the joints are in it. A fit that wraps another repeats its bind.
    fn bind(&self) -> HumanoidPose {
        HumanoidPose::REST
    }
}

/// A named attachment point.
#[derive(Clone, Debug, PartialEq)]
pub struct Anchor {
    /// The bone the point follows.
    pub bone: JointRef,
    /// Bind-space position.
    pub position: Vec3,
    /// Bind-space orientation.
    pub rotation: Quat,
}

/// What one part makes available to the parts after it: a [`Fact`] value per [`Publishes::put`]. The facts of this
/// crate are [`HeadFact`], [`FaceFact`], [`FitFact`], [`AnchorsFact`], [`CollidersFact`] and [`StatureFact`]; any crate
/// may define more ([`fact!`](crate::fact)).
#[derive(Clone, Default)]
pub struct Publishes {
    /// By fact name, so the merge order is deterministic.
    slots: BTreeMap<&'static str, Slot>,
    /// The first value put for a fact whose name another fact already has here: [`Published::absorb`] refuses it.
    clash: Option<String>,
}

impl Publishes {
    /// Publishes one value of fact `F`. An [`Append`] or [`Keyed`] fact takes several `put`s, merged in the order they
    /// were made.
    pub fn put<F: Fact>(&mut self, value: F::Value)
    where
        F::Value: Clone,
        F::Merged: Clone,
    {
        let slot = self.slots.entry(F::NAME).or_insert_with(Slot::new::<F>);
        match slot.id.clash(FactId::of::<F>(), F::NAME) {
            None => slot.values.push(Cell::new(value)),
            Some(message) => {
                self.clash.get_or_insert(message);
            }
        }
    }

    /// [`Publishes::put`] for building a value in one expression.
    #[must_use]
    pub fn with<F: Fact>(mut self, value: F::Value) -> Self
    where
        F::Value: Clone,
        F::Merged: Clone,
    {
        self.put::<F>(value);
        self
    }

    /// The values of fact `F` put so far, in order (what the part itself published, before any merge).
    pub fn values<F: Fact>(&self) -> impl Iterator<Item = &F::Value> {
        self.slots
            .get(F::NAME)
            .filter(|slot| slot.id.is::<F>())
            .into_iter()
            .flat_map(|slot| slot.values.iter())
            .filter_map(Cell::get::<F::Value>)
    }

    /// The values of fact `F` put so far, in order, to change in place (the build scales them to real size).
    pub fn values_mut<F: Fact>(&mut self) -> impl Iterator<Item = &mut F::Value> {
        self.slots
            .get_mut(F::NAME)
            .filter(|slot| slot.id.is::<F>())
            .into_iter()
            .flat_map(|slot| slot.values.iter_mut())
            .filter_map(Cell::get_mut::<F::Value>)
    }

    /// Whether nothing was put: the part is silent to the parts after it.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty() && self.clash.is_none()
    }
}

impl Debug for Publishes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut facts = f.debug_map();
        for (name, slot) in &self.slots {
            facts.entry(name, &slot.values.len());
        }
        facts.finish()
    }
}

/// Everything published so far during one build. Later parts read it; [`Published::absorb`] is the only writer.
#[derive(Clone, Default)]
pub struct Published {
    /// The merged value of every fact published so far, by fact name.
    merged: BTreeMap<&'static str, Merged>,
}

impl Debug for Published {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.merged.keys()).finish()
    }
}

// ------------------------------------------------------------------------------------------------ facts

crate::fact! {
    /// The head surface (published by `head`); the last part that publishes one wins.
    pub HeadFact = "head", Last<Arc<dyn HeadSurface>>;
}

crate::register_fact!(HeadFact);

crate::fact! {
    /// Where painted face layers land on the head (published by `head`); the last part that publishes one wins.
    pub FaceFact = "face", Last<FaceProjection>;
}

crate::register_fact!(FaceFact);

crate::fact! {
    /// Anchors for accessories, keyed by the publishing part and the anchor name.
    pub AnchorsFact = "anchors", Keyed<AnchorId, Anchor>;
}

crate::register_fact!(AnchorsFact);

crate::fact! {
    /// Collider groups later parts may use ([`BuildCtx::colliders`]): hair and cloth keep out of the head and
    /// shoulders. They also join the rig's spring set. Colliders must sit on humanoid bones (provisional ids from
    /// [`humanoid_id`](crate::humanoid_id)): the only bones another part can locate. Appended in part order.
    pub CollidersFact = "colliders", Append<ColliderGroup>;
}

crate::register_fact!(CollidersFact);

/// A part's outer envelope, for garments built over it ([`FitFact`]).
#[derive(Clone, Debug, Default)]
pub struct Fits {
    /// Fit surfaces, innermost first: the bare body, then each garment's envelope in build order.
    pub fits: Vec<Arc<dyn BodyFit>>,
    /// The first part that published a fit bound in anything but the T-pose, and that pose ([`Published::bind`]).
    pub bound_by: Option<(PartId, HumanoidPose)>,
}

/// The fit surfaces parts publish, appended in part order. Every fit not in the T-pose must share one bind pose
/// (decision 0014): a character has one, so the build refuses instead of picking one. Fits that agree within 1e-6 per
/// component, or that are in the T-pose, are fine.
pub struct FitFact;

impl Fact for FitFact {
    const NAME: &'static str = "fit";
    type Value = Arc<dyn BodyFit>;
    type Merged = Fits;

    fn merge(merged: &mut Fits, part: &PartId, fit: Arc<dyn BodyFit>) -> Result<(), String> {
        let bind = fit.bind();
        if bind != HumanoidPose::REST {
            match &merged.bound_by {
                Some((first, pose)) if !same_bind(pose, &bind) => {
                    return Err(format!(
                        "its fit is bound in a different pose than the fit of part '{first}': a character has one \
                         bind pose (decision 0014), so both parts must sculpt in the same one"
                    ));
                }
                Some(_) => {}
                None => merged.bound_by = Some((part.clone(), bind)),
            }
        }
        merged.fits.push(fit);
        Ok(())
    }
}

crate::register_fact!(FitFact);

/// The character's real-world stature, published by the body part (nothing for a body authored in metres). The build
/// scales the body and the head by [`Stature::scale`] before any later part is built
/// ([`scale_output`](crate::scale_output)); what the body publishes here is therefore the size it already has. It must
/// pass [`Stature::check`], and a second, different stature is an error (an equal one is fine).
pub struct StatureFact;

impl Fact for StatureFact {
    const NAME: &'static str = "stature";
    type Value = Stature;
    type Merged = Option<Stature>;

    fn merge(merged: &mut Option<Stature>, _part: &PartId, stature: Stature) -> Result<(), String> {
        stature.check()?;
        if merged.is_some_and(|first| first != stature) {
            return Err("it publishes a different stature than an earlier part: a character has one stature".to_owned());
        }
        *merged = Some(stature);
        Ok(())
    }
}

crate::register_fact!(StatureFact);

/// How far two bind poses may differ, per quaternion component and per axis of the hips offset, and still be the same
/// pose: the rounding of two parts that each computed it.
const BIND_TOLERANCE: f32 = 1e-6;

/// Whether `a` and `b` are one bind pose within [`BIND_TOLERANCE`]. A quaternion and its negation are one rotation.
fn same_bind(a: &HumanoidPose, b: &HumanoidPose) -> bool {
    let close = |x: f32, y: f32| (x - y).abs() <= BIND_TOLERANCE;
    let hips = a.hips_offset.to_array().into_iter().zip(b.hips_offset.to_array()).all(|(x, y)| close(x, y));
    let same_rotation = |p: Quat, q: Quat| {
        let (p, q) = (p.to_array(), q.to_array());
        p.iter().zip(&q).all(|(x, y)| close(*x, *y)) || p.iter().zip(&q).all(|(x, y)| close(*x, -*y))
    };
    hips && HumanoidBone::ALL.into_iter().all(|bone| same_rotation(a.get(bone), b.get(bone)))
}

impl Published {
    /// What fact `F` has merged so far: its `Default` when no part published it.
    pub fn get<F: Fact>(&self) -> &F::Merged {
        self.merged
            .get(F::NAME)
            .filter(|merged| merged.id.is::<F>())
            .and_then(|merged| merged.cell.get::<F::Merged>())
            .unwrap_or_else(|| default_of::<F::Merged>())
    }

    /// Adds what `part` published, merging each fact in name order and each fact's values in the order they were put.
    ///
    /// # Errors
    /// [`BuildError::Part`] with the message of the first fact that refuses a value, for example a fit bound in a pose
    /// that differs from the one an earlier part's fit is bound in (neither being the T-pose), or a second, different
    /// stature, or a fact whose name a different fact already has. Nothing is absorbed then.
    pub fn absorb(&mut self, part: &PartId, p: Publishes) -> Result<(), BuildError> {
        if let Some(message) = p.clash {
            return Err(BuildError::Part { part: part.clone(), message });
        }
        if p.slots.is_empty() {
            // A silent part changes nothing, so it copies nothing.
            return Ok(());
        }
        let mut next = self.merged.clone();
        for slot in p.slots.into_values() {
            slot.merge_into(&mut next, part).map_err(|message| BuildError::Part { part: part.clone(), message })?;
        }
        self.merged = next;
        Ok(())
    }

    /// The pose the character was built in: the bind of the first published fit that is bound in anything but the
    /// T-pose ([`BodyFit::bind`]), the T-pose when none is. The rig is bound in it. [`Published::absorb`] refuses two
    /// fits that disagree, so there is only one to find.
    pub fn bind(&self) -> HumanoidPose {
        self.get::<FitFact>()
            .fits
            .iter()
            .map(|f| f.bind())
            .find(|b| *b != HumanoidPose::REST)
            .unwrap_or(HumanoidPose::REST)
    }

    /// The factor the body and head were scaled by to reach real size (`1` when the body published no stature).
    pub fn scale(&self) -> f32 {
        self.get::<StatureFact>().map_or(1.0, |s| s.scale)
    }

    /// The outermost fit that garments wrap ([`BodyFit::envelope`]).
    pub fn outer_fit(&self) -> Option<&Arc<dyn BodyFit>> {
        self.get::<FitFact>().fits.iter().rev().find(|f| f.envelope())
    }

    /// Where `joint` is in the rig: fits that only move joints ([`JointFit`](crate::JointFit), `!envelope()`) are asked
    /// first, last published first, and then the envelope fits, innermost first. A joint a part moves (the head drop)
    /// therefore wins over the body it moves it on whatever the build order, and over a garment envelope that merely
    /// repeats the body's joints. The rig is assembled from this.
    pub fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        let fits = &self.get::<FitFact>().fits;
        fits.iter()
            .rev()
            .filter(|f| !f.envelope())
            .find_map(|f| f.joint(joint))
            .or_else(|| fits.iter().filter(|f| f.envelope()).find_map(|f| f.joint(joint)))
    }
}

/// `projection` with its rectangle and fade range multiplied by `s`: the face window in real-size metres.
fn scaled_projection(projection: &FaceProjection, s: f32) -> FaceProjection {
    let r = projection.rect;
    FaceProjection {
        rect: FaceRect { x0: r.x0 * s, x1: r.x1 * s, y0: r.y0 * s, y1: r.y1 * s },
        front_fade: projection.front_fade.map(|z| z * s),
    }
}

// ------------------------------------------------------------------------------------------------ build context and output

/// How much detail to build: tessellation, strand counts, texture sizes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Quality {
    /// Real-time use (§10 budgets).
    Live,
    /// Offline renders and export: denser geometry, larger textures.
    Offline,
}

/// What a part sees while it builds.
pub struct BuildCtx<'a> {
    /// The part being built.
    pub part: &'a PartId,
    /// The character's body template ([`CharacterDef::template`](crate::CharacterDef)): the body part lofts its mesh
    /// from it, and no other part needs it (everything else reads what the body published).
    pub template: &'a BodyTemplateId,
    /// The character's colours. Resolve a part's [`ColourRef`](omuzaki_face_api::ColourRef)s with [`BuildCtx::colour`]
    /// when building materials (the renderer's material types hold plain colours). A palette change doesn't rebuild the
    /// part if its kind implements [`PartKind::recolour`].
    pub palette: &'a Palette,
    /// The character's face design (the head part reads the layout; hair reads the brows style).
    pub face: &'a FaceProfile,
    /// Random numbers for this part only: seeded from the character seed and the part id, so a rebuild is identical
    /// and adding a part never shifts another part's hair.
    pub rng: Rng,
    /// What earlier parts published.
    pub published: &'a Published,
    /// Level of detail.
    pub quality: Quality,
    /// What the app injected: reach it through [`BuildCtx::asset`] and [`BuildCtx::paint_texture`].
    pub services: &'a BuildServices,
    /// Whether the build is for export ([`BuildOptions::rig_export`](crate::BuildOptions)): simulated parts (hair,
    /// cloth, tails) then also add their spring chains and skinned bake meshes, which a runtime build doesn't need.
    pub rig_export: bool,
    /// Where the part is fixed ([`PartDef::attach`](crate::PartDef)), `None` for a part skinned to the template
    /// skeleton. Accessories resolve it against `published` (anchors) and [`BuildCtx::joint`]. The builder keys the
    /// part's output on it, so editing the attachment rebuilds the part.
    pub attach: Option<&'a Attach>,
}

impl BuildCtx<'_> {
    /// The colour `r` names, resolved through the character's palette.
    ///
    /// # Errors
    /// [`BuildError::Colour`] if `r` is a slot the palette doesn't define.
    pub fn colour(&self, r: &ColourRef) -> Result<Colour, BuildError> {
        self.palette.resolve(r).map_err(|error| BuildError::Colour { part: self.part.clone(), error })
    }

    /// A [`BuildError::Part`] for this part.
    pub fn fail(&self, message: impl Into<String>) -> BuildError {
        BuildError::Part { part: self.part.clone(), message: message.into() }
    }

    /// The head surface, or [`BuildError::Missing`]. Returns a shared handle (not a borrow of the context), so a part
    /// can keep using `ctx.rng` while it holds the head.
    ///
    /// # Errors
    /// `Missing` if no head part built before this one.
    pub fn head(&self) -> Result<Arc<dyn HeadSurface>, BuildError> {
        self.published
            .get::<HeadFact>()
            .clone()
            .ok_or_else(|| BuildError::Missing { part: self.part.clone(), needs: "a head surface" })
    }

    /// The overlay that composites face layer `layer` onto this part's material: the planar mapping is placed on the
    /// published head. Hair uses it for [`FaceLayerId::Brows`] when brows show through; the head itself builds
    /// its own `Base` overlay with [`FaceOverlay::of`] because it is the one that publishes the projection.
    ///
    /// # Errors
    /// `Missing` if no head part built before this one.
    pub fn face_overlay(&self, layer: FaceLayerId) -> Result<FaceOverlay, BuildError> {
        let head = self.head()?;
        let projection = (*self.published.get::<FaceFact>())
            .ok_or_else(|| BuildError::Missing { part: self.part.clone(), needs: "a face projection" })?;
        // The projection is in the painter's head-local units; the head centre is in metres at the character's real
        // size, so the window is scaled to match (the same scaling `scale_output` applies to the head's own overlay).
        Ok(FaceOverlay::of(&scaled_projection(&projection, self.published.scale()), head.centre(), layer))
    }

    /// Where `joint` is in the rig so far ([`Published::joint`]): the position the skeleton will give it unless a part
    /// built later moves it again. `ctx.fit()?.joint(..)` is the *undropped* body joint, because it asks only the
    /// outermost envelope; use this to place bones or geometry that must agree with the rig.
    pub fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        self.published.joint(joint)
    }

    /// The collider groups earlier parts published, to point a spring chain at by name or to keep geometry clear of.
    pub fn colliders(&self) -> &[ColliderGroup] {
        self.published.get::<CollidersFact>()
    }

    /// The outermost fit garments wrap ([`Published::outer_fit`]), or [`BuildError::Missing`]. A shared handle, like [`BuildCtx::head`].
    ///
    /// # Errors
    /// `Missing` if no body part built before this one.
    pub fn fit(&self) -> Result<Arc<dyn BodyFit>, BuildError> {
        self.published
            .outer_fit()
            .cloned()
            .ok_or_else(|| BuildError::Missing { part: self.part.clone(), needs: "a body to fit over" })
    }
}

/// What a built part hands back.
#[derive(Default)]
pub struct PartOutput {
    /// Drawable meshes.
    pub meshes: Vec<PartMesh>,
    /// Materials the meshes use, by id.
    pub materials: Vec<(MaterialName, MaterialSpec)>,
    /// The class of a material, for the look (LK-06), where inferring it would be wrong: metal and modelled eyes,
    /// or a body part's cloth. A material not listed takes [`MaterialClass::infer`](crate::MaterialClass::infer).
    /// Every name must be one of `materials`.
    pub material_classes: Vec<(MaterialName, crate::look::MaterialClass)>,
    /// Procedural textures the materials use.
    pub textures: Vec<(TextureName, TextureData)>,
    /// The simulation, for dynamic parts.
    pub runtime: Option<Box<dyn PartRuntime>>,
    /// What later parts may use.
    pub publishes: Publishes,
    /// What the part adds to the rig: extra bones, spring chains and colliders.
    pub rig: RigContribution,
    /// Non-fatal notes for the report card (a clump that clips, a texture that was downsized).
    pub warnings: Vec<String>,
}

impl Debug for PartOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartOutput")
            .field("meshes", &self.meshes.len())
            .field("materials", &self.materials.len())
            .field("material_classes", &self.material_classes)
            .field("textures", &self.textures.len())
            .field("runtime", &self.runtime.is_some())
            .field("publishes", &self.publishes)
            .field("rig", &self.rig)
            .field("warnings", &self.warnings)
            .finish()
    }
}

impl PartOutput {
    /// A copy of this output: the meshes, materials, textures, publishes, rig and warnings are cloned (the surfaces and
    /// fits in `publishes` are shared read-only handles), and the simulation is [forked](PartRuntime::fork), so the copy
    /// steps on its own, starting in the generator's initial state. A caching builder keeps one fork of a part and serves
    /// a fresh fork of it for every build.
    ///
    /// `None` if the part has a simulation that cannot be forked (the trait's default): the part then has to be built
    /// again, so a builder must not keep it.
    pub fn fork(&self) -> Option<PartOutput> {
        let runtime = match &self.runtime {
            Some(runtime) => Some(runtime.fork()?),
            None => None,
        };
        Some(PartOutput {
            meshes: self.meshes.clone(),
            materials: self.materials.clone(),
            material_classes: self.material_classes.clone(),
            textures: self.textures.clone(),
            runtime,
            publishes: self.publishes.clone(),
            rig: self.rig.clone(),
            warnings: self.warnings.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{DynamicMesh, PoseSource, RuntimeCost, StepCtx};
    use omuzaki_core::glam::Mat4;
    use omuzaki_core::time::Seconds;

    /// A pose that never moves: the simulation here ignores it.
    struct Rest;

    impl PoseSource for Rest {
        fn skin_matrix(&self, _: &JointRef) -> Option<Mat4> {
            Some(Mat4::IDENTITY)
        }

        fn root(&self) -> Mat4 {
            Mat4::IDENTITY
        }
    }

    /// A simulation that counts its steps, so a test can tell an original from its fork. A fork is in the initial state,
    /// with no steps. With `copyable: false` it leaves `fork` at the trait's default.
    struct Counting {
        steps: u32,
        copyable: bool,
    }

    impl PartRuntime for Counting {
        fn step(&mut self, _: &StepCtx<'_>) {
            self.steps += 1;
        }

        fn reset(&mut self, _: &dyn PoseSource) {
            self.steps = 0;
        }

        fn dynamic_count(&self) -> usize {
            0
        }

        fn dynamic_mesh(&self, _: usize) -> Option<DynamicMesh<'_>> {
            None
        }

        fn cost(&self) -> RuntimeCost {
            // The only state the trait exposes: the test reads the step count back through `particles`.
            RuntimeCost { particles: self.steps, colliders: 0, vertices: 0 }
        }

        fn fork(&self) -> Option<Box<dyn PartRuntime>> {
            self.copyable.then(|| Box::new(Self { steps: 0, copyable: true }) as Box<dyn PartRuntime>)
        }
    }

    fn step_once(runtime: &mut dyn PartRuntime) {
        runtime.step(&StepCtx { dt: 1.0 / 60.0, time: Seconds(0.0), pose: &Rest, wind: Vec3::ZERO });
    }

    fn steps(runtime: Option<&dyn PartRuntime>) -> Option<u32> {
        runtime.map(|r| r.cost().particles)
    }

    #[test]
    fn a_fork_copies_the_pieces_and_forks_the_simulation() {
        let mut output = PartOutput {
            warnings: vec!["a clump clips the shoulder".to_owned()],
            runtime: Some(Box::new(Counting { steps: 0, copyable: true })),
            ..PartOutput::default()
        };
        step_once(output.runtime.as_deref_mut().expect("the output has a simulation"));

        let mut fork = output.fork().expect("a counting simulation can be forked");
        assert_eq!(fork.warnings, output.warnings, "the pieces are copied");
        assert_eq!(steps(output.runtime.as_deref()), Some(1));
        assert_eq!(
            steps(fork.runtime.as_deref()),
            Some(0),
            "forked after a step: the initial state, not the current one"
        );

        step_once(fork.runtime.as_deref_mut().expect("the fork has a simulation"));
        assert_eq!(steps(fork.runtime.as_deref()), Some(1), "the fork steps on its own");
        assert_eq!(steps(output.runtime.as_deref()), Some(1), "and the original does not move with it");
    }

    #[test]
    fn a_simulation_that_cannot_be_forked_makes_the_output_uncopyable() {
        let output =
            PartOutput { runtime: Some(Box::new(Counting { steps: 0, copyable: false })), ..PartOutput::default() };
        assert!(output.fork().is_none(), "the trait's default fork is None, so the output is not copyable");
        assert!(PartOutput::default().fork().is_some(), "an output without a simulation always copies");
    }
}
