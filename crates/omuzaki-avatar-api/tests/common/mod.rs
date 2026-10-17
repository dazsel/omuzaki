//! Hand-built avatars for the tests: a small humanoid, one skinned triangle with morph targets, one material.
#![allow(dead_code, clippy::expect_used, clippy::cast_precision_loss)] // each test file uses part of this; fixture setup

use omuzaki_anim_api::{HumanoidBone, Skeleton, Transform};
use omuzaki_avatar_api::avatar::{AvatarMesh, ImportedAvatar};
use omuzaki_avatar_api::expression::{AvatarExpression, ExprIx};
use omuzaki_char_api::{JointRef, MaterialName, MaterialSpec, PartMesh};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::mesh::MeshData;
use omuzaki_io_api::ExpressionName;
use omuzaki_render_api::{MorphSet, SparseMorph};
use omuzaki_store_api::ContentHash;

/// The 15 required humanoid bones, plus the eyes when `eyes`.
pub fn skeleton(eyes: bool) -> Skeleton {
    let bones: Vec<HumanoidBone> = HumanoidBone::ALL
        .into_iter()
        .filter(|b| b.is_required() || (eyes && matches!(b, HumanoidBone::LeftEye | HumanoidBone::RightEye)))
        .collect();
    Skeleton::humanoid(&bones, |b| Transform::from_translation(Vec3::new(0.0, 0.1 * (b.index() % 5) as f32, 0.0)))
        .expect("a valid humanoid")
}

pub fn skin() -> MaterialName {
    MaterialName::new("skin").expect("a slug")
}

/// One triangle skinned to the head, with `targets` morph targets that each move vertex 0.
pub fn head_mesh(targets: usize) -> AvatarMesh {
    let geometry = MeshData {
        positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
        normals: vec![Vec3::Z; 3],
        uvs: vec![Vec2::ZERO; 3],
        joints: vec![[0; 4]; 3],
        weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
        indices: vec![0, 1, 2],
        ..MeshData::default()
    };
    let mesh = PartMesh {
        name: "face".to_owned(),
        geometry,
        joint_bones: vec![JointRef::HEAD],
        material: skin(),
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    };
    let morphs = (0..targets)
        .map(|i| SparseMorph {
            name: Slug::new(format!("t{i}")).expect("a slug"),
            indices: vec![0],
            positions: vec![Vec3::Z * 0.01],
            normals: Vec::new(),
        })
        .collect();
    let mut mesh = AvatarMesh::new(mesh).with_inverse_bind(vec![Mat4::IDENTITY]);
    if targets > 0 {
        mesh = mesh.with_morphs(MorphSet::from_targets(3, morphs).expect("valid targets"));
    }
    mesh
}

pub fn name(n: &str) -> ExpressionName {
    ExpressionName::parse(n).expect("a valid name")
}

/// An avatar with one mesh of `targets` targets, the skin material, and the named expressions, each moving the target
/// of its own index (or nothing, past the last target).
pub fn avatar(eyes: bool, targets: usize, expressions: &[&str]) -> ImportedAvatar {
    let mut a = ImportedAvatar::new(ContentHash([7; 32]), skeleton(eyes));
    a.meshes.push(head_mesh(targets));
    a.materials.insert(skin(), MaterialSpec::toon(Colour::WHITE));
    a.expressions = expressions
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let e = AvatarExpression::new(name(n));
            match u16::try_from(i).ok().filter(|i| usize::from(*i) < targets) {
                Some(t) => e.with_morph(omuzaki_avatar_api::expression::MorphBind::new(0, t, 1.0)),
                None => e,
            }
        })
        .collect();
    a
}

/// The index of the expression called `n`.
pub fn ix(a: &ImportedAvatar, n: &str) -> ExprIx {
    a.expression(&name(n)).expect("the expression exists")
}
