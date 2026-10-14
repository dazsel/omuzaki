//! Variant ops (spec CB-13, §26 SH-10): a character's per-use variants, edited as `variant.*` ops.
//!
//! A variant is a set of overrides on the base character, stored as one node under `characters/<id>/variants/<name>`
//! (`variants/<name>.toml` in a project). Every edit is checked by resolving the variant on a copy with
//! `resolve_variant`, so a wrong path or value is refused before anything is written.
//!
//! | Module | Holds |
//! |---|---|
//! | [`create`] | `variant.create`: a new variant, optionally extending another and covering a range of shots |
//! | [`set_override`], [`clear_override`] | `variant.set_override` and `variant.clear_override`: one override each |
//! | [`delete`] | `variant.delete`, refused while another variant extends it |
//! | [`list`], [`preview`] | `variant.list` and `variant.preview`: queries that write nothing |
//! | [`select`] | `VariantSelector`: the stage's pick of a variant from head coverage, over `select_variant` |
//! | [`access`] | reading variants from the character's own `variants/` sub-tree, and the shared refusals |

pub mod clear_override;
pub mod create;
pub mod delete;
pub mod list;
pub mod preview;
pub mod select;
pub mod set_override;

pub(crate) mod access;
