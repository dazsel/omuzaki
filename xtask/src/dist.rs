//! `cargo xtask dist`: the Windows zip (`omuzaki.exe`, models, notices) the owner unpacks on Windows (spec v3 §31, §40).
//!
//! The zip is deterministic: entries sorted by name, timestamps fixed at 1980-01-01, one permission mode, deflate level
//! 6, so two runs over the same inputs give identical bytes, on any host. Nothing is downloaded: every input is a local path.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::{ArgGroup, Args};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, System, ZipArchive, ZipWriter};

/// The one target the zip supports.
const TARGET: &str = "x86_64-pc-windows-gnu";
/// The MSVC triple builds the same layout, so `--exe` may come from it.
const TARGET_MSVC: &str = "x86_64-pc-windows-msvc";
/// Folder of the face models inside the zip.
const MODELS_DIR: &str = "models/mediapipe-face";
/// What a models folder must contain besides its `manifest.json`.
const MODEL_FILES: [&str; 4] = [
    "face_detector.onnx",
    "face_landmarks_detector.onnx",
    "face_blendshapes.onnx",
    "geometry_pipeline_metadata_landmarks.binarypb",
];
/// SHA-256 of Microsoft's `onnxruntime-win-x64-1.28.3.zip`, the build `omuzaki-ai-ort` pins (`builds.rs`, register row
/// "ONNX Runtime 1.28.3 win-x64 (CPU)"). `tests/dist.rs` fails when the pin in `builds.rs` moves.
const ORT_ARCHIVE_SHA256: &str = "1d6fab48e85f948436af7c8c971d2c145cf224e2c444755dec894f8b0de11a83";
/// No archive member the packager reads is larger than this, so a hostile archive can't exhaust memory.
const MAX_MEMBER_BYTES: u64 = 1 << 30;

const README: &str = include_str!("../../dist/README.txt");
const MODELS_NOTICE: &str = include_str!("../../dist/notices/MODELS.md");
const APACHE_NOTICE: &str = include_str!("../../dist/notices/Apache-2.0.txt");

/// Why a zip could not be built; every variant is one line, which is what `xtask` prints.
#[derive(Debug, thiserror::Error)]
pub enum DistError {
    /// A file or folder could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// The models folder is not what its manifest says.
    #[error("models: {0}")]
    Models(String),
    /// The ONNX Runtime archive is not the expected one.
    #[error("onnxruntime archive: {0}")]
    Ort(String),
    /// The zip could not be written.
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// An option or helper program is wrong.
    #[error("{0}")]
    Input(String),
}

/// Everything the zip is made of, as local paths.
#[derive(Debug, Clone)]
pub struct DistInputs {
    /// The built `omuzaki.exe`.
    pub exe: PathBuf,
    /// The models folder (with its `manifest.json`), or `None` for `--no-models`.
    pub models: Option<PathBuf>,
    /// Microsoft's `onnxruntime-win-x64` zip, or `None` to leave the runtime out.
    pub ort_archive: Option<PathBuf>,
    /// The third-party notices file.
    pub notices: PathBuf,
    /// The version in the zip's name.
    pub version: String,
}

/// Options of `cargo xtask dist`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("models_source").required(true).args(["models", "no_models"])))]
pub struct DistArgs {
    /// The Windows target to build for.
    #[arg(long, value_name = "TRIPLE", default_value = TARGET)]
    target: String,
    /// A built `omuzaki.exe` (default: `cargo build --release -p omuzaki-cli --target <target>`).
    #[arg(long, value_name = "PATH")]
    exe: Option<PathBuf>,
    /// The models folder: the ONNX files and the `manifest.json` that lists their SHA-256.
    #[arg(long, value_name = "DIR")]
    models: Option<PathBuf>,
    /// Make a zip without models; its README.txt says so.
    #[arg(long)]
    no_models: bool,
    /// Microsoft's `onnxruntime-win-x64` zip at the pinned version; checked against the pinned SHA-256.
    #[arg(long, value_name = "ZIP")]
    ort_archive: Option<PathBuf>,
    /// The third-party notices file (default: run `tools/release/notices.sh`).
    #[arg(long, value_name = "FILE")]
    notices: Option<PathBuf>,
    /// Where the zip goes (default: `target/dist`).
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
}

