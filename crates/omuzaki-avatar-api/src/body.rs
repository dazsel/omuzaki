//! [`Body`] and [`BodyView`]: the one performer surface (design D1, condition 1).
//!
//! The stage, recording, export, the tracking doctor and agents see a performer only through a [`BodyView`], whether
//! it is a built character or an imported avatar, so nothing downstream branches on where a body came from. A built
//! character has no morphs, no node constraints and no expressions; an imported avatar has no face binding, no palette
//! and no part runtimes of its own. The view says so with empty or absent fields, never with a kind to match on.
//!
//! The view lives here, not in the stage, because `omuzaki-io-ops` (export) and the tracking doctor read it and may not
//! depend on an App crate.

use std::collections::BTreeMap;

use omuzaki_anim_api::{ConstraintSet, HumanoidPose, Skeleton, SpringSet};
use omuzaki_char_api::{
    Anchor, AnchorId, BoneDriver, BuiltCharacter, FaceBinding, MaterialName, MaterialSpec, PartId, PartMesh,
    RuntimeSlot, Stature, TextureData, TextureName,
};
use omuzaki_core::glam::Mat4;
use omuzaki_face_api::Palette;
use omuzaki_render_api::MorphSet;

use crate::avatar::{AvatarExport, AvatarFace, AvatarMesh, ImportedAvatar};

/// A performer's body: built from parts by the kit, or imported from a file. Both are boxed: a body is large, long
/// lived and read through its view, so the indirection costs nothing that matters.
#[non_exhaustive]
pub enum Body {
    /// A procedural character, built from its `CharacterDef`.
    Built(Box<BuiltCharacter>),
    /// An imported avatar.
    Imported(Box<ImportedAvatar>),
}

impl Body {
    /// The body as the stage and every other reader see it.
    pub fn view(&self) -> BodyView<'_> {
        match self {
            Self::Built(built) => BodyView::built(built),
            Self::Imported(avatar) => BodyView::imported(avatar),
        }
    }
}

/// No node constraints: what a built character's view holds.
static NO_CONSTRAINTS: ConstraintSet = ConstraintSet { constraints: Vec::new() };

/// A borrowed, read-only view of a [`Body`].
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct BodyView<'a> {
    /// The meshes, in build or file order.
    pub meshes: Meshes<'a>,
    /// Materials by name.
    pub materials: &'a BTreeMap<MaterialName, MaterialSpec>,
    /// Textures by name.
    pub textures: &'a BTreeMap<TextureName, TextureData>,
    /// The skeleton every mesh is skinned to. Its rest is the VRM T-pose.
    pub skeleton: &'a Skeleton,
    /// The pose the meshes were made in, in the normalised space of [`HumanoidPose`]: the stage shows it when nothing
    /// animates the body.
    pub bind: &'a HumanoidPose,
    /// Spring chains and colliders.
    pub springs: &'a SpringSet,
    /// Node constraints; empty for a built character.
    pub constraints: &'a ConstraintSet,
    /// Extra bones posed by a channel or another bone; empty for an imported avatar.
    pub drivers: &'a [BoneDriver],
    /// Attachment points.
    pub anchors: Anchors<'a>,
    /// How the painted face attaches; `None` for an imported avatar and for a built character without a head.
    pub face: Option<&'a FaceBinding>,
    /// The palette the face profile's colour slots resolve in; `None` for an imported avatar.
    pub palette: Option<&'a Palette>,
    /// Part simulations (hair, cloth), in build order; empty for an imported avatar.
    pub runtimes: &'a [RuntimeSlot],
    /// The real-world stature the body was scaled to; `None` when it is authored in metres (every imported avatar).
    pub stature: Option<Stature>,
    /// What drives an imported face: expressions, drivers, gaze route and look-at, which the expression evaluator
    /// and the look-at stage read; `None` for a built character, whose face is painted ([`face`](Self::face)).
    pub expressions: Option<AvatarFace<'a>>,
    /// What export reads beyond the body (the original, first-person flags, the retained source data); `None` for a
    /// built character, which exports from its parts.
    pub export: Option<AvatarExport<'a>>,
}

impl<'a> BodyView<'a> {
    /// The view of a built character: exactly what the stage reads from it.
    pub fn built(built: &'a BuiltCharacter) -> Self {
        Self {
            meshes: Meshes(MeshSource::Built(&built.meshes)),
            materials: &built.materials,
            textures: &built.textures,
            skeleton: &built.rig.skeleton,
            bind: &built.rig.bind,
            springs: &built.rig.springs,
            constraints: &NO_CONSTRAINTS,
            drivers: &built.rig.drivers,
            anchors: Anchors(AnchorSource::ByPart(&built.anchors)),
            face: built.face.as_ref(),
            palette: Some(&built.palette),
            runtimes: &built.runtimes,
            stature: built.stature,
            expressions: None,
            export: None,
        }
    }

