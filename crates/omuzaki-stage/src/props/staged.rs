//! A scene's props on the stage (design D10, #932): resolved off the render thread, uploaded once per build, drawn
//! every frame.
//!
//! - **Resolve** ([`ResolvedProps::resolve_into`], cold, beside the rest of the scene): each prop's kind builds its
//!   meshes, materials and lights from its params. A build is kept while its kind and params stay the same, so moving a
//!   prop rebuilds nothing and a param edit rebuilds that one prop. Each prop gets a [`Placement`]: its world transform,
//!   or, for a prop attached to a bone, its transform in the bone's frame. The lights of the visible, unattached props
//!   are placed in the world and appended to the rig's, within the render limit; the extra ones are dropped in id
//!   order and counted.
//! - **Pay** ([`StageProps::set`], cold): the builds the renderer doesn't have yet are uploaded and the ones the scene
//!   no longer uses are released. A build the renderer already has (the same [`Arc`]) stays as it is, so a param edit
//!   uploads exactly one prop.
//! - **Fill** ([`StageProps::push`], hot): one instance per part of each visible prop, at its placement or its bone.
//!   Allocation-free once [`StageProps::set`] reserved the instances.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_anim_api::HumanoidBone;
use omuzaki_core::glam::{Mat4, Vec2, Vec3};
use omuzaki_render_api::{
    BackdropFit, Instance, Light, Material, MaterialId, MeshId, Renderer, Scene, TextureDesc, TextureId, UvTransform,
};
use omuzaki_stage_api::resolve::LightBuf;
use omuzaki_stage_api::{
    Anchor, MemberId, PartImage, PropDef, PropId, PropKindId, PropOutput, PropPart, SceneDef, find_prop_kind,
};
use omuzaki_store_api::{ContentHash, Store};

use crate::error::StageError;
use crate::scene::backdrop::ImagePixels;
use crate::scene::resolve::Figure;

/// Numbers every set of builds, so a stage can tell which one the renderer has. 0 is the empty set a stage starts with.
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// One prop as its kind built it.
#[derive(Clone, Debug, PartialEq)]
pub struct PropBuild {
    /// The prop.
    pub id: PropId,
    /// The kind that built it.
    pub kind: PropKindId,
    /// The params it was built from, as stored.
    pub params: omuzaki_doc_api::Value,
    /// What the kind built: `None` for a kind nothing registered (a finding: the prop is skipped) or a build the kind
    /// refused (logged).
    pub output: Option<Arc<PropOutput>>,
}

/// Every prop of a scene as built, by id. Shared between the resolve, the stage and its debt, so handing it on costs a
/// reference count.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PropBuilds {
    /// Names this set: a new set gets a new number.
    pub generation: u64,
    /// The builds by id, in the scene's order.
    pub props: Vec<PropBuild>,
    /// The members other than the subject that props are attached to, for [`AttachTo::Member`].
    pub members: Vec<MemberId>,
    /// The pixels of the images the builds show ([`PartImage`]), once loaded ([`ResolvedProps::load_images`]); a
    /// rebuild keeps those still shown.
    pub images: BTreeMap<ContentHash, ImagePixels>,
}

/// Whose bone an attached prop follows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AttachTo {
    /// The scene's subject: the body the stage itself runs.
    Subject,
    /// Another member: the index of its id in [`PropBuilds::members`]. A [`CastStage`](crate::cast::CastStage) places
    /// these.
    Member(u8),
}

/// Where one prop is drawn this frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Placement {
    /// Model to world for an unattached prop; model to the bone's frame for an attached one.
    pub transform: Mat4,
    /// The bone it follows, and whose.
    pub attach: Option<(AttachTo, HumanoidBone)>,
    /// Drawn at all: the prop is visible and its anchor member is on the stage.
    pub visible: bool,
    /// Its parts shadow the cast and the other props.
    pub casts_shadow: bool,
}

/// A scene's props, resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedProps {
    /// The builds, by id.
    pub builds: Arc<PropBuilds>,
    /// One placement per build, in the same order. Capacity [`MAX_PROPS`](omuzaki_stage_api::MAX_PROPS).
    pub placements: Vec<Placement>,
    /// How many of the props' lights didn't fit beside the rig's and were dropped (the last ones by prop id).
    pub dropped_lights: usize,
}

