//! The licence record ops: `licence.derive`, `licence.describe`, `licence.set` and `licence.set_source`.
//!
//! A character's licence record is what `io.check_export` and `io.export` read to fill the rights of a file, and nothing
//! else may: rights are never guessed. These ops are how a person or an agent fixes what the check reports:
//!
//! | Op | Kind | Scope | Does |
//! |---|---|---|---|
//! | [`derive::LicenceDerive`] | query | `read` | the record the parts and application records allow: the strictest join of their rights |
//! | [`describe::LicenceDescribe`] | query | `read` | what the record holds and what is missing for an export purpose |
//! | [`set::LicenceSet`] | edit | `edit:character` | stores a record, refusing one that grants more than the parts and application records allow |
//! | [`set_source::LicenceSetSource`] | edit | `edit:character` | records where one part came from and what its rights are |
//!
//! The record lives at `characters/<id>/licence` ([`record_path`](crate::gather::avatar::record_path)); each part's
//! source row at `characters/<id>/sources/<part>` ([`source::source_path`]). `docs/design/io-api.md` has the rules.
//!
//! One file each: [`source`] (the rows being joined, and [`source::row_patch`], the one way a row is written), `store`
//! (how both documents are stored), `terms` (the nine permission flags and their order), [`mod@derive`]
//! (the join), `excess` (where a record grants more than it may), `findings` (those excesses and the private-test rule as
//! findings, shared by `licence.describe` and `io.check_export`), and an op per file.

pub mod derive;
pub mod describe;
mod excess;
pub(crate) mod findings;
pub mod set;
pub mod set_source;
pub mod source;
mod store;
mod terms;
