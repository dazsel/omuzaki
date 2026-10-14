//! The ops the stage registers (spec v3 §6): `render.*` over a staged character and `sheet.*` over its sheets. Ops
//! register themselves next to their definition, so nothing here lists them.

pub mod render;
pub mod scene;
pub mod sheet;
