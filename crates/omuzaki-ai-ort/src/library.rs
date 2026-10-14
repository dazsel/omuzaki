//! Getting ONNX Runtime into the process: fetch the pinned archive through the app's [`FileSource`], unpack the
//! library (and the provider libraries that must sit beside it), and hand its path to `ort` once.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use flate2::read::GzDecoder;
use omuzaki_ai_api::{FileSource, LoadError, NoProgress, Provider};

use crate::builds::{Archive, Build, select};

/// What a process has settled about its ONNX Runtime: the app's provider preference (until something loads) and the
/// library that loaded (final).
struct Process {
    preferred: Vec<Provider>,
    loaded: Option<Loaded>,
}

static PROCESS: Mutex<Process> = Mutex::new(Process { preferred: Vec::new(), loaded: None });

fn lock(process: &Mutex<Process>) -> std::sync::MutexGuard<'_, Process> {
    process.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Sets which providers pick the process's ONNX Runtime flavour, for the app to call at start-up with the person's
/// settings, **before the first probe or load**. ONNX Runtime is loaded once per process and a probe (which wants no
/// provider) would otherwise fix the CPU build for good, so a person who opted in to CUDA would never get it. Returns
/// `false`, and changes nothing, when the library is already loaded (the flavour is then final).
pub fn prefer_providers(providers: &[Provider]) -> bool {
    let mut process = lock(&PROCESS);
    if process.loaded.is_some() {
        return false;
    }
    process.preferred = providers.to_vec();
    true
}

/// The providers that choose the flavour: the app's preference when it set one, else what the first caller wants.
pub(crate) fn flavour_providers(preferred: &[Provider], wanted: &[Provider]) -> Vec<Provider> {
    if preferred.is_empty() { wanted.to_vec() } else { preferred.to_vec() }
}

/// What [`ensure_loaded`] settled on.
#[derive(Clone, Debug)]
pub struct Loaded {
    /// The pinned build whose library was handed to `ort`.
    pub build: &'static Build,
    /// False when another crate in this process (`omuzaki-audio`) had already initialised `ort` with its own copy
    /// first; that copy is the one in use, so `build` is only what we would have loaded.
    pub ours: bool,
}

/// The library this process loaded through this crate, once some probe or load has loaded it.
pub fn loaded() -> Option<Loaded> {
    lock(&PROCESS).loaded.clone()
}

/// Loads ONNX Runtime once per process and returns what was loaded; later calls return the first result whatever
/// `wanted` says (the flavour is fixed by the first load).
///
/// The flavour is picked by the first call: from [`prefer_providers`] if the app set one, else from `wanted`; CUDA when
/// that asks for it and a CUDA build exists for the platform, the platform's default build otherwise ([`select_here`]).
///
/// `ort::init_from` is itself once-only and first-wins: if `omuzaki-audio` (which loads its own pinned copy) initialised
/// it first, this returns `ours: false` and every session in the process uses that library.
///
/// # Errors
/// [`LoadError::Native`] when the archive can't be had, [`LoadError::Runtime`] when it can't be unpacked or loaded or
/// there is no build for this platform.
pub fn ensure_loaded(natives: &dyn FileSource, wanted: &[Provider]) -> Result<Loaded, LoadError> {
    let platform = (std::env::consts::OS, std::env::consts::ARCH);
    ensure_in(&PROCESS, platform, natives, wanted, &|library| {
        Ok(ort::init_from(library).map_err(|e| LoadError::Runtime(e.to_string()))?.with_name("omuzaki").commit())
    })
}

/// [`ensure_loaded`] over an explicit process state, platform and `init` (what hands the unpacked library to `ort` and
/// says whether this call was the one that initialised it). Tests inject a counting `init` so the order of probe and
/// load can be checked without a native library.
fn ensure_in(
    process: &Mutex<Process>,
    (os, arch): (&str, &str),
    natives: &dyn FileSource,
    wanted: &[Provider],
    init: &dyn Fn(&Path) -> Result<bool, LoadError>,
) -> Result<Loaded, LoadError> {
    let mut process = lock(process);
    if let Some(loaded) = &process.loaded {
        return Ok(loaded.clone());
    }
    let no_build = || LoadError::Runtime(format!("there is no pinned ONNX Runtime build for {os}-{arch}"));
    let chosen = flavour_providers(&process.preferred, wanted);
    let preferred = select(os, arch, &chosen).ok_or_else(no_build)?;
    let (build, archive) = match natives.get(&preferred.pin(), &NoProgress) {
        Ok(archive) => (preferred, archive),
        // A flavour the person opted into (CUDA, hundreds of MB) that can't be had, say offline, must not take the CPU
        // path down with it: the default build runs every shipped model.
        Err(error) => {
            let default =
                select(os, arch, &[]).filter(|d| d.id != preferred.id).ok_or(LoadError::Native(error.clone()))?;
            tracing::warn!(wanted = preferred.id, %error, "that ONNX Runtime build could not be had; using the default build");
            (default, natives.get(&default.pin(), &NoProgress)?)
        }
    };
    let dir = unpack(build, &archive).map_err(|e| LoadError::Runtime(format!("unpacking {}: {e}", build.asset)))?;
    let ours = init(&dir.join(build.library))?;
    let loaded = Loaded { build, ours };
    process.loaded = Some(loaded.clone());
    Ok(loaded)
}

