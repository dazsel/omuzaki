//! Getting LiteRT into the process (design §3.2, §3.6): the verified archive's library is unpacked through
//! `omuzaki-ai-native`, loaded once, its symbols resolved into one table, and its version, schema version and shim ABI
//! checked against the build table before anything else is called.
#![allow(unsafe_code)] // loading a native library and resolving its C functions; each call says why it is sound

use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};

use libloading::Library;
use omuzaki_ai_api::LoadError;
use omuzaki_ai_native::{ArchiveKind, LoadOnce, Unpack};

use crate::builds::{Archive, Build, OMZ_LITERT_ABI, SCHEMA_VERSION, TFLITE_VERSION, select_here};

/// The longest version string read from `TfLiteVersion()`, its NUL included.
pub const MAX_VERSION_BYTES: usize = 64;

/// `TfLiteModel`, `TfLiteInterpreterOptions`, `TfLiteInterpreter`, `TfLiteTensor` and the delegate: opaque here.
pub(crate) type Opaque = c_void;
/// `TfLiteStatus` and `TfLiteType` are C enums, passed as `int`.
pub(crate) type Status = c_int;

/// Declares the table: one field per C function, resolved by its own name.
macro_rules! api {
    ($($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        /// The library's functions (design §3.2), each resolved and non-null. They point into a library that stays
        /// loaded for the life of the process ([`open`] never unloads one it hands out).
        #[derive(Debug)]
        #[allow(non_snake_case)] // the C names, so a field is found by grepping LiteRT's headers
        pub(crate) struct Api {
            $(pub(crate) $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
        }

        impl Api {
            #[allow(non_snake_case)]
            fn resolve(library: &Library) -> Result<Self, LoadError> {
                $(let $name = symbol(library, stringify!($name))?;)*
                Ok(Self { $($name,)* })
            }
        }
    };
}

api! {
    TfLiteVersion: fn() -> *const c_char;
    TfLiteSchemaVersion: fn() -> c_int;
    TfLiteModelCreate: fn(*const c_void, usize) -> *mut Opaque;
    TfLiteModelDelete: fn(*mut Opaque);
    TfLiteInterpreterOptionsCreate: fn() -> *mut Opaque;
    TfLiteInterpreterOptionsDelete: fn(*mut Opaque);
    TfLiteInterpreterOptionsSetNumThreads: fn(*mut Opaque, i32);
    TfLiteInterpreterOptionsAddDelegate: fn(*mut Opaque, *mut Opaque);
    TfLiteInterpreterCreate: fn(*const Opaque, *const Opaque) -> *mut Opaque;
    TfLiteInterpreterDelete: fn(*mut Opaque);
    TfLiteInterpreterAllocateTensors: fn(*mut Opaque) -> Status;
    TfLiteInterpreterInvoke: fn(*mut Opaque) -> Status;
    TfLiteInterpreterResizeInputTensor: fn(*mut Opaque, i32, *const c_int, i32) -> Status;
    TfLiteInterpreterGetInputTensorCount: fn(*const Opaque) -> i32;
    TfLiteInterpreterGetInputTensor: fn(*const Opaque, i32) -> *mut Opaque;
    TfLiteInterpreterGetOutputTensorCount: fn(*const Opaque) -> i32;
    TfLiteInterpreterGetOutputTensor: fn(*const Opaque, i32) -> *const Opaque;
    TfLiteTensorType: fn(*const Opaque) -> Status;
    TfLiteTensorNumDims: fn(*const Opaque) -> i32;
    TfLiteTensorDim: fn(*const Opaque, i32) -> i32;
    TfLiteTensorName: fn(*const Opaque) -> *const c_char;
    TfLiteTensorByteSize: fn(*const Opaque) -> usize;
    TfLiteTensorCopyFromBuffer: fn(*mut Opaque, *const c_void, usize) -> Status;
    TfLiteTensorCopyToBuffer: fn(*const Opaque, *mut c_void, usize) -> Status;
    omz_litert_abi: fn() -> u32;
    omz_litert_xnnpack_create: fn(i32) -> *mut Opaque;
    omz_litert_xnnpack_delete: fn(*mut Opaque);
    omz_litert_capture_errors: fn(*mut Opaque, *mut c_char, usize);
}

/// `name` from `library` as the C function type `F`, refused when it is missing or null.
fn symbol<F: Copy>(library: &Library, name: &str) -> Result<F, LoadError> {
    let missing = || LoadError::Runtime(format!("the LiteRT library has no `{name}`"));
    // SAFETY: each `F` is the signature of `name` in LiteRT's `tflite/core/c/c_api.h` at v2.3.0 or in
    // `tools/litert/omz_litert_shim.c` (the table above); `Option<F>` reads a null address as `None`.
    let symbol = unsafe { library.get::<Option<F>>(name.as_bytes()) }.map_err(|_| missing())?;
    (*symbol).ok_or_else(missing)
}

static PROCESS: LoadOnce<&'static Api, LoadError> = LoadOnce::new();

/// What [`load_archive`] settled on.
#[derive(Clone, Copy, Debug)]
pub struct Loaded {
    /// The build whose library was loaded.
    pub build: &'static Build,
    /// Its function table, which every session keeps.
    pub(crate) api: &'static Api,
}

/// Two results are the same load when they share the one function table a process keeps.
impl PartialEq for Loaded {
    fn eq(&self, other: &Self) -> bool {
        self.build == other.build && std::ptr::eq(self.api, other.api)
    }
}

/// The library this process loaded, once [`load_archive`] has loaded one.
pub fn loaded() -> Option<Loaded> {
    let build = select_here()?;
    PROCESS.get().and_then(Result::ok).map(|api| Loaded { build, api })
}

/// Loads LiteRT from `archive` once per process: the library is unpacked into a folder named by the archive's
/// SHA-256, loaded, every symbol of design §3.2 resolved, and `TfLiteVersion()`, `TfLiteSchemaVersion()` and
/// `omz_litert_abi()` checked. Later calls return the first result, success or error, without reading `archive`.
///
/// The caller must have checked `archive` against `sha256` first: until a published pin exists (#1748) the archive's
/// trust root is the SHA-256 of the CI run that built it (`OMUZAKI_LITERT_SHA256`), not a hash in this repository.
///
/// # Errors
/// [`LoadError::Runtime`] when there is no build for this platform, `sha256` is not 64 hex digits, or the archive
/// can't be unpacked, or the library can't be loaded, lacks a symbol or reports another version, schema or ABI.
pub fn load_archive(archive: &Path, sha256: &str) -> Result<Loaded, LoadError> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let build = select_here().ok_or_else(|| LoadError::Runtime(format!("there is no LiteRT build for {os}-{arch}")))?;
    let api = load_in(&PROCESS, build, archive, sha256, &open)?;
    Ok(Loaded { build, api })
}

/// [`load_archive`] over an explicit once-guard and `open`, so tests can count what is opened without a library.
fn load_in<T: Clone>(
    process: &LoadOnce<T, LoadError>,
    build: &Build,
    archive: &Path,
    sha256: &str,
    open: &dyn Fn(&Path) -> Result<T, LoadError>,
) -> Result<T, LoadError> {
    // Settled first: a later call needs neither the archive nor the folder it was unpacked into.
    if let Some(settled) = process.get() {
        return settled;
    }
    let dir =
        unpack(build, archive, sha256).map_err(|e| LoadError::Runtime(format!("unpacking {}: {e}", build.asset)))?;
    process.get_or_init(build.asset, sha256, || open(&dir.join(build.library)))
}

/// The library of `archive` unpacked under `<archive's folder>/litert-<sha256>/`, so a new archive under the same file
/// name never reuses another build's unpacked library. The archive is copied there first, because the shared
/// unpacker names its folder after the archive's path.
fn unpack(build: &Build, archive: &Path, sha256: &str) -> std::io::Result<PathBuf> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "the SHA-256 is not 64 hex digits"));
    }
    let folder = archive.parent().unwrap_or(Path::new(".")).join(format!("litert-{}", sha256.to_ascii_lowercase()));
    let keyed = folder.join(build.asset);
    if !keyed.is_file() {
        std::fs::create_dir_all(&folder)?;
        let staged = tempfile::NamedTempFile::new_in(&folder)?;
        std::fs::copy(archive, staged.path())?;
        staged.persist(&keyed).map_err(|e| e.error)?;
    }
    let kind = match build.archive {
        Archive::TarGz => ArchiveKind::TarGz,
        Archive::Zip => ArchiveKind::Zip,
    };
    Unpack::new(kind, &build.lib_dir(), build.library).run(&keyed)
}

