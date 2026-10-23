//! A simulated disk under the journal writer (#366, journal.md 12 item 2): what a power cut leaves, which a `kill -9`
//! can't show because the page cache survives it.
//!
//! [`SimFs`] keeps every journal file in memory twice: what was written (volatile) and what a flush made durable. A
//! cut, at a chosen operation or when asked, freezes the disk: it keeps the durable image, plus a seeded random half of
//! the sectors written since the last flush and one torn sector (a random prefix of one more), and drops files whose
//! folder entry was never synced. Every operation after the cut fails and changes nothing. [`SimFs::materialize`] then
//! writes that image over the real journal folder, for the next open to recover from.
//!
//! The project's TOML files and the snapshot store are not simulated: they live on the real disk.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_doc::journal::{Flush, JournalFile, JournalFs};

/// The unit a disk writes atomically, or tears.
pub const SECTOR: usize = 512;

/// A seeded `SplitMix64`: the cut's choices come from the seed alone.
#[derive(Clone, Debug)]
pub struct SimRng(u64);

impl SimRng {
    /// A generator for `seed`.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number below `n` (0 for `n` of 0).
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }
}

#[derive(Clone, Debug, Default)]
struct File {
    volatile: Vec<u8>,
    durable: Vec<u8>,
    /// Sectors written since the last flush.
    dirty: BTreeSet<usize>,
    /// Whether the folder entry is durable (a folder sync after the create).
    entry_durable: bool,
}

#[derive(Debug)]
struct Disk {
    files: BTreeMap<PathBuf, File>,
    /// Operations made so far.
    ops: u64,
    /// The operation the cut happens before, if one is set.
    cut_at: Option<u64>,
    /// The image the cut left, once it happened.
    image: Option<BTreeMap<PathBuf, Vec<u8>>>,
    rng: SimRng,
}

/// A simulated disk for the journal writer. Cheap to clone: clones share the disk.
#[derive(Clone, Debug)]
pub struct SimFs {
    disk: Arc<Mutex<Disk>>,
}

fn lock(disk: &Mutex<Disk>) -> MutexGuard<'_, Disk> {
    disk.lock().unwrap_or_else(PoisonError::into_inner)
}

fn cut_error() -> io::Error {
    io::Error::other("the power was cut")
}

impl SimFs {
    /// A disk holding the journal files already in `dir` (durable), whose cut is chosen by `seed`.
    ///
    /// # Errors
    /// Any I/O error reading `dir`; a missing folder is an empty disk.
    pub fn load(dir: &Path, seed: u64) -> io::Result<Self> {
        let mut files = BTreeMap::new();
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if entry.file_type()?.is_file() {
                        let bytes = std::fs::read(entry.path())?;
                        let file = File {
                            volatile: bytes.clone(),
                            durable: bytes,
                            dirty: BTreeSet::new(),
                            entry_durable: true,
                        };
                        files.insert(entry.path(), file);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let disk = Disk { files, ops: 0, cut_at: None, image: None, rng: SimRng::new(seed) };
        Ok(Self { disk: Arc::new(Mutex::new(disk)) })
    }

    /// Cuts the power just before operation `n` (counted from 1): that operation and every later one fail.
    pub fn cut_before(&self, n: u64) {
        lock(&self.disk).cut_at = Some(n);
    }

    /// Cuts the power now, if it wasn't cut already.
    pub fn cut_now(&self) {
        let mut disk = lock(&self.disk);
        cut(&mut disk);
    }

    /// Whether the power was cut.
    pub fn was_cut(&self) -> bool {
        lock(&self.disk).image.is_some()
    }

    /// The operations made so far.
    pub fn ops(&self) -> u64 {
        lock(&self.disk).ops
    }

    /// Writes what the cut left over the journal files in `dir`: files the image holds get its bytes, journal files
    /// it doesn't hold are removed. Cuts now if the power wasn't cut.
    ///
    /// # Errors
    /// Any I/O error writing `dir`.
    pub fn materialize(&self, dir: &Path) -> io::Result<()> {
        let image = {
            let mut disk = lock(&self.disk);
            cut(&mut disk);
            disk.image.clone().unwrap_or_default()
        };
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_file()) && !image.contains_key(&entry.path()) {
                    std::fs::remove_file(entry.path())?;
                }
            }
        }
        std::fs::create_dir_all(dir)?;
        for (path, bytes) in &image {
            std::fs::write(path, bytes)?;
        }
        Ok(())
    }

    /// Counts an operation; `Err` once the power is cut (the cut happening now if this is the chosen operation).
    fn op(&self) -> io::Result<MutexGuard<'_, Disk>> {
        let mut disk = lock(&self.disk);
        disk.ops += 1;
        if disk.cut_at.is_some_and(|at| disk.ops >= at) {
            cut(&mut disk);
        }
        if disk.image.is_some() { Err(cut_error()) } else { Ok(disk) }
    }
}