/// Runs `cargo xtask dist` from the workspace at `root` and returns the zip's path.
///
/// # Errors
/// An unsupported target, a failed build or notices step, an input that can't be read, or any error of [`write_zip`].
pub fn run(root: &Path, args: &DistArgs) -> Result<PathBuf, DistError> {
    if args.target != TARGET && args.target != TARGET_MSVC {
        return Err(DistError::Input(format!("unsupported target {}: use {TARGET}", args.target)));
    }
    let target_dir = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), |dir| root.join(dir));
    if let Some(archive) = &args.ort_archive {
        let actual = sha256_file(archive)?;
        if actual != ORT_ARCHIVE_SHA256 {
            return Err(DistError::Ort(format!("SHA-256 is {actual}, the pinned build is {ORT_ARCHIVE_SHA256}")));
        }
    }
    let exe = match &args.exe {
        Some(exe) => exe.clone(),
        None => build_exe(root, &args.target, &target_dir)?,
    };
    let notices = match &args.notices {
        Some(notices) => notices.clone(),
        None => build_notices(root)?,
    };
    let inputs = DistInputs {
        exe,
        models: args.models.clone(),
        ort_archive: args.ort_archive.clone(),
        notices,
        version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    write_zip(&inputs, &args.out.clone().unwrap_or_else(|| target_dir.join("dist")))
}

/// `cargo build --release -p omuzaki-cli --target <target>`; returns the exe it makes.
fn build_exe(root: &Path, target: &str, target_dir: &Path) -> Result<PathBuf, DistError> {
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo.args(["build", "--locked", "--release", "-p", "omuzaki-cli", "--target", target]);
    // Cargo's progress goes to stderr; stdout stays free for the zip's path.
    let status = cargo.current_dir(root).stdout(Stdio::null()).status().map_err(|e| spawn_error("cargo build", &e))?;
    if !status.success() {
        return Err(DistError::Input(format!("cargo build for {target} failed ({status})")));
    }
    Ok(target_dir.join(target).join("release").join("omuzaki.exe"))
}

/// `tools/release/notices.sh`; returns the file it prints the path of.
fn build_notices(root: &Path) -> Result<PathBuf, DistError> {
    let output = Command::new("bash")
        .arg("tools/release/notices.sh")
        .current_dir(root)
        .output()
        .map_err(|e| spawn_error("tools/release/notices.sh", &e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("no message");
        return Err(DistError::Input(format!("tools/release/notices.sh failed: {}", reason.trim())));
    }
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(root.join(printed))
}

fn spawn_error(command: &str, source: &io::Error) -> DistError {
    DistError::Input(format!("cannot run {command}: {source}"))
}

/// Checks a models folder against its `manifest.json`: every file the manifest lists exists and its SHA-256 matches,
/// and the files the zip ships are among them.
///
/// # Errors
/// A missing or unreadable `manifest.json`, a required model it does not list, a listed file that is missing, or a hash
/// that does not match.
pub fn verify_models(dir: &Path) -> Result<(), DistError> {
    let listed = read_manifest(dir)?;
    for name in MODEL_FILES {
        if !listed.contains_key(name) {
            return Err(DistError::Models(format!("manifest.json does not list {name}")));
        }
    }
    for (name, expected) in &listed {
        let path = dir.join(name);
        if !path.is_file() {
            return Err(DistError::Models(format!(
                "{name} is listed in manifest.json but missing from {}",
                dir.display()
            )));
        }
        let actual = sha256_file(&path)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(DistError::Models(format!("{name}: SHA-256 is {actual}, manifest.json says {expected}")));
        }
    }
    Ok(())
}

