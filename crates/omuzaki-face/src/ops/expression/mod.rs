//! The expression ops (FE-03): `face.expression.set`, `delete`, `rename` and `duplicate` edit a character's named
//! expressions under `face/expressions/<id>`, and `face.expression.values` previews one at a weight.
//!
//! Every edit plans against the document and returns a patch, as the style ops do; none writes the document itself, so
//! a refusal changes nothing and undo is the exact inverse. The five visemes and `blink` are the VRM contract: they can
//! be tuned with `set` (keeping their kind) but never deleted, renamed or duplicated. The rules are in `rules.rs`, the
//! stored-face mechanics in `store.rs`.

mod delete;
mod duplicate;
mod rename;
mod rules;
mod set;
mod store;
mod values;

pub use delete::DeleteExpression;
pub use duplicate::DuplicateExpression;
pub use rename::RenameExpression;
pub use set::SetExpression;
pub use values::ExpressionValues;
