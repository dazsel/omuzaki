//! Unpacking a pinned native archive, shared by the AI runtimes (ADR 0061 §4).
//!
//! A runtime crate selects its build, has the app's `FileSource` fetch and verify the archive, and then calls
//! [`Unpack::run`] to get the wanted libraries as plain files in a folder beside the archive. The folder is
//! deterministic (a second run reuses it) and appears atomically (an interrupted unpack leaves nothing under the final
//! name). Selecting the build, choosing providers and handing the library to the runtime stay in the runtime's crate.
//!
//! A process loads one native library per runtime, once: [`LoadOnce`] is the guard that runs the runtime's init for the
//! archive's pinned identity exactly once and gives every later caller the same result or error.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use flate2::read::GzDecoder;

/// How an archive is packed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArchiveKind {
    /// A gzip-compressed tar (`.tgz`).
    TarGz,
    /// A zip (`.zip`).
    Zip,
}

/// A once-per-process guard for handing a native library to its runtime.
///
/// The first [`get_or_init`](Self::get_or_init) runs its `init`; the result, success or error, is kept and every later
/// call (from any thread) returns a clone of it without running `init` again. The guard is keyed by the archive's pinned
/// identity (its name and SHA-256), recorded with the first result: a process holds one native library per runtime, so
/// a later call that names another archive still gets the first result, which says what was really loaded.
///
/// Fetching and unpacking belong before the call, so a transient failure there (offline) can be retried; only the
/// init, which a runtime cannot repeat, is guarded.
pub struct LoadOnce<T, E> {
    settled: Mutex<Option<Settled<T, E>>>,
}

struct Settled<T, E> {
    name: String,
    sha256: String,
    result: Result<T, E>,
}

impl<T: Clone, E: Clone> LoadOnce<T, E> {
    /// A guard that has not run yet.
    pub const fn new() -> Self {
        Self { settled: Mutex::new(None) }
    }

    /// The settled result, or `None` before the first [`get_or_init`](Self::get_or_init) has finished.
    pub fn get(&self) -> Option<Result<T, E>> {
        self.lock().as_ref().map(|settled| settled.result.clone())
    }

    /// The pinned identity (name, SHA-256) the settled result was loaded for, or `None` before it settled.
    pub fn identity(&self) -> Option<(String, String)> {
        self.lock().as_ref().map(|settled| (settled.name.clone(), settled.sha256.clone()))
    }

