//! Avatars built by hand for the evaluator's tests and bench: a small humanoid, meshes whose targets each belong to one
//! expression, a material for colour and UV binds. Nothing third-party.
#![allow(dead_code, clippy::expect_used, clippy::cast_precision_loss)] // each test file uses part of this; fixture setup

use omuzaki_anim_api::{Channel, ChannelBuf, HumanoidBone, Skeleton, Transform};
use omuzaki_avatar::buffers::{BindDeltas, MorphWeights};
use omuzaki_avatar::evaluator::ExpressionEvaluator;
use omuzaki_avatar_api::avatar::{AvatarMesh, ImportedAvatar};
use omuzaki_avatar_api::expression::{AvatarExpression, ExprIx, MorphBind};
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

pub fn material() -> MaterialName {
    MaterialName::new("face").expect("a slug")
}

/// One triangle skinned to the head with `targets` morph targets.
pub fn mesh(targets: usize) -> AvatarMesh {
    let geometry = MeshData {
        positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
        normals: vec![Vec3::Z; 3],
        uvs: vec![Vec2::ZERO; 3],
        joints: vec![[0; 4]; 3],
        weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
        indices: vec![0, 1, 2],
        ..MeshData::default()
    };
    let part = PartMesh {
        name: "face".to_owned(),
        geometry,
        joint_bones: vec![JointRef::HEAD],
        material: material(),
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    };
    let mesh = AvatarMesh::new(part).with_inverse_bind(vec![Mat4::IDENTITY]);
    if targets == 0 {
        return mesh;
    }
    let morphs = (0..targets)
        .map(|i| SparseMorph {
            name: Slug::new(format!("t{i}")).expect("a slug"),
            indices: vec![0],
            positions: vec![Vec3::Z * 0.01],
            normals: Vec::new(),
        })
        .collect();
    mesh.with_morphs(MorphSet::from_targets(3, morphs).expect("valid targets"))
}

pub fn name(n: &str) -> ExpressionName {
    ExpressionName::parse(n).expect("a valid name")
}

/// An avatar with one mesh and one expression per name, expression `i` moving target `i` at full weight. Every name
/// that is a channel outside the gaze region drives its own expression; gaze drivers are the test's to set.
pub fn avatar(eyes: bool, names: &[&str]) -> ImportedAvatar {
    let mut a = ImportedAvatar::new(ContentHash([3; 32]), skeleton(eyes));
    a.meshes.push(mesh(names.len()));
    a.materials.insert(material(), MaterialSpec::toon(Colour::WHITE));
    for (i, n) in names.iter().enumerate() {
        let target = u16::try_from(i).expect("few expressions");
        a.expressions.push(AvatarExpression::new(name(n)).with_morph(MorphBind::new(0, target, 1.0)));
        if let Some(channel) = Channel::from_name(n).filter(|c| c.region() != omuzaki_anim_api::Region::Gaze) {
            a.drivers.set(channel, Some(ExprIx(target)));
        }
    }
    a
}

/// The index of the expression called `n`.
pub fn ix(a: &ImportedAvatar, n: &str) -> ExprIx {
    a.expression(&name(n)).expect("the expression exists")
}

/// An evaluator with its buffers.
pub struct Run {
    pub evaluator: ExpressionEvaluator,
    pub weights: MorphWeights,
    pub binds: BindDeltas,
}

impl Run {
    pub fn new(avatar: &ImportedAvatar) -> Self {
        assert_eq!(avatar.validate(), Ok(()), "the test avatar is valid");
        let evaluator = ExpressionEvaluator::new(avatar);
        Self { weights: evaluator.morph_weights(), binds: evaluator.bind_deltas(), evaluator }
    }

    /// Evaluates `channels` set as listed, the rest 0.
    pub fn eval(&mut self, set: &[(Channel, f32)]) -> &mut Self {
        let mut channels = ChannelBuf::NEUTRAL;
        for (c, v) in set {
            channels.set(*c, *v);
        }
        self.evaluator.evaluate(&channels, &mut self.weights, &mut self.binds);
        self
    }

    /// The morph weight of target `i` of mesh 0: expression `i`'s own target.
    pub fn target(&self, i: ExprIx) -> f32 {
        self.weights.mesh(0)[i.index()]
    }
}