impl Default for ResolvedProps {
    fn default() -> Self {
        Self {
            builds: Arc::new(PropBuilds::default()),
            placements: Vec::with_capacity(omuzaki_stage_api::MAX_PROPS),
            dropped_lights: 0,
        }
    }
}

/// What a props resolve changed.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PropChanges {
    /// A prop was added, removed or rebuilt: the stage uploads (cold).
    pub builds: bool,
    /// A placement changed: values only.
    pub placements: bool,
}

impl ResolvedProps {
    /// Resolves `def`'s props into `self`, against the visible `figures` and the scene's `subject`, and appends the
    /// lights of the visible, unattached ones to `lights`. Rebuilds only props whose kind or params changed; allocates
    /// nothing when the props didn't change.
    pub fn resolve_into(
        &mut self,
        def: &SceneDef,
        figures: &BTreeMap<MemberId, Figure>,
        subject: Option<&MemberId>,
        lights: &mut LightBuf,
    ) -> PropChanges {
        let mut changes = PropChanges::default();
        if !same_builds(&self.builds, def, subject) {
            self.builds = Arc::new(rebuild(&self.builds, def, subject));
            changes.builds = true;
        }
        let before = self.placements.len();
        self.placements.truncate(def.props.len());
        for (i, prop) in def.props.values().enumerate() {
            let placement = place(prop, figures, subject, &self.builds.members);
            match self.placements.get_mut(i) {
                Some(slot) if *slot == placement => {}
                Some(slot) => {
                    *slot = placement;
                    changes.placements = true;
                }
                None => {
                    self.placements.push(placement);
                    changes.placements = true;
                }
            }
        }
        changes.placements |= before != self.placements.len();
        let mut dropped = 0;
        for (placement, build) in self.placements.iter().zip(&self.builds.props) {
            let Some(output) = build.output.as_deref() else { continue };
            if !placement.visible || placement.attach.is_some() {
                continue;
            }
            for light in &output.lights {
                let placed = place_light(light, &placement.transform);
                // A light pushed out to a non-finite place by a huge position is not drawn rather than failing the scene.
                if light_finite(&placed) && !lights.try_push(placed) {
                    dropped += 1;
                }
            }
        }
        if dropped != self.dropped_lights {
            if dropped > 0 {
                tracing::warn!(dropped, "the props' lights don't all fit beside the rig's; the last ones are dropped");
            }
            self.dropped_lights = dropped;
        }
        changes
    }
}

impl ResolvedProps {
    /// Loads from `store` the pixels of every image the builds show and don't have yet. Cold, off the render thread: it
    /// reads and decodes files. An image the store lacks or can't decode is logged and left out, and its piece is drawn
    /// without it: a picture keeps its painted canvas.
    pub fn load_images(&mut self, store: &dyn Store) {
        let mut wanted: Vec<ContentHash> = parts(&self.builds.props).filter_map(|p| p.image.map(|i| i.asset)).collect();
        wanted.sort_unstable();
        wanted.dedup();
        wanted.retain(|asset| !self.builds.images.contains_key(asset));
        let mut loaded = Vec::new();
        for asset in wanted {
            let pixels = store
                .get(&asset)
                .map_err(|e| e.to_string())
                .and_then(|bytes| ImagePixels::decode_for("props.params.image", asset, &bytes).map_err(|e| e.to_string()));
            match pixels {
                Ok(pixels) => loaded.push((asset, pixels)),
                Err(error) => tracing::warn!(%asset, %error, "a prop's image can't be loaded; its piece is drawn without it"),
            }
        }
        if !loaded.is_empty() {
            let builds = Arc::make_mut(&mut self.builds);
            builds.images.extend(loaded);
            // New pixels are a new set: the stage uploads the props that show them again.
            builds.generation = GENERATION.fetch_add(1, Ordering::Relaxed);
            for b in &mut builds.props {
                if b.output.as_ref().is_some_and(|o| o.parts.iter().any(|p| p.image.is_some())) {
                    b.output = b.output.as_deref().cloned().map(Arc::new);
                }
            }
        }
    }
}