/// Loads the library at `path`, resolves its table and checks it; the table is kept for the life of the process.
fn open(path: &Path) -> Result<&'static Api, LoadError> {
    // SAFETY: the library is the archive's, checked by the caller against its build's SHA-256; loading it runs only
    // LiteRT's own static initialisers.
    let library =
        unsafe { Library::new(path) }.map_err(|e| LoadError::Runtime(format!("loading {}: {e}", path.display())))?;
    let api = Api::resolve(&library)?;
    // SAFETY: the three functions take nothing; `TfLiteVersion` returns a static string or null, read by `c_text`
    // only up to its NUL and within `MAX_VERSION_BYTES`.
    let (version, schema, abi) = unsafe {
        (c_text((api.TfLiteVersion)(), MAX_VERSION_BYTES), (api.TfLiteSchemaVersion)(), (api.omz_litert_abi)())
    };
    check_library(version.as_deref(), schema, abi)?;
    // The table points into the library, and sessions keep the table for good, so the library is never unloaded.
    Box::leak(Box::new(library));
    Ok(Box::leak(Box::new(api)))
}

/// The bytes of a C string before its NUL, or `None` for null or no NUL within `cap` bytes.
///
/// # Safety
/// `text` is null, or readable up to its NUL or for `cap` bytes, whichever comes first.
pub(crate) unsafe fn c_text(text: *const c_char, cap: usize) -> Option<Vec<u8>> {
    if text.is_null() {
        return None;
    }
    let mut bytes = Vec::new();
    for i in 0..cap {
        // SAFETY: the caller's contract: readable to the NUL or the cap, and the loop stops at either.
        match unsafe { text.add(i).cast::<u8>().read() } {
            0 => return Some(bytes),
            byte => bytes.push(byte),
        }
    }
    None
}

