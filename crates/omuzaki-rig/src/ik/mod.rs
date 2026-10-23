//! The IK stage of the rig (RG-05, FE-07), implementing [`IkSolver`](omuzaki_anim_api::IkSolver) on a skeleton's own bones.
//!
//! It runs after the humanoid stage ([`HumanoidRig`](crate::pose::HumanoidRig)) and before spring bones (spec §8:
//! anim blend → IK → physics). Each part is one module, added by its own task:
//!
//! | Module | Part |
//! |---|---|
//! | [`two_bone`] | analytic two-bone solve for arms and legs: pole vector, soft reach, hinge limits, weight |
//! | [`look_at`] | neck, head and eye aim (FE-07) |
//! | [`targets`] | goal sources for hands and feet (follow-up) |
//! | [`feet`] | foot placement (follow-up) |
//! | [`guard`] | keeps the forearms and hands out of the body, last before the springs (#2353) |

pub mod feet;
pub mod guard;
pub mod look_at;
pub mod targets;
pub mod two_bone;

pub use two_bone::TwoBoneIk;
