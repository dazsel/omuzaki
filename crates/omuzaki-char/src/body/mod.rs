//! The `body` part kind: torso, arms, legs and feet lofted from the template sections, skinned with capsule weights.
//!
//! | Module | Holds |
//! |---|---|
//! | [`mesh`] | the loft: rings from the layout, caps, the point grid, the surface and the skin |
//! | [`junction`] | the shaping of the rings where regions meet (shoulder, hip, knee, elbow, ankle) |
//! | [`part`] | the [`Body`](part::Body) kind: parameters to layout to meshes, material and published fit |
//! | [`hands`] | the hand styles (mitten, simple, five-finger) |

pub mod hands;
pub mod junction;
pub mod mesh;
pub mod part;