/// Every part of every build.
fn parts(props: &[PropBuild]) -> impl Iterator<Item = &PropPart> {
    props.iter().filter_map(|b| b.output.as_deref()).flat_map(|o| o.parts.iter())
}

/// Whether any build shows `asset`.
fn shows(props: &[PropBuild], asset: &ContentHash) -> bool {
    parts(props).any(|p| p.image.is_some_and(|i| i.asset == *asset))
}

/// Whether `builds` was built from exactly `def`'s props (ids, kinds and params) and attach members. Allocation-free.
fn same_builds(builds: &PropBuilds, def: &SceneDef, subject: Option<&MemberId>) -> bool {
    builds.props.len() == def.props.len()
        && builds.props.iter().zip(&def.props).all(|(b, (id, p))| b.id == *id && b.kind == p.kind && b.params == p.params)
        && def.props.values().filter_map(|p| other_member(p, subject)).all(|m| builds.members.contains(m))
        && builds.members.iter().all(|m| def.props.values().any(|p| other_member(p, subject) == Some(m)))
}

/// The member other than the subject that `prop` follows a bone of.
fn other_member<'a>(prop: &'a PropDef, subject: Option<&MemberId>) -> Option<&'a MemberId> {
    prop.attach?;
    prop.anchor.member().filter(|m| Some(*m) != subject)
}

/// Builds `def`'s props, keeping every build of `old` whose kind and params are the same. Cold.
fn rebuild(old: &PropBuilds, def: &SceneDef, subject: Option<&MemberId>) -> PropBuilds {
    let props: Vec<PropBuild> = def
        .props
        .iter()
        .map(|(id, prop)| {
            // Kept by id as well, so no two props share a build and each has its own upload.
            let kept = old.props.iter().find(|b| b.id == *id && b.kind == prop.kind && b.params == prop.params);
            let output = match kept {
                Some(b) => b.output.clone(),
                None => build(id, prop),
            };
            PropBuild { id: id.clone(), kind: prop.kind.clone(), params: prop.params.clone(), output }
        })
        .collect();
    let mut members: Vec<MemberId> = def.props.values().filter_map(|p| other_member(p, subject)).cloned().collect();
    members.sort();
    members.dedup();
    members.truncate(usize::from(u8::MAX));
    let images = old.images.iter().filter(|(asset, _)| shows(&props, asset)).map(|(a, p)| (*a, p.clone())).collect();
    PropBuilds { generation: GENERATION.fetch_add(1, Ordering::Relaxed), props, members, images }
}

/// One prop's build, or `None` (logged) for a kind nothing registered or params it refuses.
fn build(id: &PropId, prop: &PropDef) -> Option<Arc<PropOutput>> {
    let kind = find_prop_kind(prop.kind.as_str())?;
    match kind.build(&prop.params) {
        Ok(output) => Some(Arc::new(output)),
        Err(error) => {
            tracing::warn!(prop = %id, %error, "a prop's kind refused its params; it isn't drawn");
            None
        }
    }
}

/// Where `prop` is drawn: its anchor's root times its own transform, or its transform under a bone.
fn place(
    prop: &PropDef,
    figures: &BTreeMap<MemberId, Figure>,
    subject: Option<&MemberId>,
    members: &[MemberId],
) -> Placement {
    let local = prop.local();
    let anchor_member = match &prop.anchor {
        Anchor::Stage => None,
        Anchor::Subject => subject,
        Anchor::Member(id) => Some(id),
    };
    // A member anchor needs that member on the stage; the subject anchor of an empty cast is the origin (D4).
    let figure = anchor_member.and_then(|id| figures.get(id));
    let present = anchor_member.is_none() || figure.is_some();
    let attach = prop.attach.and_then(|bone| {
        let to = match anchor_member {
            Some(id) if Some(id) != subject => AttachTo::Member(u8::try_from(members.iter().position(|m| m == id)?).ok()?),
            _ => AttachTo::Subject,
        };
        Some((to, bone))
    });
    let transform = match (attach, figure) {
        (None, Some(figure)) => Mat4::from(figure.root) * local,
        _ => local,
    };
    Placement { transform, attach, visible: prop.visible && present, casts_shadow: prop.casts_shadow }
}