/// The folder next to `archive` that holds its unpacked libraries: deterministic, so a second run reuses it.
fn unpack_dir(archive: &Path) -> PathBuf {
    let mut name = archive.as_os_str().to_owned();
    name.push(".lib");
    PathBuf::from(name)
}

/// True for the files of `lib/` worth keeping: the library itself and the provider libraries it loads from its own
/// folder (CUDA's `libonnxruntime_providers_cuda.so` and its shared helper).
fn wanted_file(build: &Build, name: &str) -> bool {
    name == build.library || name.starts_with("libonnxruntime_providers") || name.starts_with("onnxruntime_providers")
}

/// Unpacks the libraries of `archive` (verified by the caller) to [`unpack_dir`] and returns it. Written to a
/// temporary sibling and renamed, so an interrupted unpack never leaves a half-filled folder under the final name.
fn unpack(build: &Build, archive: &Path) -> io::Result<PathBuf> {
    let dest = unpack_dir(archive);
    if dest.join(build.library).is_file() {
        return Ok(dest);
    }
    let parent = dest.parent().unwrap_or(Path::new("."));
    let staging = tempfile::Builder::new().prefix(".ort-unpack-").tempdir_in(parent)?;
    match build.archive {
        Archive::TarGz => unpack_tar(build, archive, staging.path())?,
        Archive::Zip => unpack_zip(build, archive, staging.path())?,
    }
    if !staging.path().join(build.library).is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("`{}` is not in the archive's lib folder", build.library),
        ));
    }
    let staged = staging.keep();
    if let Err(error) = fs::rename(&staged, &dest) {
        // Another process finished first: keep its folder, drop ours.
        let _ = fs::remove_dir_all(&staged);
        if !dest.join(build.library).is_file() {
            return Err(error);
        }
    }
    Ok(dest)
}

/// The name of `path` when it is a direct child of `lib_dir`, else `None`.
fn child_name<'a>(path: &'a Path, lib_dir: &str) -> Option<&'a str> {
    let name = path.file_name()?.to_str()?;
    (path.parent()? == Path::new(lib_dir)).then_some(name)
}

/// The most one unpacked library may be: the biggest real one (CUDA's provider library) is 622 MB.
const MAX_FILE_BYTES: u64 = 1 << 30;

/// Writes `reader` to `out/name`, refusing more than [`MAX_FILE_BYTES`]. `name` is a bare file name that passed
/// [`wanted_file`], never a path taken from the archive. The archive itself is already verified (size and SHA-256) by
/// the [`FileSource`], so this guards against a pin mistake, not an attack.
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

fn unpack_tar(build: &Build, archive: &Path, out: &Path) -> io::Result<()> {
    let lib_dir = build.lib_dir();
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(archive)?));
    for entry in tar.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue; // folders, and symlinks such as libonnxruntime.so -> libonnxruntime.so.1
        }
        let path = entry.path()?.into_owned();
        if let Some(name) = child_name(&path, &lib_dir).filter(|n| wanted_file(build, n)) {
            store(&mut entry, out, name)?;
        }
    }
    Ok(())
}

