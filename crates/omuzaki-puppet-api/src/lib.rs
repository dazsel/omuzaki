//! Contract for flat 2D puppets (spec v3 EX-08; `docs/decisions/0010-live2d-and-inochi2d.md`; design:
//! `docs/design/puppet2d.md`).
//!
//! A puppet is layered 2D art on meshes: each [`Part`] is a textured triangle mesh, parts hang under groups and
//! [`Lattice`] deformers, and [`Param`]eters move them through keyform [`Binding`]s (vertex offsets, transforms,
//! opacity, tint, draw order). Physics pendulums write parameters too (hair, ears, accessories). The runtime evaluates
//! the parameters into a [`DrawList2D`], which a renderer (the CPU reference in `omuzaki-puppet`, the GPU pass in
//! `omuzaki-render`) draws with the puppet's blend modes, masks and composites.
//!
//! The model is a superset of the `Inochi2D` puppet (BSD-2): an `.inp` imports into it without loss of what it draws,
//! and a puppet built from layered art (`omuzaki-puppet`'s auto-rig) is the same data. Everything stored is `serde` +
//! `JsonSchema`, so ops and the MCP server derive from the types.
//!
//! **Space.** Model space is the art's pixels: x right, y down, the origin where the puppet's root sits (the canvas
//! centre for imported art). Angles are radians, positive clockwise on screen (y down).
#![deny(missing_docs)]

pub mod check;
pub mod draw;
pub mod mapping;
pub mod model;
pub mod param;
pub mod physics;

pub use check::{PuppetError, Rule};
pub use draw::{DrawCmd2D, DrawList2D, DrawState, Vertex2D};
pub use mapping::{ChannelSource, MappingEntry, ParamMapping};
pub use model::{
    BlendMode, Composite, Drawable, Lattice, MaskMode, MaskRef, Mesh, Node, NodeIx, NodeKind, Part, PuppetDef,
    PuppetMeta, TexIx, TextureRef, Transform2,
};
pub use param::{BindTarget, BindValues, Binding, Interp, Param, ParamIx};
pub use physics::{MapMode, Pendulum, PendulumModel, PhysicsWorld};
