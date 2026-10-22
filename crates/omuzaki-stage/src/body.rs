//! A character from the document on a stage, whichever way it was made (`docs/design/byom.md` D1): a kit character is
//! built from its parts, an imported one is rebuilt from its stored original, and both go on the one frame loop as an
//! [`AnyStage`]. The studio, the CLI and the outputs load a character through [`load_body`] and never branch on where
//! it came from.

use omuzaki_anim_api::HumanoidPose;
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, LookDef, MaterialName, Quality as BuildQuality,
};
use omuzaki_char_api::{VariantId, paths as char_paths};
use omuzaki_core::time::Clock;
use omuzaki_doc_api::DocRead;
use omuzaki_io_ops::ops::avatar::{is_imported, load_avatar};
use omuzaki_ops_api::OpError;
use omuzaki_render_api::{MtoonSupport, Quality, Renderer, Scene, TargetId};
use omuzaki_store_api::Store;

use crate::avatar::AvatarStage;
use crate::error::StageError;
use crate::frame::{Stage, StageClock};
use crate::ops::render::job::read_definition;
use crate::scene::landmarks::Landmarks;
use crate::stages::AnyStage;
use crate::upload::UploadOptions;

/// A character ready to stage: built from the kit's parts, or imported.
#[derive(Debug)]
pub enum Body {
    /// A kit character, built.
    Kit(Box<BuiltCharacter>),
    /// An imported avatar, prepared.
    Avatar(Box<ImportedAvatar>),
}

impl Body {
    /// The pose a performer layers onto: the bind the body's meshes were made in (decision 0014 §4), with an imported
    /// avatar's T-pose arms lowered to hang at ease ([`AvatarStage::base_pose`](crate::avatar::AvatarStage::base_pose)).
    #[must_use]
    pub fn base_pose(&self) -> HumanoidPose {
        match self {
            Self::Kit(built) => built.rig.bind,
            Self::Avatar(avatar) => crate::avatar::ease::at_ease(&avatar.bind),
        }
    }

    /// What framing measures on the body in its bind pose.
    ///
    /// # Errors
    /// [`StageError::Framing`] for a body framing can't measure (see [`Landmarks::measure`]).
    pub fn landmarks(&self) -> Result<Landmarks, StageError> {
        match self {
            Self::Kit(built) => Landmarks::measure(built),
            Self::Avatar(avatar) => Landmarks::measure_avatar(avatar),
        }
    }

    /// Puts the body on `renderer` with its frame loop. `upload` applies to a kit character only (an imported avatar
    /// brings its textures at their own size). Cold: uploads everything.
    ///
    /// # Errors
    /// What [`Stage::new`] or [`AvatarStage::new`] reports; nothing is left on the renderer.
    pub fn stage(
        self,
        renderer: &mut dyn Renderer,
        upload: UploadOptions,
        scene: Scene,
        target: TargetId,
        quality: Quality,
        clock: StageClock,
    ) -> Result<AnyStage, StageError> {
        Ok(match self {
            Self::Kit(built) => Stage::new(renderer, *built, upload, scene, target, quality, clock)?.into(),
            Self::Avatar(avatar) => AvatarStage::new(renderer, *avatar, scene, target, quality, clock)?.into(),
        })
    }
}

/// What loading a character needs besides the document: the kit's builder for a built character, the store and the
/// renderer's `MToon` support for an imported one.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct BodyServices<'a> {
    /// Builds a kit character from its definition.
    pub builder: &'a dyn CharacterBuilder,
    /// What the builder builds with.
    pub build: &'a BuildServices,
    /// Holds an imported character's original.
    pub store: &'a dyn Store,
    /// The `MToon` groups the renderer draws (`Capabilities::mtoon`).
    pub mtoon: MtoonSupport,
    /// Times the import's stages.
    pub clock: &'a dyn Clock,
}

impl<'a> BodyServices<'a> {
    /// The services, from their parts.
    #[must_use]
    pub fn new(
        builder: &'a dyn CharacterBuilder,
        build: &'a BuildServices,
        store: &'a dyn Store,
        mtoon: MtoonSupport,
        clock: &'a dyn Clock,
    ) -> Self {
        Self { builder, build, store, mtoon, clock }
    }
}

impl std::fmt::Debug for BodyServices<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyServices").field("mtoon", &self.mtoon).finish_non_exhaustive()
    }
}

/// Character `id` of `doc`, ready to stage: an imported character's avatar rebuilt from its original, a kit
/// character built at `quality` with `variant` applied. A variant is a kit idea; asking one of an imported character
/// is an error.
///
/// # Errors
/// `NotFound` for no such character, variant or stored original; `Invalid` for a character that doesn't read, build
/// or prepare, or a variant asked of an imported one.
pub fn load_body(
    doc: &dyn DocRead,
    services: &BodyServices<'_>,
    id: &CharId,
    variant: Option<&VariantId>,
    quality: BuildQuality,
) -> Result<Body, OpError> {
    if is_imported(doc, id) {
        if variant.is_some() {
            return Err(OpError::invalid("variant", format!("'{id}' is imported, and an imported character has no variants"))
                .hint("leave out the variant"));
        }
        let avatar = load_avatar(doc, services.store, id, services.mtoon, services.clock)?;
        return Ok(Body::Avatar(Box::new(avatar)));
    }
    let def = read_definition(doc, id, variant)?;
    let options = BuildOptions { variant: variant.cloned(), quality, rig_export: false };
    let mut built = services
        .builder
        .build(id, &def, &options, services.build)
        .map_err(|e| OpError::invalid("character", format!("character '{id}' doesn't build: {e}")))?;
    draw_in_look(doc, id, &mut built)?;
    Ok(Body::Kit(Box::new(built)))
}

/// Draws `built` in the look stored at `characters/<id>/look` (LK-06, #714): its materials take the look's class
/// settings, and [`BuiltCharacter::look`] keeps it for the frame's post ([`post_in_look`](crate::scene::alone::post_in_look)).
/// No look node leaves the parts' own materials. Returns the materials that changed, for a stage that has them on the
/// renderer already.
///
/// # Errors
/// `Invalid` (field `look`) for a look node that doesn't read or resolve, or that the materials can't take.
pub fn draw_in_look(doc: &dyn DocRead, id: &CharId, built: &mut BuiltCharacter) -> Result<Vec<MaterialName>, OpError> {
    let Some(node) = doc.get(&char_paths::look(id)) else { return Ok(Vec::new()) };
    let refused = |e: &dyn std::fmt::Display| {
        OpError::invalid("look", format!("the look of '{id}' {e}")).hint("set it again with look.set_preset or look.set_param")
    };
    let look = LookDef::from_node(Some(node))
        .and_then(|def| def.resolve())
        .map_err(|e| refused(&format!("doesn't resolve: {e}")))?;
    built.apply_look(&look).map_err(|e| refused(&format!("can't be drawn: {e}")))
}