/// Checks what a library reports about itself against the build table: `TfLiteVersion()` (its bytes before the NUL,
/// `None` when unreadable), `TfLiteSchemaVersion()` and `omz_litert_abi()`.
///
/// # Errors
/// [`LoadError::Runtime`] naming the first value that differs.
pub fn check_library(version: Option<&[u8]>, schema: c_int, abi: u32) -> Result<(), LoadError> {
    let version = version.and_then(|v| std::str::from_utf8(v).ok()).ok_or_else(|| {
        LoadError::Runtime(format!("the LiteRT library reports no readable version within {MAX_VERSION_BYTES} bytes"))
    })?;
    let mismatch = |what: &str, got: &dyn std::fmt::Display, want: &dyn std::fmt::Display| {
        Err(LoadError::Runtime(format!("the LiteRT library is {what} {got}; this runtime needs {want}")))
    };
    if version != TFLITE_VERSION {
        return mismatch("version", &version, &TFLITE_VERSION);
    }
    if u32::try_from(schema).ok() != Some(SCHEMA_VERSION) {
        return mismatch("schema version", &schema, &SCHEMA_VERSION);
    }
    if abi != OMZ_LITERT_ABI {
        return mismatch("shim ABI", &abi, &OMZ_LITERT_ABI);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pinned_library_passes_and_any_mismatch_is_refused() {
        let ok = TFLITE_VERSION.as_bytes();
        let refused = |result: Result<(), LoadError>, needle: &str| match result {
            Err(LoadError::Runtime(why)) => assert!(why.contains(needle), "{why}"),
            other => panic!("{other:?}"),
        };
        assert_eq!(check_library(Some(ok), 3, OMZ_LITERT_ABI), Ok(()));
        refused(check_library(Some(b"2.21.0"), 3, OMZ_LITERT_ABI), "version 2.21.0");
        refused(check_library(Some(b"2.22.0-dev0+selfbuilt "), 3, OMZ_LITERT_ABI), "version");
        refused(check_library(None, 3, OMZ_LITERT_ABI), "no readable version");
        refused(check_library(Some(&[0xff, 0xfe]), 3, OMZ_LITERT_ABI), "no readable version");
        refused(check_library(Some(ok), 4, OMZ_LITERT_ABI), "schema version 4");
        refused(check_library(Some(ok), -3, OMZ_LITERT_ABI), "schema version -3");
        refused(check_library(Some(ok), 3, OMZ_LITERT_ABI + 1), "shim ABI 2");
    }

    #[test]
    fn a_version_string_is_read_to_its_nul_and_only_within_the_cap() {
        let read = |text: &[u8]| unsafe { c_text(text.as_ptr().cast(), MAX_VERSION_BYTES) };
        assert_eq!(read(b"2.22.0\0junk"), Some(b"2.22.0".to_vec()));
        assert_eq!(read(b"\0"), Some(Vec::new()));
        let long = [b'x'; MAX_VERSION_BYTES + 1];
        assert_eq!(read(&long), None, "no NUL within the cap");
        assert_eq!(unsafe { c_text(std::ptr::null(), MAX_VERSION_BYTES) }, None);
    }

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// Writes an archive of `build` whose library holds `library`, under the build's asset name in `dir`.
    fn archive(dir: &Path, build: &Build, library: &[u8]) -> PathBuf {
        use std::io::Write as _;
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(library.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, format!("{}/{}", build.lib_dir(), build.library), library).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar.into_inner().unwrap()).unwrap();
        let path = dir.join(build.asset);
        std::fs::write(&path, gz.finish().unwrap()).unwrap();
        path
    }

    /// An `open` that returns the library's bytes and counts its calls.
    fn reading(count: &std::cell::Cell<u32>) -> impl Fn(&Path) -> Result<Vec<u8>, LoadError> + '_ {
        move |path| {
            count.set(count.get() + 1);
            Ok(std::fs::read(path).unwrap())
        }
    }

    #[test]
    fn a_new_archive_under_the_same_name_unpacks_its_own_library() {
        let (dir, build, count) = (tempfile::tempdir().unwrap(), &crate::builds::BUILDS[0], std::cell::Cell::new(0));
        let path = archive(dir.path(), build, b"first build");
        assert_eq!(load_in(&LoadOnce::new(), build, &path, SHA_A, &reading(&count)), Ok(b"first build".to_vec()));
        // A later process: the same file name in the same folder, another build.
        archive(dir.path(), build, b"second build");
        assert_eq!(load_in(&LoadOnce::new(), build, &path, SHA_B, &reading(&count)), Ok(b"second build".to_vec()));
    }

    #[test]
    fn later_calls_return_the_first_result_without_reading_the_archive() {
        let (dir, build, count) = (tempfile::tempdir().unwrap(), &crate::builds::BUILDS[0], std::cell::Cell::new(0));
        let path = archive(dir.path(), build, b"library");
        let process = LoadOnce::new();
        let first = load_in(&process, build, &path, SHA_A, &reading(&count));
        std::fs::remove_dir_all(dir.path()).unwrap();
        assert_eq!(load_in(&process, build, &path, SHA_A, &reading(&count)), first);
        assert_eq!(count.get(), 1, "opened once");
        // A failure is settled the same way.
        let failing = LoadOnce::new();
        let refuse = |_: &Path| -> Result<Vec<u8>, LoadError> { Err(LoadError::Runtime("refused".to_owned())) };
        let dir = tempfile::tempdir().unwrap();
        let path = archive(dir.path(), build, b"library");
        let first = load_in(&failing, build, &path, SHA_A, &refuse);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(load_in(&failing, build, &path, SHA_A, &reading(&count)), first);
        assert_eq!(count.get(), 1, "never opened after the failure");
    }

    #[test]
    fn a_digest_that_is_not_64_hex_digits_is_refused_before_any_path_is_made() {
        let (dir, build, count) = (tempfile::tempdir().unwrap(), &crate::builds::BUILDS[0], std::cell::Cell::new(0));
        let path = archive(dir.path(), build, b"library");
        for digest in ["", "../../etc", &SHA_A[1..], &format!("{}g", &SHA_A[1..])] {
            assert!(matches!(
                load_in(&LoadOnce::new(), build, &path, digest, &reading(&count)),
                Err(LoadError::Runtime(_))
            ));
        }
        assert_eq!(count.get(), 0);
    }

    #[test]
    fn a_library_that_cannot_be_opened_is_a_runtime_error() {
        let dir = std::env::temp_dir().join("omuzaki-ai-litert-no-such-library.so");
        assert!(matches!(open(&dir), Err(LoadError::Runtime(_))));
    }
}