/// `light`, built in the prop's frame, in the world: positions and directions through `transform`, ranges by its scale
/// and intensities by the scale squared (a lamp scaled up lights the same at the same relative distance).
fn place_light(light: &Light, transform: &Mat4) -> Light {
    let scale = transform.transform_vector3(Vec3::X).length();
    match *light {
        Light::Point { position, colour, intensity, range } => Light::Point {
            position: transform.transform_point3(position),
            colour,
            intensity: intensity * scale * scale,
            range: range * scale,
        },
        Light::Spot { position, direction, colour, intensity, range, cone_outer_deg, penumbra, casts_shadow } => {
            Light::Spot {
                position: transform.transform_point3(position),
                direction: transform.transform_vector3(direction).normalize_or(direction),
                colour,
                intensity: intensity * scale * scale,
                range: range * scale,
                cone_outer_deg,
                penumbra,
                casts_shadow,
            }
        }
        // A directional or hemisphere light has no place to move.
        other => other,
    }
}

/// Whether every number of a placed light is finite.
fn light_finite(light: &Light) -> bool {
    match *light {
        Light::Point { position, intensity, range, .. } => position.is_finite() && intensity.is_finite() && range.is_finite(),
        Light::Spot { position, direction, intensity, range, .. } => {
            position.is_finite() && direction.is_finite() && intensity.is_finite() && range.is_finite()
        }
        _ => true,
    }
}

/// One prop on the renderer.
#[derive(Debug)]
struct Uploaded {
    id: PropId,
    build: Arc<PropOutput>,
    /// Each part's mesh, material and whether it casts a shadow.
    parts: Vec<(MeshId, MaterialId, bool)>,
    /// The textures of the images its parts show.
    textures: Vec<TextureId>,
}

/// The props of a scene on a renderer: what the stage drew from, and what it draws each frame. Both stages hold one.
#[derive(Debug)]
pub(crate) struct StageProps {
    /// What the renderer has, by id.
    uploaded: Vec<Uploaded>,
    /// The latest builds and placements the stage was given ([`take`](Self::take)), which may be newer than the
    /// uploads until the next pay.
    builds: Arc<PropBuilds>,
    placements: Vec<Placement>,
}

impl Default for StageProps {
    fn default() -> Self {
        Self {
            uploaded: Vec::new(),
            builds: Arc::new(PropBuilds::default()),
            placements: Vec::with_capacity(omuzaki_stage_api::MAX_PROPS),
        }
    }
}

impl StageProps {
    /// Takes the latest resolved props for the next frames. Allocation-free (a reference count and a copy into
    /// reserved room).
    pub(crate) fn take(&mut self, props: &ResolvedProps) {
        if !Arc::ptr_eq(&self.builds, &props.builds) {
            self.builds = Arc::clone(&props.builds);
        }
        self.placements.clear();
        self.placements.extend_from_slice(&props.placements);
    }

    /// Makes the renderer hold `builds`: uploads the builds it doesn't have, keeps the ones it has (the same build)
    /// and releases the rest, then makes room in `scene` for every part. Cold. On an error nothing changes: what was
    /// uploaded by this call is released and the old props stay.
    ///
    /// # Errors
    /// [`StageError::Render`] with the first mesh or material the renderer refused.
    pub(crate) fn set(
        &mut self,
        renderer: &mut dyn Renderer,
        builds: &PropBuilds,
        scene: &mut Scene,
    ) -> Result<(), StageError> {
        let mut fresh: Vec<Uploaded> = Vec::new();
        for b in &builds.props {
            let Some(build) = &b.output else { continue };
            if self.uploaded.iter().any(|u| Arc::ptr_eq(&u.build, build)) {
                continue;
            }
            match upload(renderer, b.id.clone(), build, &builds.images) {
                Ok(up) => fresh.push(up),
                Err(error) => {
                    for up in fresh {
                        release_one(renderer, up);
                    }
                    return Err(error);
                }
            }
        }
        let mut old = std::mem::take(&mut self.uploaded);
        let mut next = Vec::with_capacity(builds.props.len());
        for b in &builds.props {
            let Some(build) = &b.output else { continue };
            let found = old.iter().position(|u| Arc::ptr_eq(&u.build, build)).map(|i| old.swap_remove(i));
            let found = found.or_else(|| {
                fresh.iter().position(|u| Arc::ptr_eq(&u.build, build)).map(|i| fresh.swap_remove(i))
            });
            next.extend(found);
        }
        for up in old.into_iter().chain(fresh) {
            release_one(renderer, up);
        }
        let parts: usize = next.iter().map(|u| u.parts.len()).sum();
        scene.instances.reserve(parts);
        self.uploaded = next;
        Ok(())
    }

