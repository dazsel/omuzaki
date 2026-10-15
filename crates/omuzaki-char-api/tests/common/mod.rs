//! Shared by the integration tests that build whole characters. Every built character needs a skeleton, and the
//! skeleton comes from a body part, so these tests give their characters this one: the 15 joints VRM requires, no
//! meshes. (`tests/rig.rs` has a richer body of its own because it tests the rig.)

use std::sync::Arc;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{
    BuildCtx, BuildError, FitFact, JointFit, JointRef, PartCategory, PartDef, PartKind, PartKindId, PartMeta,
    PartOutput, Publishes,
};
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::{Check, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `test-body` takes no parameters.
#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct NoParams {}

impl Validate for NoParams {
    fn check(&self, _: &mut Check) {}
}

/// The `test-body` kind.
pub struct TestBody;

impl PartKind for TestBody {
    type Params = NoParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-body",
            summary: "Publish the required joints.",
            category: PartCategory::Body,
            requires: &[],
        }
    }

    fn build(_: &NoParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let joints = HumanoidBone::ALL
            .into_iter()
            .filter(|b| b.is_required())
            .map(|b| (JointRef::Humanoid(b), Vec3::new(0.0, 0.05 * f32::from(b as u8), 0.0)));
        Ok(PartOutput::new().with_publishes(Publishes::default().with::<FitFact>(Arc::new(JointFit::new(joints)))))
    }
}
omuzaki_char_api::register_part_kind!(TestBody);

/// A `test-body` part for a character's part map.
pub fn body_part() -> PartDef {
    PartDef {
        kind: PartKindId::new("test-body").expect("a valid kind id"),
        parent: None,
        enabled: true,
        layer: 0,
        attach: None,
        params: serde_json::json!({}),
    }
}
