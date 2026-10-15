//! Unpacking a pinned native archive, shared by the AI runtimes (ADR 0061 §4).
//!
//! A runtime crate selects its build, has the app's `FileSource` fetch and verify the archive, and then calls
//! [`Unpack::run`] to get the wanted libraries as plain files in a folder beside the archive. The folder is
//! deterministic (a second run reuses it) and appears atomically (an interrupted unpack leaves nothing under the final
//! name). Selecting the build, choosing providers and handing the library to the runtime stay in the runtime's crate.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

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
