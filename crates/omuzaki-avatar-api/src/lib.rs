//! Contract for imported avatars (spec v3 §27 EX-12; design: `docs/design/byom.md`; ADR 0033).
//!
//! An imported avatar (a VRM or glTF a person brings) is a performer body of its own, not a part of a procedural
//! `CharacterDef`: it brings its whole skeleton, its own weights and inverse bind matrices, and its own expressions.
//! This crate holds the types the importer (`omuzaki-io-ops`) writes and everything downstream reads:
//! - [`avatar`]: [`ImportedAvatar`](avatar::ImportedAvatar), its meshes, [`driven`](avatar::ImportedAvatar::driven) and
//!   [`validate`](avatar::ImportedAvatar::validate)
//! - [`expression`]: expressions and their binds, override rules and the [`ChannelDrivers`](expression::ChannelDrivers)
//!   table that says which channel drives which expression
//! - [`gaze`]: the one gaze route chosen at prepare time and the look-at range maps
//! - [`retained`]: what export needs and the runtime does not use, kept exactly as the file had it (D8)
//! - [`def`]: [`AvatarDef`](def::AvatarDef), the document node at `characters/<id>/avatar` (D5)
//! - [`summary`]: [`AvatarSummary`](summary::AvatarSummary), what `io.inspect` reports about a prepared avatar
//! - [`body`]: [`Body`](body::Body) and [`BodyView`](body::BodyView), the one performer surface for built and imported
//!   characters (D1, condition 1)
//! - [`error`]: [`AvatarError`](error::AvatarError), why an avatar is not valid
//!
//! Callers import by path (ADR 0054 §2); [`prelude`] holds the few names nearly every caller needs.
#![deny(missing_docs)]

pub mod avatar;
pub mod body;
pub mod def;
pub mod error;
pub mod expression;
pub mod gaze;
pub mod retained;
pub mod summary;

/// The names nearly every caller needs: the avatar, the body surface, the error.
pub mod prelude {
    pub use crate::avatar::ImportedAvatar;
    pub use crate::body::{Body, BodyView};
    pub use crate::error::AvatarError;
    pub use crate::expression::ExprIx;
}