    /// Runs `init` if this is the first call and keeps its result under the identity `name` and `sha256`; otherwise
    /// returns the kept result and drops `init`. Callers racing the first call wait for it and share its result.
    ///
    /// # Errors
    /// The error `init` returned, now and for every later call.
    pub fn get_or_init(&self, name: &str, sha256: &str, init: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let mut slot = self.lock();
        let settled =
            slot.get_or_insert_with(|| Settled { name: name.to_owned(), sha256: sha256.to_owned(), result: init() });
        settled.result.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Settled<T, E>>> {
        self.settled.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl<T: Clone, E: Clone> Default for LoadOnce<T, E> {
    fn default() -> Self {
        Self::new()
    }
}

/// What to take out of one archive. Build it with [`Unpack::new`] and run it with [`Unpack::run`].
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct Unpack<'a> {
    /// How the archive is packed.
    pub kind: ArchiveKind,
    /// The folder inside the archive that holds the libraries, for example `onnxruntime-linux-x64-1.28.3/lib`.
    pub lib_dir: &'a str,
    /// The main library, which must be among the unpacked files; its presence also marks a finished unpack.
    pub library: &'a str,
    /// Whether a bare file name in `lib_dir` is kept. The main library is always kept.
    pub wanted: &'a dyn Fn(&str) -> bool,
}

impl<'a> Unpack<'a> {
    /// An unpack of just `library` from `lib_dir`.
    pub fn new(kind: ArchiveKind, lib_dir: &'a str, library: &'a str) -> Self {
        Self { kind, lib_dir, library, wanted: &|_| false }
    }

    /// Also keeps the files of `lib_dir` that `wanted` accepts (for example provider libraries that sit beside the main
    /// one).
    #[must_use]
    pub fn with_wanted(mut self, wanted: &'a dyn Fn(&str) -> bool) -> Self {
        self.wanted = wanted;
        self
    }

    fn keeps(&self, name: &str) -> bool {
        name == self.library || (self.wanted)(name)
    }

    /// Unpacks the wanted files of `archive` (already verified by the caller) to [`unpack_dir`] and returns that
    /// folder. Written to a temporary sibling and renamed, so an interrupted unpack never leaves a half-filled folder
    /// under the final name; a folder that already holds the main library is reused as it is.
    ///
    /// # Errors
    /// An I/O error when the archive can't be read or the folder can't be written, [`io::ErrorKind::NotFound`] when the
    /// archive has no main library in `lib_dir`, and [`io::ErrorKind::InvalidData`] when a file is over the size cap.
    pub fn run(&self, archive: &Path) -> io::Result<PathBuf> {
        let dest = unpack_dir(archive);
        if dest.join(self.library).is_file() {
            return Ok(dest);
        }
        let parent = dest.parent().unwrap_or(Path::new("."));
        let staging = tempfile::Builder::new().prefix(".unpack-").tempdir_in(parent)?;
        match self.kind {
            ArchiveKind::TarGz => self.unpack_tar(archive, staging.path())?,
            ArchiveKind::Zip => self.unpack_zip(archive, staging.path())?,
        }
        if !staging.path().join(self.library).is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("`{}` is not in the archive's lib folder", self.library),
            ));
        }
        let staged = staging.keep();
        if let Err(error) = fs::rename(&staged, &dest) {
            // Another process finished first: keep its folder, drop ours.
            let _ = fs::remove_dir_all(&staged);
            if !dest.join(self.library).is_file() {
                return Err(error);
            }
        }
        Ok(dest)
    }

    fn unpack_tar(&self, archive: &Path, out: &Path) -> io::Result<()> {
        let mut tar = tar::Archive::new(GzDecoder::new(File::open(archive)?));
        for entry in tar.entries()? {
            let mut entry = entry?;
            if !entry.header().entry_type().is_file() {
                continue; // folders, and symlinks such as libonnxruntime.so -> libonnxruntime.so.1
            }
            let path = entry.path()?.into_owned();
            if let Some(name) = child_name(&path, self.lib_dir).filter(|n| self.keeps(n)) {
                store(&mut entry, out, name)?;
            }
        }
        Ok(())
    }

    fn unpack_zip(&self, archive: &Path, out: &Path) -> io::Result<()> {
        let mut zip = zip::ZipArchive::new(File::open(archive)?).map_err(io::Error::other)?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(io::Error::other)?;
            if !entry.is_file() {
                continue;
            }
            let Some(path) = entry.enclosed_name() else { continue };
            if let Some(name) = child_name(&path, self.lib_dir).filter(|n| self.keeps(n)) {
                store(&mut entry, out, name)?;
            }
        }
        Ok(())
    }
}

/// The folder next to `archive` that holds its unpacked libraries: deterministic, so a second run reuses it.
pub fn unpack_dir(archive: &Path) -> PathBuf {
    let mut name = archive.as_os_str().to_owned();
    name.push(".lib");
    PathBuf::from(name)
}

/// The name of `path` when it is a direct child of `lib_dir`, else `None`.
fn child_name<'a>(path: &'a Path, lib_dir: &str) -> Option<&'a str> {
    let name = path.file_name()?.to_str()?;
    (path.parent()? == Path::new(lib_dir)).then_some(name)
}

/// The most one unpacked library may be: the biggest real one (CUDA's provider library) is 622 MB.
const MAX_FILE_BYTES: u64 = 1 << 30;

