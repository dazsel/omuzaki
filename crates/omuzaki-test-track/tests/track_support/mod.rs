//! The runtime-archive source shared by `tests/track_models.rs` and `benches/track.rs`.

use std::io::Read;
use std::path::PathBuf;

use omuzaki_ai_api::{FetchError, FileSource, PinnedFile, Progress, Sha256};
use sha2::Digest;

/// Archives from one folder (`OMUZAKI_ORT_CACHE`) under their release file names; never downloads. Checks the pinned
/// size, then the pinned SHA-256 (ADR 0017), as `FileSource::get` does: a file with the right length and other bytes
/// is refused.
pub struct DirSource(pub PathBuf);

impl FileSource for DirSource {
    fn get(&self, pin: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
        let asset = pin.urls.first().and_then(|url| url.rsplit('/').next()).unwrap_or_default();
        let path = self.0.join(asset);
        let io = |e: std::io::Error| FetchError::Io { path: path.display().to_string(), reason: e.to_string() };
        let actual = std::fs::metadata(&path).map_err(io)?.len();
        if actual != pin.size {
            return Err(FetchError::Size { url: asset.to_owned(), expected: pin.size, actual });
        }
        let mut file = std::fs::File::open(&path).map_err(io)?;
        let mut hasher = sha2::Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = file.read(&mut buffer).map_err(io)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let actual = Sha256(hasher.finalize().into());
        if actual != pin.sha256 {
            return Err(FetchError::Hash { url: asset.to_owned(), expected: pin.sha256, actual });
        }
        Ok(path)
    }
}
