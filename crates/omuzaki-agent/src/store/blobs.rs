//! Image blobs: content-addressed files in the project's `blobs/` folder.
//!
//! A blob is named `<blake3 hex>.<ext>`. Blobs are shared by every conversation of a project, so two conversations that
//! show the same render keep one file, and it is deleted only when the last conversation naming it is gone. "Naming
//! it" is found by looking for the hash in the remaining logs, which is cheap and cannot disagree with the entries.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use omuzaki_agent_api::entry::StoredBlock;
use omuzaki_agent_api::error::HostError;
use omuzaki_llm_api::message::ImageMime;

use super::io_err;

/// The folder inside a project's folder.
pub(super) const DIR: &str = "blobs";

/// The file extension of an encoding.
pub(super) fn extension(mime: ImageMime) -> &'static str {
    match mime {
        ImageMime::Png => "png",
        ImageMime::Jpeg => "jpg",
        ImageMime::Webp => "webp",
        ImageMime::Gif => "gif",
        // A future encoding is still stored; only its extension is generic.
        _ => "bin",
    }
}

/// Whether `text` is a BLAKE3 hash as the store writes it (64 lower-case hex digits). A log is a file a person can
/// edit, so a blob name read from one is checked before it becomes part of a path.
pub(super) fn is_hash(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Where the blob `hash` of encoding `mime` lives.
pub(super) fn path(project_dir: &Path, hash: &str, mime: ImageMime) -> Result<PathBuf, HostError> {
    if !is_hash(hash) {
        return Err(HostError::Store("a log names a blob that is not a hash".into()));
    }
    Ok(project_dir.join(DIR).join(format!("{hash}.{}", extension(mime))))
}

/// Writes `bytes` as a blob and returns the block that names it. Writing the same bytes twice keeps one file.
pub(super) fn put(project_dir: &Path, bytes: &[u8], mime: ImageMime) -> Result<StoredBlock, HostError> {
    let hash = blake3::hash(bytes).to_hex().to_string();
    let target = path(project_dir, &hash, mime)?;
    if !target.exists() {
        fs::create_dir_all(project_dir.join(DIR)).map_err(|e| io_err("create the blob folder", e))?;
        // Written beside the target and renamed, so a crash never leaves a half-written blob under its hash.
        let partial = target.with_extension("partial");
        fs::write(&partial, bytes).map_err(|e| io_err("write a blob", e))?;
        fs::rename(&partial, &target).map_err(|e| io_err("finish a blob", e))?;
    }
    Ok(StoredBlock::image(mime, hash))
}

/// Deletes the blobs named in `removed_logs` (the text of conversations just deleted) that no log in `remaining`
/// names. Blobs nobody ever referenced are left alone, and so are the `pinned` ones: written but not yet named by an
/// appended entry.
pub(super) fn release(
    project_dir: &Path,
    removed_logs: &[String],
    remaining: &[String],
    pinned: &HashSet<String>,
) -> Result<(), HostError> {
    let Ok(files) = fs::read_dir(project_dir.join(DIR)) else {
        return Ok(()); // no blobs folder: nothing to release
    };
    for file in files {
        let file = file.map_err(|e| io_err("list blobs", e))?;
        let name = file.file_name();
        let Some(hash) = name.to_str().and_then(|n| n.split('.').next()).filter(|h| is_hash(h)) else {
            continue;
        };
        let was_used = removed_logs.iter().any(|log| log.contains(hash));
        let still_used = remaining.iter().any(|log| log.contains(hash));
        if was_used && !still_used && !pinned.contains(hash) {
            fs::remove_file(file.path()).map_err(|e| io_err("delete a blob", e))?;
        }
    }
    Ok(())
}