    /// The view of an imported avatar.
    pub fn imported(avatar: &'a ImportedAvatar) -> Self {
        Self {
            meshes: Meshes(MeshSource::Imported(&avatar.meshes)),
            materials: &avatar.materials,
            textures: &avatar.textures,
            skeleton: &avatar.skeleton,
            bind: &avatar.bind,
            springs: &avatar.springs,
            constraints: &avatar.constraints,
            drivers: &[],
            anchors: Anchors(AnchorSource::Own(&avatar.anchors)),
            face: None,
            palette: None,
            runtimes: &[],
            stature: None,
            expressions: Some(avatar.face()),
            export: Some(avatar.export()),
        }
    }
}

/// One mesh of a body.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct MeshView<'a> {
    /// Geometry, joint list, material, shadow flag and draw order.
    pub mesh: &'a PartMesh,
    /// The part that made it, for a built character; `None` for a mesh of an imported avatar.
    pub part: Option<&'a PartId>,
    /// Its index in that part's own mesh list (what a part runtime's `DynamicMesh` refers to); its index among the
    /// avatar's meshes for an imported one.
    pub local: u16,
    /// Inverse bind matrices per joint run, one per entry of `mesh.joint_bones`, when the mesh carries its own (every
    /// imported mesh). `None` means the skeleton's per-bone inverse binds in the body's bind pose (a built character).
    pub inverse_bind: Option<&'a [Mat4]>,
    /// Its morph targets; `None` for a mesh without any (every built mesh).
    pub morphs: Option<&'a MorphSet>,
}

/// Where a body's meshes are stored.
#[derive(Clone, Copy)]
enum MeshSource<'a> {
    Built(&'a [omuzaki_char_api::BuiltMesh]),
    Imported(&'a [AvatarMesh]),
}

/// The meshes of a body, read one [`MeshView`] at a time without allocating.
#[derive(Clone, Copy)]
pub struct Meshes<'a>(MeshSource<'a>);

impl<'a> Meshes<'a> {
    /// How many meshes there are.
    pub fn len(&self) -> usize {
        match self.0 {
            MeshSource::Built(m) => m.len(),
            MeshSource::Imported(m) => m.len(),
        }
    }

    /// True if there are none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The mesh at `index`.
    pub fn get(&self, index: usize) -> Option<MeshView<'a>> {
        match self.0 {
            MeshSource::Built(meshes) => meshes.get(index).map(|m| MeshView {
                mesh: &m.mesh,
                part: Some(&m.part),
                local: m.local,
                inverse_bind: None,
                morphs: None,
            }),
            MeshSource::Imported(meshes) => meshes.get(index).map(|m| MeshView {
                mesh: &m.mesh,
                part: None,
                // Validation keeps the count within `MAX_INDEXED`, so every index fits.
                local: u16::try_from(index).unwrap_or_else(|_| {
                    debug_assert!(false, "an imported avatar with more than MAX_INDEXED meshes was not validated");
                    u16::MAX
                }),
                inverse_bind: Some(&m.inverse_bind),
                morphs: m.morphs.as_ref(),
            }),
        }
    }

    /// Every mesh, in order.
    pub fn iter(&self) -> impl Iterator<Item = MeshView<'a>> + 'a {
        let meshes = *self;
        (0..meshes.len()).filter_map(move |i| meshes.get(i))
    }
}

/// Where a body's anchors are stored.
#[derive(Clone, Copy)]
enum AnchorSource<'a> {
    ByPart(&'a BTreeMap<(PartId, AnchorId), Anchor>),
    Own(&'a BTreeMap<AnchorId, Anchor>),
}

/// The attachment points of a body: published by its parts (a built character) or by the body itself (an imported
/// avatar).
#[derive(Clone, Copy)]
pub struct Anchors<'a>(AnchorSource<'a>);

impl<'a> Anchors<'a> {
    /// Every anchor in order, with the part that published it (`None` for the body's own). Allocates nothing.
    pub fn all(&self) -> impl Iterator<Item = (Option<&'a PartId>, &'a AnchorId, &'a Anchor)> + 'a {
        let (by_part, own) = match self.0 {
            AnchorSource::ByPart(map) => (Some(map), None),
            AnchorSource::Own(map) => (None, Some(map)),
        };
        let by_part = by_part.into_iter().flatten().map(|((part, id), a)| (Some(part), id, a));
        by_part.chain(own.into_iter().flatten().map(|(id, a)| (None, id, a)))
    }

    /// The anchor `id`, from `part` when given, or the first of that name otherwise (parts in id order).
    pub fn find(&self, part: Option<&PartId>, id: &AnchorId) -> Option<&'a Anchor> {
        self.all().find(|(p, i, _)| *i == id && (part.is_none() || *p == part)).map(|(_, _, a)| a)
    }

    /// How many anchors there are.
    pub fn len(&self) -> usize {
        match self.0 {
            AnchorSource::ByPart(map) => map.len(),
            AnchorSource::Own(map) => map.len(),
        }
    }

    /// True if there are none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
