//! The confirm token (design D6): what `io.inspect {as: avatar}` returns and `io.import {as: avatar}` must be given, so
//! a person confirms exactly what they saw.
//!
//! It is a BLAKE3 hash ([`ContentHash::of`]) over these fields, in this order, each written as its length (a `u64`,
//! little-endian) and then its bytes:
//! 1. the token's own tag and [`PREPARE_VERSION`], so a change to the preparation invalidates old tokens;
//! 2. the source file's content hash;
//! 3. every file the import read beside it (sidecar `.bin`s and images, through the `Resolver`): its URI and content
//!    hash, sorted by URI;
//! 4. the canonical [`AvatarDef`] (its JSON, whose maps are ordered) and the preparation options (the `MToon` groups
//!    the renderer draws, and strict mode);
//! 5. the importer's format id and this crate's version;
//! 6. the canonical report (its JSON);
//! 7. the character id the import suggests, so an import without `id` makes the character the person saw named.
//!
//! The avatar node names the file (`AvatarSource::file`), so the token covers the name too: import under the name the
//! file was inspected with (a `path`'s file name, or the same `name`).
//!
//! A file replaced between inspect and import, a changed dependency byte, or a changed option gives another token, so
//! the import is refused even when the findings would read the same.

use omuzaki_avatar_api::def::AvatarDef;
use omuzaki_io_api::Report;
use omuzaki_ops_api::OpError;
use omuzaki_render_api::MtoonSupport;
use omuzaki_store_api::ContentHash;

/// The version of the preparation's output. Bump it whenever `to_avatar` would prepare the same file differently, so
/// tokens from an older build are refused instead of silently confirming something else.
pub const PREPARE_VERSION: u32 = 1;

/// The token's tag: what the hash is of, so it can never equal another hash of the same bytes.
const TAG: &[u8] = b"omuzaki/io-ops/avatar-confirm";

/// The `MToon` groups, in bit order, as the token writes them.
const GROUPS: [MtoonSupport; 7] = [
    MtoonSupport::MATCAP,
    MtoonSupport::GI_EQUALISATION,
    MtoonSupport::UV_ANIMATION,
    MtoonSupport::RENDER_QUEUE_OFFSET,
    MtoonSupport::OUTLINE_WIDTH_SCREEN,
    MtoonSupport::RIM_LIGHTING_MIX,
    MtoonSupport::OUTLINE_LIGHTING_MIX,
];

/// What the token covers.
#[derive(Clone, Copy, Debug)]
pub struct Inputs<'a> {
    /// The source file's content hash.
    pub source: ContentHash,
    /// Every file read beside it, by URI, with its content hash; any order (the token sorts them).
    pub dependencies: &'a [(String, ContentHash)],
    /// The avatar node it was prepared with.
    pub def: &'a AvatarDef,
    /// The `MToon` groups the renderer draws.
    pub mtoon: MtoonSupport,
    /// Whether warnings counted as errors.
    pub strict: bool,
    /// The importer's format id.
    pub importer: &'a str,
    /// The whole report the person saw.
    pub report: &'a Report,
    /// The character id the import suggests.
    pub suggestion: &'a str,
}

/// The token of `inputs`, as 64 lowercase hex characters.
///
/// # Errors
/// `Internal` if the avatar node or the report cannot be written as JSON (they always can).
pub fn token(inputs: &Inputs<'_>) -> Result<String, OpError> {
    let mut buf = Vec::new();
    let mut field = |bytes: &[u8]| {
        buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        buf.extend_from_slice(bytes);
    };
    field(TAG);
    field(&PREPARE_VERSION.to_le_bytes());
    field(&inputs.source.0);
    let mut dependencies: Vec<&(String, ContentHash)> = inputs.dependencies.iter().collect();
    dependencies.sort();
    field(&(dependencies.len() as u64).to_le_bytes());
    for (uri, hash) in dependencies {
        field(uri.as_bytes());
        field(&hash.0);
    }
    let json = |what: &str, value: serde_json::Result<Vec<u8>>| {
        value.map_err(|e| OpError::internal(format!("writing the {what} for the confirm token: {e}")))
    };
    field(&json("avatar node", serde_json::to_vec(inputs.def))?);
    let groups: u8 = GROUPS.iter().enumerate().filter(|(_, g)| inputs.mtoon.contains(**g)).map(|(i, _)| 1u8 << i).sum();
    field(&[groups, u8::from(inputs.strict)]);
    field(inputs.importer.as_bytes());
    field(env!("CARGO_PKG_VERSION").as_bytes());
    field(&json("report", serde_json::to_vec(inputs.report))?);
    field(inputs.suggestion.as_bytes());
    Ok(ContentHash::of(&buf).to_string())
}
