//! Scene preparation: pure functions over an [`IoScene`](omuzaki_io_api::IoScene) that make an imported model ready to
//! perform and to export (spec v3 §27, EX-12). [`avatar::to_avatar`] runs them all, in order, to turn a model into an
//! imported avatar (`docs/design/byom.md`).

pub mod arkit;
pub mod arkit_shapes;
pub mod avatar;
pub mod cleanup;
pub mod humanoid;
pub mod tpose;