/// `manifest.json` of a models folder, parsed.
fn load_manifest(dir: &Path) -> Result<serde_json::Value, DistError> {
    let path = dir.join("manifest.json");
    let text = fs::read_to_string(&path).map_err(|source| DistError::Io { path: path.clone(), source })?;
    serde_json::from_str(&text).map_err(|e| DistError::Models(format!("manifest.json is not JSON: {e}")))
}

/// The `files` table of `manifest.json`: file name to SHA-256. A value is the hash itself or an object with a `sha256`.
fn read_manifest(dir: &Path) -> Result<BTreeMap<String, String>, DistError> {
    let json = load_manifest(dir)?;
    let files = json
        .get("files")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| DistError::Models("manifest.json has no `files` table".to_owned()))?;
    let mut listed = BTreeMap::new();
    for (name, entry) in files {
        // A name is a plain file name: no folders, so nothing escapes the models folder.
        if name.is_empty() || name.contains(['/', '\\']) || name == ".." || name == "." {
            return Err(DistError::Models(format!("manifest.json lists an unsafe name {name:?}")));
        }
        let hash = entry.as_str().or_else(|| entry.get("sha256").and_then(serde_json::Value::as_str));
        let hash = hash.ok_or_else(|| DistError::Models(format!("manifest.json gives no SHA-256 for {name}")))?;
        listed.insert(name.clone(), hash.to_owned());
    }
    Ok(listed)
}

/// The manifest the zip carries: the converter's, cut down to the files the zip ships. The converter also lists the source
/// `.tflite` files, which stay out of the zip, so copying its manifest would leave the unpacked folder failing
/// [`verify_models`]. Every other key (`source_url`, `bundle`, `tools`: the source `.task` hash and the tool versions)
/// and each shipped file's own entry are kept, so the provenance survives.
fn distribution_manifest(dir: &Path) -> Result<Vec<u8>, DistError> {
    let mut manifest = load_manifest(dir)?;
    let listed = manifest.get("files").and_then(serde_json::Value::as_object).cloned().unwrap_or_default();
    let mut shipped = serde_json::Map::new();
    for name in MODEL_FILES {
        let entry = match listed.get(name) {
            Some(entry) if entry.is_object() => entry.clone(),
            Some(hash) => serde_json::json!({ "sha256": hash }),
            None => return Err(DistError::Models(format!("manifest.json does not list {name}"))),
        };
        shipped.insert(name.to_owned(), entry);
    }
    let table =
        manifest.as_object_mut().ok_or_else(|| DistError::Models("manifest.json is not an object".to_owned()))?;
    table.insert("files".to_owned(), serde_json::Value::Object(shipped));
    let mut text = serde_json::to_string_pretty(&manifest).map_err(|e| DistError::Models(e.to_string()))?;
    text.push('\n');
    Ok(text.into_bytes())
}

/// Writes `<out>/omuzaki-<version>-windows-x64.zip` and returns its path. Models and the ORT archive are verified
/// first; nothing is written when a check fails.
///
/// # Errors
/// A model or ORT check fails, an input can't be read, or the zip can't be written.
pub fn write_zip(inputs: &DistInputs, out: &Path) -> Result<PathBuf, DistError> {
    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    entries.insert("omuzaki.exe".to_owned(), read(&inputs.exe)?);
    entries.insert("README.txt".to_owned(), readme(inputs.models.is_some()).into_bytes());
    entries.insert("notices/THIRD-PARTY-NOTICES.txt".to_owned(), read(&inputs.notices)?);
    entries.insert("notices/MODELS.md".to_owned(), MODELS_NOTICE.as_bytes().to_vec());
    entries.insert("notices/Apache-2.0.txt".to_owned(), APACHE_NOTICE.as_bytes().to_vec());
    if let Some(models) = &inputs.models {
        verify_models(models)?;
        for name in MODEL_FILES {
            entries.insert(format!("{MODELS_DIR}/{name}"), read(&models.join(name))?);
        }
        entries.insert(format!("{MODELS_DIR}/manifest.json"), distribution_manifest(models)?);
    }
    if let Some(archive) = &inputs.ort_archive {
        let ort = read_ort(archive)?;
        entries.insert("onnxruntime.dll".to_owned(), ort.dll);
        entries.insert("notices/onnxruntime-LICENSE.txt".to_owned(), ort.license);
        entries.insert("notices/onnxruntime-ThirdPartyNotices.txt".to_owned(), ort.third_party);
    }

    // BTreeMap order is byte order of the names, the "entries sorted" rule.
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6))
        .last_modified_time(DateTime::default())
        // Named, because the zip crate otherwise records the host's own system (DOS on Windows, Unix elsewhere), so the
        // same inputs would give different bytes, and different permission bits on reading, depending on the host.
        .system(System::Unix)
        .unix_permissions(0o644);
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in &entries {
        writer.start_file(name.as_str(), options)?;
        writer.write_all(bytes).map_err(|source| DistError::Io { path: PathBuf::from(name), source })?;
    }
    let bytes = writer.finish()?.into_inner();

    fs::create_dir_all(out).map_err(|source| DistError::Io { path: out.to_owned(), source })?;
    let path = out.join(format!("omuzaki-{}-windows-x64.zip", inputs.version));
    fs::write(&path, bytes).map_err(|source| DistError::Io { path: path.clone(), source })?;
    Ok(path)
}