    /// Adds an instance for every part of each visible prop the subject's stage draws: the unattached ones at their
    /// placement, the ones attached to the subject under `bone(bone)` (skipped while it is `None`). Hot: allocation-free
    /// after [`set`](Self::set).
    pub(crate) fn push(&self, scene: &mut Scene, bone: &dyn Fn(HumanoidBone) -> Option<Mat4>) {
        self.push_where(scene, |attach| match attach {
            None => Some(Mat4::IDENTITY),
            Some((AttachTo::Subject, b)) => bone(b),
            Some((AttachTo::Member(_), _)) => None,
        });
    }

    /// Adds an instance for every part of each visible prop attached to cast member `member`, under `bone(bone)`.
    /// Hot: allocation-free after [`set`](Self::set).
    pub(crate) fn push_member(
        &self,
        scene: &mut Scene,
        member: &MemberId,
        bone: &dyn Fn(HumanoidBone) -> Option<Mat4>,
    ) {
        let Some(slot) = self.builds.members.iter().position(|m| m == member) else { return };
        self.push_where(scene, |attach| match attach {
            Some((AttachTo::Member(m), b)) if usize::from(m) == slot => bone(b),
            _ => None,
        });
    }

    /// The instances of every visible prop for which `frame` gives the matrix its placement goes under.
    fn push_where(&self, scene: &mut Scene, frame: impl Fn(Option<(AttachTo, HumanoidBone)>) -> Option<Mat4>) {
        // The uploads and the builds are both in id order: a merge finds each prop's parts without a search.
        let mut uploads = self.uploaded.iter().peekable();
        for (placement, build) in self.placements.iter().zip(&self.builds.props) {
            while uploads.next_if(|u| u.id < build.id).is_some() {}
            let Some(up) = uploads.next_if(|u| u.id == build.id) else { continue };
            if !placement.visible {
                continue;
            }
            let Some(under) = frame(placement.attach) else { continue };
            let transform = under * placement.transform;
            for &(mesh, material, shadow) in &up.parts {
                let mut instance = Instance::new(mesh, material, transform);
                instance.casts_shadow = shadow && placement.casts_shadow;
                scene.instances.push(instance);
            }
        }
    }

    /// Gives every prop back to `renderer`.
    ///
    /// # Errors
    /// [`StageError::Render`] with the first release the renderer refused.
    pub(crate) fn release(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        let mut first = Ok(());
        for up in std::mem::take(&mut self.uploaded) {
            for (mesh, material, _) in up.parts {
                let released = renderer.release(mesh.into()).and(renderer.release(material.into()));
                if let (Ok(()), Err(source)) = (&first, released) {
                    first = Err(StageError::render("a prop", source));
                }
            }
            for texture in up.textures {
                if let (Ok(()), Err(source)) = (&first, renderer.release(texture.into())) {
                    first = Err(StageError::render("a prop's image", source));
                }
            }
        }
        first
    }
}

