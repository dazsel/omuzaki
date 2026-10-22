//! The PNG sink: one PNG per frame, alpha kept (`docs/design/record.md` D6).
//!
//! Each frame is written as `<stem>_<index>.png.tmp` and renamed when complete, so after a crash or `kill -9` every
//! `.png` on disk is whole; stale `.tmp` files are removed when a sink opens the folder again. A frame the recorder
//! could not encode in time is filled at the end with a hard link to the frame before it (a copy where the file system
//! has no links), so the sequence has no hole and the take's timeline stays exact.
#![allow(
    clippy::disallowed_methods,
    reason = "the recorder writes only inside the take folder under `record.dir`, a Machine setting no agent may write; no op passes a path in"
)]

use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use image::ImageEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use omuzaki_core::glam::UVec2;
use omuzaki_stream_api::error::StreamError;

/// Digits in a frame number: enough for 27 hours at 100 fps, and a fixed width so files sort in order.
pub const INDEX_DIGITS: usize = 7;

/// Writes the frames of one take into a folder.
#[derive(Clone, Debug)]
pub struct PngSink {
    dir: PathBuf,
    stem: String,
}

impl PngSink {
    /// Opens `dir` for frames named `<stem>_<index>.png`, creating it and removing stale `.tmp` files.
    ///
    /// # Errors
    /// [`StreamError::Io`] if the folder cannot be created or read.
    pub fn create(dir: &Path, stem: &str) -> Result<Self, StreamError> {
        fs::create_dir_all(dir).map_err(|e| io("create the frame folder", dir, &e))?;
        for entry in fs::read_dir(dir).map_err(|e| io("read the frame folder", dir, &e))?.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|x| x == "tmp") {
                // A stale partial frame from a crashed take; failing to remove it only leaves clutter behind.
                let _ = fs::remove_file(&path);
            }
        }
        Ok(Self { dir: dir.to_owned(), stem: stem.to_owned() })
    }

    /// The folder the frames go into.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The path of frame `index`.
    pub fn path_for(&self, index: u64) -> PathBuf {
        self.dir.join(format!("{}_{index:0width$}.png", self.stem, width = INDEX_DIGITS))
    }

    /// Encodes straight-alpha RGBA8 `pixels` of `size` as frame `index` and returns the file's size in bytes.
    ///
    /// # Errors
    /// [`StreamError::Config`] for a pixel buffer of the wrong length, [`StreamError::Io`] or
    /// [`StreamError::Encode`] if the file cannot be written.
    pub fn write(&self, index: u64, size: UVec2, pixels: &[u8]) -> Result<u64, StreamError> {
        let want = size.x as usize * size.y as usize * 4;
        if pixels.len() != want {
            return Err(StreamError::Config(format!(
                "{} bytes for a {}x{} RGBA frame (needs {want})",
                pixels.len(),
                size.x,
                size.y
            )));
        }
        let path = self.path_for(index);
        let tmp = path.with_extension("png.tmp");
        let file = fs::File::create(&tmp).map_err(|e| io("create", &tmp, &e))?;
        // `Fast` with the `Sub` filter keeps a 1080p frame near 20 ms on one core; the files are a working format for
        // editors, not an archive, so size matters less than keeping up.
        let encoder = PngEncoder::new_with_quality(BufWriter::new(file), CompressionType::Fast, FilterType::Sub);
        encoder
            .write_image(pixels, size.x, size.y, image::ExtendedColorType::Rgba8)
            .map_err(|e| StreamError::Encode(format!("PNG frame {index}: {e}")))?;
        fs::rename(&tmp, &path).map_err(|e| io("finish", &path, &e))?;
        let bytes = fs::metadata(&path).map_err(|e| io("measure", &path, &e))?.len();
        Ok(bytes)
    }

    /// Fills every missing frame below `frames` with the frame before it, and returns how many it filled. A missing
    /// first frame is filled from the first frame that exists.
    ///
    /// # Errors
    /// [`StreamError::Io`] if a link or copy fails, or no frame exists at all while `frames > 0`.
    pub fn fill_gaps(&self, frames: u64) -> Result<u64, StreamError> {
        let Some(first) = (0..frames).find(|&i| self.path_for(i).exists()) else {
            return if frames == 0 {
                Ok(0)
            } else {
                Err(StreamError::Io(format!("no frame of the take reached {}", self.dir.display())))
            };
        };
        let mut filled = 0;
        let mut last = self.path_for(first);
        for index in 0..frames {
            let path = self.path_for(index);
            if path.exists() {
                last = path;
                continue;
            }
            if fs::hard_link(&last, &path).is_err() {
                fs::copy(&last, &path).map_err(|e| io("fill a missing frame at", &path, &e))?;
            }
            filled += 1;
        }
        Ok(filled)
    }
}

fn io(what: &str, path: &Path, e: &std::io::Error) -> StreamError {
    StreamError::Io(format!("could not {what} {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(size: UVec2, value: u8) -> Vec<u8> {
        let mut px = Vec::new();
        for i in 0..size.x * size.y {
            px.extend_from_slice(&[
                value,
                u8::try_from(i % 256).unwrap_or(0),
                7,
                u8::try_from(i * 3 % 256).unwrap_or(0),
            ]);
        }
        px
    }

    #[test]
    fn a_frame_round_trips_with_its_alpha() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let sink = PngSink::create(dir.path(), "take")?;
        let size = UVec2::new(5, 3);
        let px = frame(size, 200);
        let bytes = sink.write(4, size, &px)?;
        assert!(bytes > 0);
        let back = image::open(sink.path_for(4))?.to_rgba8();
        assert_eq!(back.dimensions(), (5, 3));
        assert_eq!(back.into_raw(), px);
        assert!(sink.path_for(4).ends_with("take_0000004.png"));
        Ok(())
    }

    #[test]
    fn gaps_are_filled_from_the_frame_before() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let sink = PngSink::create(dir.path(), "t")?;
        let size = UVec2::new(2, 2);
        sink.write(1, size, &frame(size, 1))?;
        sink.write(4, size, &frame(size, 4))?;
        assert_eq!(sink.fill_gaps(6)?, 4);
        let first = |i| image::open(sink.path_for(i)).map(|im| im.to_rgba8().into_raw()[0]);
        assert_eq!([first(0)?, first(1)?, first(2)?, first(3)?, first(4)?, first(5)?], [1, 1, 1, 1, 4, 4]);
        Ok(())
    }

    #[test]
    fn stale_temporary_files_are_removed_and_bad_sizes_refused() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        fs::write(dir.path().join("t_0000000.png.tmp"), b"half")?;
        let sink = PngSink::create(dir.path(), "t")?;
        assert!(!dir.path().join("t_0000000.png.tmp").exists());
        assert!(matches!(sink.write(0, UVec2::new(2, 2), &[0; 3]), Err(StreamError::Config(_))));
        assert!(matches!(sink.fill_gaps(3), Err(StreamError::Io(_))));
        Ok(())
    }
}
