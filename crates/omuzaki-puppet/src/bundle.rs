//! A puppet on disk: a folder (`<name>.puppet/`) holding `puppet.json` (the [`PuppetDef`]), `mapping.json` (its
//! [`ParamMapping`]) and `textures/<n>.png` (straight-alpha PNGs, named by the def). Plain files, so a person can look
//! inside and an agent can diff it.

use std::path::Path;

use omuzaki_puppet_api::{ParamMapping, PuppetDef};

use crate::texture::Texture;

/// The puppet file in a bundle.
pub const PUPPET_FILE: &str = "puppet.json";
/// The mapping file in a bundle.
pub const MAPPING_FILE: &str = "mapping.json";
/// The largest texture side a bundle may hold.
pub const MAX_TEXTURE_SIDE: u32 = 16_384;

/// A loaded puppet: its definition, mapping and textures.
#[derive(Clone, Debug)]
pub struct Bundle {
    /// The puppet.
    pub def: PuppetDef,
    /// Its mapping (empty when the bundle has none).
    pub mapping: ParamMapping,
    /// Its textures, in the def's order.
    pub textures: Vec<Texture>,
}

/// Why a bundle didn't save or load.
#[derive(Debug, thiserror::Error)]
#[error("puppet bundle {path}: {detail}")]
pub struct BundleError {
    /// The file at fault.
    pub path: String,
    /// What went wrong.
    pub detail: String,
}

fn err(path: &Path, e: impl std::fmt::Display) -> BundleError {
    BundleError { path: path.display().to_string(), detail: e.to_string() }
}

/// Writes a bundle into `dir` (created if missing): the def, the mapping and each texture from `images` (straight
/// RGBA8 with its size, in the def's texture order).
///
/// # Errors
/// [`BundleError`] when a file can't be written or an image doesn't match its size.
pub fn save(
    dir: &Path,
    def: &PuppetDef,
    mapping: &ParamMapping,
    images: &[([u32; 2], Vec<u8>)],
) -> Result<(), BundleError> {
    std::fs::create_dir_all(dir.join("textures")).map_err(|e| err(dir, e))?;
    let json = serde_json::to_vec_pretty(def).map_err(|e| err(dir, e))?;
    std::fs::write(dir.join(PUPPET_FILE), json).map_err(|e| err(&dir.join(PUPPET_FILE), e))?;
    let json = serde_json::to_vec_pretty(mapping).map_err(|e| err(dir, e))?;
    std::fs::write(dir.join(MAPPING_FILE), json).map_err(|e| err(&dir.join(MAPPING_FILE), e))?;
    for (t, (size, px)) in def.textures.iter().zip(images) {
        let path = dir.join(&t.file);
        let img = image::RgbaImage::from_raw(size[0], size[1], px.clone())
            .ok_or_else(|| err(&path, "pixels don't match the size"))?;
        img.save_with_format(&path, image::ImageFormat::Png).map_err(|e| err(&path, e))?;
    }
    Ok(())
}

/// Loads the bundle in `dir`, checking the puppet and every texture against the def.
///
/// # Errors
/// [`BundleError`] for a missing or malformed file, a texture of the wrong size, or a puppet that fails its checks.
pub fn load(dir: &Path) -> Result<Bundle, BundleError> {
    load_with(|file| match std::fs::read(dir.join(file)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && file == MAPPING_FILE => Ok(None),
        Err(e) => Err(e.to_string()),
    })
}

/// Loads a bundle through `read`, which gives the bytes of a file of the bundle by its path inside it (`puppet.json`,
/// `textures/0.png`), or `None` when it isn't there; so a host reads the files through its own file access. Checks
/// as [`load`].
///
/// # Errors
/// As [`load`].
pub fn load_with(mut read: impl FnMut(&str) -> Result<Option<Vec<u8>>, String>) -> Result<Bundle, BundleError> {
    let named = |file: &str, detail: String| BundleError { path: file.to_owned(), detail };
    let bytes =
        read(PUPPET_FILE).map_err(|e| named(PUPPET_FILE, e))?.ok_or_else(|| named(PUPPET_FILE, "missing".into()))?;
    let def: PuppetDef = serde_json::from_slice(&bytes).map_err(|e| named(PUPPET_FILE, e.to_string()))?;
    def.check().map_err(|e| named(PUPPET_FILE, e.to_string()))?;
    let mapping = match read(MAPPING_FILE).map_err(|e| named(MAPPING_FILE, e))? {
        Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| named(MAPPING_FILE, e.to_string()))?,
        None => ParamMapping::default(),
    };
    let mut textures = Vec::with_capacity(def.textures.len());
    for t in &def.textures {
        if t.file.contains("..") || Path::new(&t.file).is_absolute() || t.file.starts_with(['/', '\\']) {
            return Err(named(&t.file, "the texture leaves the bundle".to_owned()));
        }
        if t.size[0] > MAX_TEXTURE_SIDE || t.size[1] > MAX_TEXTURE_SIDE {
            return Err(named(&t.file, format!("larger than {MAX_TEXTURE_SIDE}")));
        }
        let bytes = read(&t.file).map_err(|e| named(&t.file, e))?.ok_or_else(|| named(&t.file, "missing".into()))?;
        let img = image::load_from_memory(&bytes).map_err(|e| named(&t.file, e.to_string()))?.into_rgba8();
        if [img.width(), img.height()] != t.size {
            return Err(named(
                &t.file,
                format!("{}x{} where the puppet says {}x{}", img.width(), img.height(), t.size[0], t.size[1]),
            ));
        }
        let tex = Texture::from_straight(img.width(), img.height(), img.as_raw())
            .ok_or_else(|| named(&t.file, "an empty texture".into()))?;
        textures.push(tex);
    }
    Ok(Bundle { def, mapping, textures })
}

impl Bundle {
    /// The bundle of a freshly rigged puppet, its textures made from its images.
    #[must_use]
    pub fn from_rigged(rigged: crate::autorig::Rigged) -> Self {
        let textures = rigged.textures();
        Self { def: rigged.def, mapping: rigged.mapping, textures }
    }
}