/// Uploads every part of one build, a part that shows an image with its texture (when `images` has its pixels); on an
/// error the parts made so far are released.
fn upload(
    renderer: &mut dyn Renderer,
    id: PropId,
    build: &Arc<PropOutput>,
    images: &BTreeMap<ContentHash, ImagePixels>,
) -> Result<Uploaded, StageError> {
    let mut up = Uploaded { id, build: Arc::clone(build), parts: Vec::with_capacity(build.parts.len()), textures: Vec::new() };
    for part in &build.parts {
        let shown = part.image.and_then(|image| images.get(&image.asset).map(|pixels| (image, pixels)));
        let made = match shown {
            Some((image, pixels)) => upload_shown(renderer, part, image, pixels, &mut up.textures),
            None => upload_part(renderer, &part.mesh, &part.material),
        };
        match made {
            Ok((mesh, material)) => up.parts.push((mesh, material, part.casts_shadow)),
            Err(source) => {
                release_one(renderer, up);
                return Err(StageError::render("a prop", source));
            }
        }
    }
    Ok(up)
}

/// Uploads one mesh and its material; on an error the mesh is released.
fn upload_part(
    renderer: &mut dyn Renderer,
    mesh: &omuzaki_core::mesh::MeshData,
    material: &Material,
) -> Result<(MeshId, MaterialId), omuzaki_render_api::RenderError> {
    let mesh = renderer.create_mesh(mesh)?;
    match renderer.create_material(material) {
        Ok(material) => Ok((mesh, material)),
        Err(error) => {
            if let Err(cleanup) = renderer.release(mesh.into()) {
                tracing::warn!(%cleanup, "releasing a prop mesh whose material failed failed too");
            }
            Err(error)
        }
    }
}

/// Uploads a part that shows `image`: its texture (kept in `textures` for the release), its material reading it, and
/// its mesh, cropped to fill the piece (`cover`) or shrunk round its centre to the image's shape (`contain`).
fn upload_shown(
    renderer: &mut dyn Renderer,
    part: &PropPart,
    image: PartImage,
    pixels: &ImagePixels,
    textures: &mut Vec<TextureId>,
) -> Result<(MeshId, MaterialId), omuzaki_render_api::RenderError> {
    let texture = renderer.create_texture(&TextureDesc::colour(pixels.size()), Some(pixels.rgba()))?;
    textures.push(texture);
    let mut material = part.material.clone();
    material.base_texture = Some(texture);
    let (uv, shrink) = fit_image(image.fit, image.aspect, pixels.aspect());
    material.uv_transform = uv;
    if shrink == Vec2::ONE {
        return upload_part(renderer, &part.mesh, &material);
    }
    let mut mesh = part.mesh.clone();
    let (lo, hi) = mesh.positions.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    let centre = (lo + hi) * 0.5;
    for p in &mut mesh.positions {
        p.x = centre.x + (p.x - centre.x) * shrink.x;
        p.y = centre.y + (p.y - centre.y) * shrink.y;
    }
    upload_part(renderer, &mesh, &material)
}

/// How an image of aspect `texture` meets a piece of aspect `piece` (both width over height): the UV transform that
/// crops it for `cover`, or how much the piece shrinks across and up for `contain`.
fn fit_image(fit: BackdropFit, piece: f32, texture: f32) -> (UvTransform, Vec2) {
    if !(piece > 0.0 && texture > 0.0 && piece.is_finite() && texture.is_finite()) {
        return (UvTransform::IDENTITY, Vec2::ONE);
    }
    let wider = texture > piece;
    match fit {
        BackdropFit::Cover => {
            let scale = if wider { Vec2::new(piece / texture, 1.0) } else { Vec2::new(1.0, texture / piece) };
            (UvTransform { scale, offset: (Vec2::ONE - scale) * 0.5 }, Vec2::ONE)
        }
        BackdropFit::Contain => {
            let shrink = if wider { Vec2::new(1.0, piece / texture) } else { Vec2::new(texture / piece, 1.0) };
            (UvTransform::IDENTITY, shrink)
        }
    }
}

/// Releases one prop's parts, logging what the renderer refuses (a cleanup path).
fn release_one(renderer: &mut dyn Renderer, up: Uploaded) {
    for (mesh, material, _) in up.parts {
        if let Err(error) = renderer.release(mesh.into()).and(renderer.release(material.into())) {
            tracing::warn!(%error, prop = %up.id, "releasing a prop failed");
        }
    }
    for texture in up.textures {
        if let Err(error) = renderer.release(texture.into()) {
            tracing::warn!(%error, prop = %up.id, "releasing a prop's image failed");
        }
    }
}
