//! Remote access with OAuth.

// Nothing calls these yet: the token check that uses them lands. Until then the allow keeps the build clean.
#[allow(dead_code)]
mod scopes;
// Nothing calls these until the remote HTTP port wires them in.
// Crate-visible so the access and gate modules can name the challenge types.
#[allow(dead_code)]
pub(crate) mod challenge;
#[allow(dead_code)]
pub(crate) mod metadata;