/// `dist/README.txt` plus a closing section about the models.
fn readme(with_models: bool) -> String {
    let models = if with_models {
        "Models\n------\nmodels/mediapipe-face/ holds the face-tracking models; manifest.json lists the SHA-256 of each file.\n"
    } else {
        "Models\n------\nThis build does NOT include the face models (it was made with --no-models), so models/ is absent.\n\
         Face tracking needs them: copy the mediapipe-face folder to models/mediapipe-face next to omuzaki.exe.\n"
    };
    format!("{}\n{models}", README.trim_end())
}

/// The three members of Microsoft's archive the zip carries.
struct Ort {
    dll: Vec<u8>,
    license: Vec<u8>,
    third_party: Vec<u8>,
}

/// Reads `lib/onnxruntime.dll`, `LICENSE` and `ThirdPartyNotices.txt` from the archive's single top folder.
fn read_ort(path: &Path) -> Result<Ort, DistError> {
    let file = File::open(path).map_err(|source| DistError::Io { path: path.to_owned(), source })?;
    let mut archive = ZipArchive::new(file).map_err(|e| DistError::Ort(format!("not a zip: {e}")))?;
    let (mut dll, mut license, mut third_party) = (None, None, None);
    for index in 0..archive.len() {
        let mut member = archive.by_index(index)?;
        let Some(name) = member.enclosed_name() else { continue };
        let parts: Vec<&OsStr> = name.iter().collect();
        let slot = match parts.as_slice() {
            [_, lib, dll_name] if *lib == "lib" && *dll_name == "onnxruntime.dll" => &mut dll,
            [_, file] if *file == "LICENSE" => &mut license,
            [_, file] if *file == "ThirdPartyNotices.txt" => &mut third_party,
            _ => continue,
        };
        let mut bytes = Vec::new();
        member
            .by_ref()
            .take(MAX_MEMBER_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|source| DistError::Io { path: name.clone(), source })?;
        *slot = Some(bytes);
    }
    let need =
        |found: Option<Vec<u8>>, what: &str| found.ok_or_else(|| DistError::Ort(format!("no {what} in the archive")));
    Ok(Ort {
        dll: need(dll, "lib/onnxruntime.dll")?,
        license: need(license, "LICENSE")?,
        third_party: need(third_party, "ThirdPartyNotices.txt")?,
    })
}

fn read(path: &Path) -> Result<Vec<u8>, DistError> {
    fs::read(path).map_err(|source| DistError::Io { path: path.to_owned(), source })
}

/// Lower-case hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// Lower-case hex SHA-256 of a file, streamed.
fn sha256_file(path: &Path) -> Result<String, DistError> {
    let io_error = |source| DistError::Io { path: path.to_owned(), source };
    let mut file = File::open(path).map_err(io_error)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(io_error)?;
    Ok(hex(&hasher.finalize()))
}
