//! `BodyView`: a built character's view is exactly what the stage reads from it, field for field (over the char mock);
//! an imported avatar's view exposes its morphs, per-mesh inverse binds, constraints and anchors.
#![allow(clippy::unwrap_used, clippy::expect_used)] // fixture helpers outside #[test] functions

mod common;

use std::ptr;

use omuzaki_anim_api::{BoneConstraint, ConstraintKind, ConstraintSet, HumanoidBone};
use omuzaki_avatar_api::body::{Body, BodyView};
use omuzaki_char_api::{
    Anchor, AnchorId, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, JointRef, RegistryBuilder,
};
use omuzaki_core::glam::{Quat, Vec3};

/// True if `a` and `b` are the same value in memory: the view borrows, it never copies.
fn same<T: ?Sized>(a: &T, b: &T) -> bool {
    ptr::eq(a, b)
}

fn built() -> BuiltCharacter {
    omuzaki_char_mock::link();
    let def = omuzaki_char_mock::mock_character().unwrap();
    RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

#[test]
fn a_built_view_is_what_the_stage_reads_field_for_field() {
    let body = Body::Built(Box::new(built()));
    let Body::Built(b) = &body else { unreachable!() };
    let view = body.view();
    assert!(!b.meshes.is_empty() && !b.runtimes.is_empty() && !b.anchors.is_empty(), "the mock exercises every field");

    assert_eq!(view.meshes.len(), b.meshes.len());
    for (i, (m, built)) in view.meshes.iter().zip(&b.meshes).enumerate() {
        assert!(same(m.mesh, &built.mesh), "mesh {i} is the built mesh itself");
        assert_eq!((m.part, m.local), (Some(&built.part), built.local));
        assert_eq!((m.inverse_bind, m.morphs), (None, None), "built meshes use the rig's binds and have no morphs");
    }
    assert!(same(view.materials, &b.materials) && same(view.textures, &b.textures));
    assert!(same(view.skeleton, &b.rig.skeleton) && same(view.springs, &b.rig.springs));
    assert!(same(view.bind, &b.rig.bind) && same(view.drivers, b.rig.drivers.as_slice()));
    assert_eq!(view.constraints, &ConstraintSet::default(), "a built character has no node constraints");
    assert_eq!(view.face, b.face.as_ref());
    assert!(view.palette.is_some_and(|p| same(p, &b.palette)));
    assert!(same(view.runtimes, b.runtimes.as_slice()));
    assert_eq!(view.stature, b.stature);

    assert_eq!(view.anchors.len(), b.anchors.len());
    let listed: Vec<_> = view.anchors.all().map(|(p, id, a)| (p.cloned(), id.clone(), a.clone())).collect();
    let expected: Vec<_> = b.anchors.iter().map(|((p, id), a)| (Some(p.clone()), id.clone(), a.clone())).collect();
    assert_eq!(listed, expected);
    let ((part, id), anchor) = b.anchors.iter().next().unwrap();
    assert_eq!(view.anchors.find(Some(part), id), Some(anchor));
    assert_eq!(view.anchors.find(None, id), Some(anchor), "without a part, the first of that name");
}

#[test]
fn an_imported_view_exposes_morphs_binds_constraints_and_anchors() {
    let mut avatar = common::avatar(true, 3, &["happy"]);
    let head_top = AnchorId::new("head-top").unwrap();
    let anchor = Anchor { bone: JointRef::HEAD, position: Vec3::Y * 0.2, rotation: Quat::IDENTITY };
    avatar.anchors.insert(head_top.clone(), anchor.clone());
    let skeleton = &avatar.skeleton;
    let (eye_l, eye_r) = (
        skeleton.humanoid_bone(HumanoidBone::LeftEye).unwrap(),
        skeleton.humanoid_bone(HumanoidBone::RightEye).unwrap(),
    );
    avatar.constraints = ConstraintSet {
        constraints: vec![BoneConstraint { bone: eye_r, source: eye_l, kind: ConstraintKind::Rotation, weight: 1.0 }],
    };
    assert_eq!(avatar.validate(), Ok(()));

    let body = Body::Imported(Box::new(avatar));
    let Body::Imported(a) = &body else { unreachable!() };
    let view: BodyView<'_> = body.view();
    assert_eq!(view.meshes.len(), 1);
    let mesh = view.meshes.get(0).unwrap();
    assert!(same(mesh.mesh, &a.meshes[0].mesh));
    assert_eq!((mesh.part, mesh.local), (None, 0));
    assert_eq!(mesh.inverse_bind, Some(a.meshes[0].inverse_bind.as_slice()), "per joint run");
    assert_eq!(mesh.morphs.map(omuzaki_render_api::MorphSet::len), Some(3));
    assert!(view.meshes.get(1).is_none());
    assert!(same(view.constraints, &a.constraints));
    assert!(same(view.skeleton, &a.skeleton) && same(view.materials, &a.materials));
    assert_eq!(view.anchors.find(None, &head_top), Some(&anchor));
    assert_eq!(view.anchors.all().next().map(|(p, _, _)| p), Some(None), "the body's own anchors have no part");
    assert_eq!(
        (view.face, view.palette, view.runtimes.len(), view.drivers.len(), view.stature),
        (None, None, 0, 0, None)
    );
}