/// Freezes what the disk holds after a cut: the durable bytes, plus a random half of the dirty sectors, plus one torn
/// sector; a file whose folder entry was never synced is gone.
fn cut(disk: &mut Disk) {
    if disk.image.is_some() {
        return;
    }
    let mut image = BTreeMap::new();
    let mut torn_one = false;
    let paths: Vec<PathBuf> = disk.files.keys().cloned().collect();
    for path in paths {
        let Some(file) = disk.files.get(&path).cloned() else { continue };
        if !file.entry_durable {
            continue;
        }
        let mut bytes = file.durable.clone();
        for &sector in &file.dirty {
            let start = sector * SECTOR;
            if start >= file.volatile.len() {
                continue;
            }
            let end = (start + SECTOR).min(file.volatile.len());
            let keep = match disk.rng.below(3) {
                0 => end - start,
                1 if !torn_one => {
                    torn_one = true;
                    usize::try_from(disk.rng.below((end - start) as u64)).unwrap_or(0)
                }
                _ => 0,
            };
            if keep == 0 {
                continue;
            }
            if bytes.len() < start {
                bytes.resize(start, 0);
            }
            let upto = start + keep;
            if bytes.len() < upto {
                bytes.resize(upto, 0);
            }
            bytes[start..upto].copy_from_slice(&file.volatile[start..upto]);
        }
        image.insert(path, bytes);
    }
    disk.image = Some(image);
}

impl JournalFs for SimFs {
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        drop(self.op()?);
        std::fs::create_dir_all(dir)
    }

    fn create_new(&self, path: &Path) -> io::Result<Box<dyn JournalFile>> {
        let mut disk = self.op()?;
        if disk.files.contains_key(path) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        disk.files.insert(path.to_path_buf(), File::default());
        Ok(Box::new(SimFile { fs: self.clone(), path: path.to_path_buf() }))
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn JournalFile>> {
        let disk = self.op()?;
        if !disk.files.contains_key(path) {
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(Box::new(SimFile { fs: self.clone(), path: path.to_path_buf() }))
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        let mut disk = self.op()?;
        for (path, file) in &mut disk.files {
            if path.parent() == Some(dir) {
                file.entry_durable = true;
            }
        }
        Ok(())
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        let disk = self.op()?;
        Ok(disk
            .files
            .keys()
            .filter(|path| path.parent() == Some(dir))
            .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
            .collect())
    }

    fn read_start(&self, path: &Path, len: usize) -> io::Result<Vec<u8>> {
        let disk = self.op()?;
        let file = disk.files.get(path).ok_or(io::ErrorKind::NotFound)?;
        Ok(file.volatile[..len.min(file.volatile.len())].to_vec())
    }

    fn barrier_is_durable(&self) -> bool {
        true
    }
}

struct SimFile {
    fs: SimFs,
    path: PathBuf,
}

impl SimFile {
    fn with<T>(&self, f: impl FnOnce(&mut File) -> T) -> io::Result<T> {
        let mut disk = self.fs.op()?;
        let file = disk.files.get_mut(&self.path).ok_or(io::ErrorKind::NotFound)?;
        Ok(f(file))
    }
}

impl JournalFile for SimFile {
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let offset = usize::try_from(offset).map_err(io::Error::other)?;
        self.with(|file| {
            let end = offset + bytes.len();
            if file.volatile.len() < end {
                file.volatile.resize(end, 0);
            }
            file.volatile[offset..end].copy_from_slice(bytes);
            file.dirty.extend(offset / SECTOR..end.div_ceil(SECTOR));
        })
    }

    fn flush(&mut self, _how: Flush) -> io::Result<()> {
        self.with(|file| {
            file.durable.clone_from(&file.volatile);
            file.dirty.clear();
        })
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        let len = usize::try_from(len).map_err(io::Error::other)?;
        self.with(|file| {
            if len < file.volatile.len() {
                file.dirty.extend(len / SECTOR..file.volatile.len().div_ceil(SECTOR));
            }
            file.volatile.resize(len, 0);
        })
    }
}
