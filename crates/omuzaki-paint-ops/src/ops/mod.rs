//! The `paint.*` ops, one file per group.

mod brushes;
mod export;
mod layers;
mod marks;
mod painting;
mod part;
mod render;

use omuzaki_ops_api::{OpKind, OpMeta, Scope, Undo};

/// An edit of a painting: undone by its patch.
const fn edit(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::EditCharacter, undo: Undo::Auto }
}

/// A question about a painting.
const fn query(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
}
