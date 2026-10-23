//! The manifest the app writes for a user model that came without one (ADR 0016 rule 1), and the checks of a given
//! manifest's pins against the files that were really copied.

use std::collections::BTreeMap;
use std::path::Path;

use omuzaki_ai_api::{ModelCard, ModelFile, ModelId, ModelManifest, RuntimeId, SlotRef, Version};

use super::import::Imported;

/// `user-<slug of the stem>`, made unique with a number while `taken`.
pub(super) fn unique_id(stem: &str, taken: impl Fn(&ModelId) -> bool) -> Option<ModelId> {
    let mut slug = String::new();
    for c in stem.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_alphanumeric() { c } else { '-' };
        if c != '-' || !slug.ends_with('-') {
            slug.push(c);
        }
    }
    let slug = slug.trim_matches('-');
    let base = format!("user-{}", if slug.is_empty() { "model" } else { slug });
    // 64 characters at most, with room for a number.
    let base = &base[..base.len().min(56)];
    (1u32..)
        .map(|n| if n == 1 { base.to_owned() } else { format!("{base}-{n}") })
        .find_map(|id| ModelId::new(id).ok().filter(|id| !taken(id)))
}

/// The runtime that reads this file: `.onnx` is ONNX, and the ggml magic is whisper.cpp's.
pub(super) fn runtime_of(name: &str, head: &[u8]) -> Option<RuntimeId> {
    let id = if Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("onnx")) {
        "onnx"
    } else if head.starts_with(b"lmgg") {
        "whisper-ggml"
    } else {
        return None;
    };
    RuntimeId::new(id).ok()
}

/// The file name with every character a manifest refuses replaced by `_` ("My Voice.onnx" is a fine name on disk).
/// Runtimes find files by this name and the registry finds the bytes by hash, so the original name isn't needed.
fn safe_name(name: &str) -> String {
    let ok = |c: char| c.is_ascii_alphanumeric() || "._+-".contains(c);
    let name: String = name.chars().map(|c| if ok(c) { c } else { '_' }).collect();
    match name.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None => name,
    }
}

/// The manifest for a model that came with none: version `0.0.0`, licence unknown, named after the main file.
pub(super) fn written(id: ModelId, slot: &SlotRef, runtime: RuntimeId, files: &[Imported]) -> ModelManifest {
    let main = files.first().map_or("", |f| f.name.as_str());
    let name = Path::new(main).file_stem().map_or_else(|| main.to_owned(), |s| s.to_string_lossy().into_owned());
    ModelManifest {
        id,
        version: Version::new(0, 0, 0),
        name,
        slot: slot.clone(),
        runtime,
        min_app_version: Version::default(),
        providers: Vec::new(),
        files: files
            .iter()
            .map(|f| ModelFile { name: safe_name(&f.name), sha256: f.sha256, size: f.size, urls: Vec::new() })
            .collect(),
        stages: Vec::new(),
        io: BTreeMap::new(),
        stream: None,
        tts: None,
        voices: Vec::new(),
        card: ModelCard::default(),
    }
}

/// Every way a given manifest's file list disagrees with the files copied, in words.
pub(super) fn pin_problems(manifest: &ModelManifest, files: &[Imported]) -> Vec<String> {
    let mut out = Vec::new();
    for file in &manifest.files {
        match files.iter().find(|f| f.name == file.name) {
            None => out.push(format!("the manifest lists {:?}, which wasn't among the files you added", file.name)),
            Some(real) if real.sha256 != file.sha256 || real.size != file.size => out.push(format!(
                "{:?} isn't the file the manifest pins (it has {} bytes and hash {}; the manifest says {} bytes and \
                 {})",
                file.name, real.size, real.sha256, file.size, file.sha256
            )),
            Some(_) => {}
        }
    }
    for real in files.iter().filter(|r| !manifest.files.iter().any(|f| f.name == r.name)) {
        out.push(format!("{:?} was added but the manifest doesn't list it", real.name));
    }
    out
}

/// The manifest with each file's hash and size set to what was really copied, so the model's digest names the bytes
/// it runs.
pub(super) fn adopt(mut manifest: ModelManifest, files: &[Imported]) -> ModelManifest {
    for file in &mut manifest.files {
        if let Some(real) = files.iter().find(|f| f.name == file.name) {
            (file.sha256, file.size) = (real.sha256, real.size);
        }
    }
    manifest
}
