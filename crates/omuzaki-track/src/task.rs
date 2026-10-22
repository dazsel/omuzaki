//! The official `face_landmarker.task`: MediaPipe's one published bundle of the face graphs, pinned here by URL,
//! SHA-256 and size, and unpacked into a folder [`ModelDir::open_tflite`] opens.
//!
//! The bundle is a zip of four stored members: the three TFLite graphs and the canonical face. The `tflite` runtime
//! (LiteRT with XNNPACK, ADR 0061) runs those graphs as published, so with it the tracker needs no conversion step:
//! the bytes the person's machine runs are the bytes Google published (`docs/licensing/deps/mediapipe-face-landmarker.toml`,
//! Apache-2.0 per the three model cards). Each member's own SHA-256 is pinned too ([`MEMBER_SHA256`]), so the folder
//! is checked file by file every time it is opened.
//!
//! [`unpack`] is the AI runtimes' own archive unpacker (`omuzaki-ai-native`): the members are written to a staging
//! folder and renamed into place beside the bundle, so an interrupted unpack leaves nothing, and a second call reuses
//! the folder.

use std::path::{Path, PathBuf};

use omuzaki_ai_native::{ArchiveKind, Unpack};
use sha2::{Digest, Sha256};

use crate::models::{ModelDir, ModelError, TFLITE_FILES};

/// Where MediaPipe publishes the bundle (float16, version 1).
pub const TASK_URL: &str =
    "https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task";

/// The bundle's SHA-256, lower-case hex.
pub const TASK_SHA256: &str = "64184e229b263107bc2b804c6625db1341ff2bb731874b0bcc2fe6544e0bc9ff";

/// The bundle's size in bytes.
pub const TASK_SIZE: u64 = 3_758_596;

/// The bundle's file name.
pub const TASK_FILE: &str = "face_landmarker.task";

/// The SHA-256 of each member of the pinned bundle, in [`TFLITE_FILES`] order.
pub const MEMBER_SHA256: [&str; 4] = [
    "b4578f35940bf5a1a655214a1cce5cab13eba73c1297cd78e1a04c2380b0152f",
    "c7d54204ce0448474c7f3fa9af494787c0965cbdd6f20fc72867e43046bd43d5",
    "4f36dded049db18d76048567439b2a7f58f1daabc00d78bfe8f3ad396a2d2082",
    "bdbcda96dfcb7da883da124aaa2c55dee49770d934f0fcc71747f8c21bdc75b4",
];

/// Checks the bundle at `task` against the pin, unpacks its members into the folder beside it
/// (`<task>.lib`, `omuzaki_ai_native::unpack_dir`) and opens that folder. Cold: reads and writes 3.7 MB the first time.
///
/// The folder is named after `task`'s path, so `task` should be content-addressed (the registry's cache names files
/// by hash) or at least never replaced by other bytes; the members are checked against [`MEMBER_SHA256`] on every
/// open regardless, so a stale folder is refused, never used.
///
/// # Errors
/// [`ModelError::Missing`] when `task` can't be read, [`ModelError::HashMismatch`] when it is not the pinned bundle or
/// a member is not the published one, [`ModelError::Load`] when the folder can't be written.
pub fn unpack(task: &Path) -> Result<ModelDir, ModelError> {
    let bytes = std::fs::read(task).map_err(|_| ModelError::Missing(task.to_path_buf()))?;
    if bytes.len() as u64 != TASK_SIZE || format!("{:x}", Sha256::digest(&bytes)) != TASK_SHA256 {
        return Err(ModelError::HashMismatch { file: TASK_FILE.to_owned() });
    }
    ModelDir::open_tflite(&unpack_unchecked(task)?)
}

/// Unpacks `task`'s members without checking the bundle: [`unpack`] after its check, and the tests' stand-ins.
fn unpack_unchecked(task: &Path) -> Result<PathBuf, ModelError> {
    let wanted = |name: &str| TFLITE_FILES.contains(&name);
    Unpack::new(ArchiveKind::Zip, "", TFLITE_FILES[1])
        .with_wanted(&wanted)
        .run(task)
        .map_err(|e| ModelError::Load(format!("{TASK_FILE}: {e}")))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    /// A stand-in bundle at `path`: `members` as zip entries at the root.
    fn bundle(path: &Path, members: &[&str]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for name in members {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(name.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn the_members_unpack_beside_the_bundle_and_are_checked_against_their_pins() {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join(TASK_FILE);
        bundle(&task, &[TFLITE_FILES.as_slice(), &["extra.txt"]].concat());
        let folder = unpack_unchecked(&task).unwrap();
        for name in TFLITE_FILES {
            assert_eq!(std::fs::read(folder.join(name)).unwrap(), name.as_bytes());
        }
        assert!(!folder.join("extra.txt").exists(), "only the four members are taken");
        // Stand-in bytes are not the published graphs.
        assert!(
            matches!(ModelDir::open_tflite(&folder), Err(ModelError::HashMismatch { file }) if file == TFLITE_FILES[0])
        );
        // And a bundle that is not the pinned one is refused before anything is unpacked.
        assert!(matches!(unpack(&task), Err(ModelError::HashMismatch { file }) if file == TASK_FILE));
        assert!(matches!(unpack(&dir.path().join("absent")), Err(ModelError::Missing(_))));
    }

    #[test]
    fn a_bundle_without_the_landmark_graph_unpacks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join(TASK_FILE);
        bundle(&task, &[TFLITE_FILES[0], TFLITE_FILES[2]]);
        assert!(matches!(unpack_unchecked(&task), Err(ModelError::Load(_))));
        assert!(!omuzaki_ai_native::unpack_dir(&task).exists());
    }

    /// The real bundle, when `OMUZAKI_FACE_TASK` names a downloaded copy: the pins match the published bytes.
    #[test]
    fn the_published_bundle_unpacks_and_opens() {
        let Some(task) = std::env::var_os("OMUZAKI_FACE_TASK") else {
            eprintln!("skipping: set OMUZAKI_FACE_TASK to a downloaded {TASK_FILE} ({TASK_URL})");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join(TASK_FILE);
        std::fs::copy(task, &copy).unwrap();
        let models = unpack(&copy).unwrap();
        assert_eq!(models.files(), &TFLITE_FILES);
        assert!(models.geometry().is_ok());
    }
}
