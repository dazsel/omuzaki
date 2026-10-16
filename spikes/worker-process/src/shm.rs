//! The slot ring: a POSIX shared-memory object (a file under `/dev/shm`) the worker writes frames into and the host reads.
//!
//! Plain positioned reads and writes, not `mmap`, so the spike needs no `unsafe` and no extra crate. The page cache is the
//! shared memory; the read costs the host one copy per frame, which is measured and reported as the hand-off cost.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::Error;

/// Slots in the ring: one being written, one just finished, one spare, so a slow host read is not overwritten.
pub const SLOTS: u32 = 3;

/// The ring file, opened by both processes.
pub struct SlotFile {
    file: File,
    frame_bytes: u64,
}

impl SlotFile {
    /// Creates the ring (host side) sized for `SLOTS` frames of `frame_bytes`.
    ///
    /// # Errors
    /// Fails when the file cannot be created or sized.
    pub fn create(path: &Path, frame_bytes: u64) -> Result<Self, Error> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(Error::io(format!("creating {}", path.display())))?;
        file.set_len(frame_bytes * u64::from(SLOTS)).map_err(Error::io("sizing the slot ring"))?;
        Ok(Self { file, frame_bytes })
    }

    /// Opens an existing ring (worker side).
    ///
    /// # Errors
    /// Fails when the file is missing or too small.
    pub fn open(path: &Path, frame_bytes: u64) -> Result<Self, Error> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(Error::io(format!("opening {}", path.display())))?;
        let len = file.metadata().map_err(Error::io("reading the slot ring size"))?.len();
        if len < frame_bytes * u64::from(SLOTS) {
            return Err(Error::Config(format!("slot ring is {len} bytes, need {SLOTS} slots of {frame_bytes}")));
        }
        Ok(Self { file, frame_bytes })
    }

    fn offset(&self, slot: u32) -> Result<u64, Error> {
        if slot >= SLOTS {
            return Err(Error::Config(format!("slot {slot} is outside the {SLOTS}-slot ring")));
        }
        Ok(u64::from(slot) * self.frame_bytes)
    }

    /// Writes one whole frame into `slot`.
    ///
    /// # Errors
    /// Fails on a bad slot or a failed write.
    pub fn write(&self, slot: u32, frame: &[u8]) -> Result<(), Error> {
        self.file.write_all_at(frame, self.offset(slot)?).map_err(Error::io("writing a frame to its slot"))
    }

    /// Reads `slot` into `frame`, which must be exactly one frame long.
    ///
    /// # Errors
    /// Fails on a bad slot or a failed read.
    pub fn read(&self, slot: u32, frame: &mut [u8]) -> Result<(), Error> {
        self.file.read_exact_at(frame, self.offset(slot)?).map_err(Error::io("reading a frame from its slot"))
    }
}

/// Removes the ring file when the host is done with it.
pub struct RingPath(pub PathBuf);

impl Drop for RingPath {
    fn drop(&mut self) {
        // The file lives in tmpfs and is named for this process; a failed removal leaves one small file and no remedy.
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_independent() {
        let path = std::env::temp_dir().join(format!("wp-shm-test-{}", std::process::id()));
        let _guard = RingPath(path.clone());
        let ring = SlotFile::create(&path, 8).unwrap();
        for slot in 0..SLOTS {
            ring.write(slot, &[u8::try_from(slot).unwrap() + 1; 8]).unwrap();
        }
        let mut out = [0u8; 8];
        for slot in 0..SLOTS {
            ring.read(slot, &mut out).unwrap();
            assert_eq!(out, [u8::try_from(slot).unwrap() + 1; 8]);
        }
        assert!(ring.write(SLOTS, &out).is_err());
    }
}
