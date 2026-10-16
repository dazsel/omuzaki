//! FBX for Omuzaki (spec v3 §27 EX-09), registered against `omuzaki-io-api` as format `fbx`. Import only: FBX stays a
//! one-way door (N+N against `IoScene`, io-api D1), so there is no exporter.
//!
//! Use [`run_import`](omuzaki_io_api::run_import), not [`FbxImporter`] directly: it adds the size limits, the scene
//! invariants and the rights note. FBX carries no licence fields, so the outcome states
//! [`LicenceEvidence::Absent`] and `run_import` adds `rights.no_license_info`.
//!
//! The parsing is [ufbx](https://github.com/ufbx/ufbx)'s (a C library compiled from the source the `ufbx` crate
//! vendors; no network at build time). This crate reads binary FBX 3000+ and ASCII FBX, and nothing else ufbx can read:
//! an OBJ is [`IoError::Unsupported`]. It loads from memory only (no external files; textures come through the
//! [`Resolver`](omuzaki_io_api::Resolver) in a later task) and ignores animation.
//!
//! **Canonical space.** ufbx converts the file to right-handed, +Y up, the model's front toward +Z, metres (see
//! [`load`] for the exact settings and the FBX axis convention). A file that needed a conversion says so:
//!
//! | Rule | Severity | When |
//! |---|---|---|
//! | `fbx.units_converted` | info | the file's unit scale was not metres |
//! | `fbx.axes_converted` | info | the file's axes were not right-handed Y-up with the front toward +Z |
//! | `fbx.content_dropped` | warning | the file has meshes, skins, materials, textures, lights, cameras or animation, which this version does not import |
//! | `fbx.repaired` | warning | ufbx repaired the file (a duplicate object id, a bad connection, a truncated array) and the hierarchy may differ from what was written |
//!
//! This version imports the node hierarchy only. The vertex arrays and embedded images, which are most of a real file,
//! are never read, so `Limits::max_vertices` and `Limits::max_images` cannot trip; what the file holds besides nodes is
//! counted and reported as `fbx.content_dropped` (animation, which is not loaded either, is detected from the file's structure, an `AnimationStack` object, and named without a count), which a strict import turns into an error.
//!
//! **Untrusted input.** Every failure is an [`IoError`], never a panic. The input is capped by
//! `Limits::max_input_bytes`, the node count by `Limits::max_nodes`, and ufbx's own memory, allocation count and
//! hierarchy depth by the constants in [`load`]; the node cap also sets how much memory ufbx may use while it builds the
//! scene, so a tight cap gets a tight allocator. Progress and cancellation go through `Progress::checkpoint`.
#![deny(missing_docs)]

mod load;
mod nodes;

use omuzaki_io_api::{
    Confidence, FormatInfo, ImportCtx, ImportInput, ImportOutcome, Importer, IoError, LicenceEvidence,
    register_importer,
};

/// The binary header: the 20-byte text `Kaydara FBX Binary  ` and a NUL.
const BINARY_MAGIC: &[u8; 21] = b"Kaydara FBX Binary  \0";
/// How far into the file an ASCII header comment may start.
const ASCII_WINDOW: usize = 1024;
/// A UTF-8 byte order mark, which some tools put before an ASCII file.
const BOM: &[u8] = b"\xEF\xBB\xBF";
/// What the first line of an ASCII FBX starts with.
const ASCII_MARKER: &[u8] = b"; FBX ";

/// FBX (binary 3000+ and ASCII) read into an `IoScene`. Format id `fbx`, import only.
#[derive(Default)]
pub struct FbxImporter;

/// True for the bytes of a binary or ASCII FBX header.
fn is_fbx(bytes: &[u8]) -> bool {
    if bytes.starts_with(BINARY_MAGIC) {
        return true;
    }
    let window = &bytes[..bytes.len().min(ASCII_WINDOW)];
    let text = window.strip_prefix(BOM).unwrap_or(window);
    // Blank lines may come first; the first line with content must be the header comment.
    let first = text.iter().position(|b| !b.is_ascii_whitespace()).map_or(&text[..0], |at| &text[at..]);
    first.starts_with(ASCII_MARKER)
}

impl Importer for FbxImporter {
    fn info(&self) -> FormatInfo {
        FormatInfo { id: "fbx", name: "FBX", extensions: &["fbx"], media_type: "application/vnd.autodesk.fbx" }
    }

    fn sniff(&self, input: &ImportInput<'_>) -> Confidence {
        if is_fbx(input.bytes) { Confidence::Certain } else { Confidence::No }
    }

    fn import(&self, input: &ImportInput<'_>, ctx: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
        let limits = &ctx.options.limits;
        let size = input.bytes.len() as u64;
        if size > limits.max_input_bytes {
            return Err(IoError::Limit { what: "input size in bytes", limit: limits.max_input_bytes, got: size });
        }
        if !is_fbx(input.bytes) {
            return Err(IoError::unsupported("a file that is not FBX", "convert it to glTF or VRM"));
        }
        let loaded = load::load(input.bytes, ctx)?;
        let (scene, report) = nodes::convert(&loaded, load::mentions_animation(input.bytes), ctx)?;
        let mut outcome = ImportOutcome::new(scene, LicenceEvidence::Absent);
        outcome.report = report;
        Ok(outcome)
    }
}

register_importer!(FbxImporter);

omuzaki_core::define_link!();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ascii_header_may_follow_a_bom_and_blank_lines_within_the_window() {
        assert!(is_fbx(b"\xEF\xBB\xBF\r\n\n  ; FBX 7.4.0 project file"));
        let mut far = vec![b'\n'; ASCII_WINDOW];
        far.extend_from_slice(b"; FBX 7.4.0");
        assert!(!is_fbx(&far));
        assert!(!is_fbx(b"; not fbx"));
        assert!(!is_fbx(b""));
    }
}