/// Writes `reader` to `out/name`, refusing more than [`MAX_FILE_BYTES`]. `name` is a bare file name that passed the
/// `wanted` check, never a path taken from the archive. The archive itself is already verified (size and SHA-256) by
/// the `FileSource`, so this guards against a pin mistake, not an attack.
fn store(reader: impl Read, out: &Path, name: &str) -> io::Result<()> {
    let mut file = File::create(out.join(name))?;
    let copied = io::copy(&mut reader.take(MAX_FILE_BYTES + 1), &mut file)?;
    if copied > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("`{name}` is larger than {MAX_FILE_BYTES} bytes"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, path, *data).unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar.into_inner().unwrap()).unwrap();
        gz.finish().unwrap()
    }

    fn providers(name: &str) -> bool {
        name.starts_with("libonnxruntime_providers") || name.starts_with("onnxruntime_providers")
    }

    #[test]
    fn two_threads_loading_the_same_archive_run_init_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let once: LoadOnce<u32, String> = LoadOnce::new();
        let inits = AtomicUsize::new(0);
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        once.get_or_init("lib.tgz", "abc", || {
                            inits.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            Ok(7)
                        })
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(results, [Ok(7), Ok(7)]);
        assert_eq!(inits.load(Ordering::SeqCst), 1);
        assert_eq!(once.identity(), Some(("lib.tgz".into(), "abc".into())));
    }

    #[test]
    fn a_failed_init_is_reported_to_every_caller_and_not_run_again() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let once: LoadOnce<u32, String> = LoadOnce::new();
        assert_eq!(once.get(), None);
        let inits = AtomicUsize::new(0);
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        once.get_or_init("lib.tgz", "abc", || {
                            inits.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            Err("no such symbol".to_owned())
                        })
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(results, [Err("no such symbol".to_owned()), Err("no such symbol".to_owned())]);
        // A later caller, even naming another archive, gets the same error and does not run its init.
        assert_eq!(once.get_or_init("other.tgz", "def", || Ok(1)), Err("no such symbol".to_owned()));
        assert_eq!(inits.load(Ordering::SeqCst), 1);
        assert_eq!(once.identity(), Some(("lib.tgz".into(), "abc".into())));
    }

    #[test]
    fn only_the_library_and_its_wanted_files_are_unpacked() {
        let lib = |name: &str| format!("onnxruntime-linux-x64-gpu_cuda12-1.28.3/lib/{name}");
        let bytes = tgz(&[
            (&lib("libonnxruntime.so.1.28.3"), b"main"),
            (&lib("libonnxruntime_providers_cuda.so"), b"cuda"),
            (&lib("libonnxruntime_providers_shared.so"), b"shared"),
            (&lib("pkgconfig/libonnxruntime.pc"), b"nope"),
            ("onnxruntime-linux-x64-gpu_cuda12-1.28.3/include/onnxruntime_c_api.h", b"nope"),
            ("elsewhere/lib/libonnxruntime.so.1.28.3", b"nope"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("abc");
        fs::write(&archive, bytes).unwrap();
        let unpack =
            Unpack::new(ArchiveKind::TarGz, "onnxruntime-linux-x64-gpu_cuda12-1.28.3/lib", "libonnxruntime.so.1.28.3")
                .with_wanted(&providers);

        let out = unpack.run(&archive).unwrap();
        assert_eq!(out, dir.path().join("abc.lib"));
        let mut names: Vec<String> =
            fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(
            names,
            ["libonnxruntime.so.1.28.3", "libonnxruntime_providers_cuda.so", "libonnxruntime_providers_shared.so"]
        );
        assert_eq!(fs::read(out.join("libonnxruntime.so.1.28.3")).unwrap(), b"main");
        // A second call reuses the folder instead of unpacking again.
        fs::write(&archive, b"gone").unwrap();
        assert_eq!(unpack.run(&archive).unwrap(), out);
    }

    #[test]
    fn an_archive_without_the_library_is_an_error_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("abc");
        fs::write(&archive, tgz(&[("onnxruntime-linux-x64-1.28.3/README.md", b"x")])).unwrap();
        let unpack = Unpack::new(ArchiveKind::TarGz, "onnxruntime-linux-x64-1.28.3/lib", "libonnxruntime.so.1.28.3");
        assert_eq!(unpack.run(&archive).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "only the archive remains");
    }

    #[test]
    fn a_zip_unpacks_the_dll_and_not_the_import_library() {
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("onnxruntime-win-x64-1.28.3/lib/onnxruntime.dll", options).unwrap();
        zip.write_all(b"dll").unwrap();
        zip.start_file("onnxruntime-win-x64-1.28.3/lib/onnxruntime.lib", options).unwrap();
        zip.write_all(b"import lib").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("abc");
        fs::write(&archive, zip.finish().unwrap().into_inner()).unwrap();
        let unpack = Unpack::new(ArchiveKind::Zip, "onnxruntime-win-x64-1.28.3/lib", "onnxruntime.dll");
        let out = unpack.run(&archive).unwrap();
        assert_eq!(fs::read_dir(&out).unwrap().count(), 1);
        assert_eq!(fs::read(out.join("onnxruntime.dll")).unwrap(), b"dll");
    }
}