fn unpack_zip(build: &Build, archive: &Path, out: &Path) -> io::Result<()> {
    let lib_dir = build.lib_dir();
    let mut zip = zip::ZipArchive::new(File::open(archive)?).map_err(io::Error::other)?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(io::Error::other)?;
        if !entry.is_file() {
            continue;
        }
        let Some(path) = entry.enclosed_name() else { continue };
        if let Some(name) = child_name(&path, &lib_dir).filter(|n| wanted_file(build, n)) {
            store(&mut entry, out, name)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    /// Serves a tiny archive per build, remembering which were asked for.
    struct FakeArchives {
        dir: tempfile::TempDir,
        asked: std::sync::Mutex<Vec<&'static str>>,
        missing: Vec<&'static str>,
    }

    impl FileSource for FakeArchives {
        fn get(
            &self,
            pin: &omuzaki_ai_api::PinnedFile,
            _: &dyn omuzaki_ai_api::Progress,
        ) -> Result<PathBuf, omuzaki_ai_api::FetchError> {
            let build = BUILDS.iter().find(|b| pin.urls[0].ends_with(b.asset)).unwrap();
            self.asked.lock().unwrap().push(build.id);
            if self.missing.contains(&build.id) {
                return Err(omuzaki_ai_api::FetchError::Io { path: build.asset.into(), reason: "offline".into() });
            }
            let path = self.dir.path().join(build.id);
            let lib = format!("{}/{}", build.lib_dir(), build.library);
            fs::write(&path, tgz(&[(&lib, b"library")])).unwrap();
            Ok(path)
        }
    }

    fn fake(missing: &[&'static str]) -> FakeArchives {
        FakeArchives {
            dir: tempfile::tempdir().unwrap(),
            asked: std::sync::Mutex::default(),
            missing: missing.to_vec(),
        }
    }

    const LINUX: (&str, &str) = ("linux", "x86_64");

    /// A process with a counting `init`: returns the state, and the ids of the libraries `init` was handed.
    fn run(
        preferred: &[Provider],
        calls: &[&[Provider]],
        natives: &FakeArchives,
    ) -> (Vec<Result<&'static str, String>>, Vec<String>) {
        let process = Mutex::new(Process { preferred: preferred.to_vec(), loaded: None });
        let inits = std::cell::RefCell::new(Vec::new());
        let init = |library: &Path| {
            inits.borrow_mut().push(library.file_name().unwrap().to_string_lossy().into_owned());
            Ok(true)
        };
        let results = calls
            .iter()
            .map(|wanted| {
                ensure_in(&process, LINUX, natives, wanted, &init).map(|l| l.build.id).map_err(|e| e.to_string())
            })
            .collect();
        (results, inits.into_inner())
    }

    #[test]
    fn probe_then_load_with_cuda_settings_initialises_the_cuda_build_exactly_once() {
        use Provider::{Cpu, Cuda};
        let natives = fake(&[]);
        // The app's settings say CUDA; the probe wants nothing; the load asks for [Cuda, Cpu].
        let (results, inits) = run(&[Cuda, Cpu], &[&[], &[Cuda, Cpu]], &natives);
        assert_eq!(results, [Ok("linux-x64-cuda12"), Ok("linux-x64-cuda12")]);
        assert_eq!(inits, ["libonnxruntime.so.1.28.3"], "one initialisation, from the CUDA archive");
        assert_eq!(*natives.asked.lock().unwrap(), ["linux-x64-cuda12"], "the CPU archive was never fetched");
    }

    #[test]
    fn without_settings_the_first_caller_decides_and_a_later_cuda_request_cannot_change_it() {
        use Provider::{Cpu, Cuda};
        let natives = fake(&[]);
        let (results, inits) = run(&[], &[&[], &[Cuda, Cpu]], &natives);
        assert_eq!(results, [Ok("linux-x64"), Ok("linux-x64")], "the probe fixed the CPU build");
        assert_eq!(inits.len(), 1);
        // A load that comes first and asks for CUDA gets it.
        let (results, _) = run(&[], &[&[Cuda, Cpu], &[]], &fake(&[]));
        assert_eq!(results, [Ok("linux-x64-cuda12"), Ok("linux-x64-cuda12")]);
    }

    #[test]
    fn a_cuda_build_that_cannot_be_had_falls_back_to_the_default_build() {
        use Provider::{Cpu, Cuda};
        let natives = fake(&["linux-x64-cuda12"]);
        let (results, inits) = run(&[Cuda, Cpu], &[&[]], &natives);
        assert_eq!(results, [Ok("linux-x64")]);
        assert_eq!(inits.len(), 1);
        assert_eq!(*natives.asked.lock().unwrap(), ["linux-x64-cuda12", "linux-x64"]);
    }

    #[test]
    fn the_apps_preference_beats_what_the_first_caller_wants() {
        use omuzaki_ai_api::Provider::{Cpu, Cuda};
        // A probe wants nothing; the person's settings still decide the flavour.
        assert_eq!(flavour_providers(&[Cuda, Cpu], &[]), [Cuda, Cpu]);
        assert_eq!(flavour_providers(&[], &[Cuda]), [Cuda]);
        assert_eq!(flavour_providers(&[Cpu], &[Cuda]), [Cpu]);
    }

    use super::*;
    use crate::builds::BUILDS;

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

    #[test]
    fn only_the_library_and_its_provider_libraries_are_unpacked() {
        let build = &BUILDS[1]; // linux-x64-cuda12: a main library plus provider libraries
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

        let out = unpack(build, &archive).unwrap();
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
        assert_eq!(unpack(build, &archive).unwrap(), out);
    }

    #[test]
    fn an_archive_without_the_library_is_an_error_and_leaves_nothing_behind() {
        let build = &BUILDS[0];
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("abc");
        fs::write(&archive, tgz(&[("onnxruntime-linux-x64-1.28.3/README.md", b"x")])).unwrap();
        assert!(unpack(build, &archive).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "only the archive remains");
    }

    #[test]
    fn a_windows_zip_unpacks_the_dll() {
        let build = BUILDS.iter().find(|b| b.id == "win-x64").unwrap();
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("onnxruntime-win-x64-1.28.3/lib/onnxruntime.dll", options).unwrap();
        zip.write_all(b"dll").unwrap();
        zip.start_file("onnxruntime-win-x64-1.28.3/lib/onnxruntime.lib", options).unwrap();
        zip.write_all(b"import lib").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("abc");
        fs::write(&archive, zip.finish().unwrap().into_inner()).unwrap();
        let out = unpack(build, &archive).unwrap();
        assert_eq!(fs::read_dir(&out).unwrap().count(), 1);
        assert_eq!(fs::read(out.join("onnxruntime.dll")).unwrap(), b"dll");
    }
}
