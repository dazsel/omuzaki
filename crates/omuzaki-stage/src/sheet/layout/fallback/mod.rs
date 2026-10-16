//! Fallback fonts for the sheet text: the faces that draw what the embedded Latin font cannot.
//!
//! The sheet's own font is a small Latin subset. A name in Japanese, Chinese, Korean, Arabic, Hindi or Thai, or an
//! emoji, would draw the missing-glyph box in it. Instead the layout asks a [`FontConfig`] for a Noto family that
//! covers the character, loaded the first time it is needed:
//!
//! - [`script`] says which families may draw a character and which regional forms of Han the sheet's language wants;
//! - [`catalog`] pins each font file by URL, size and SHA-256 (nothing is committed; the CJK files are 4-8 MB);
//! - [`source`] reads them through the shared verified cache (`omuzaki_ai`), or from a directory.
//!
//! A sheet without a font source keeps working: its characters are reported by
//! [`SheetLayout::font_report`](super::SheetLayout::font_report) as missing so export can warn, never silently drawn
//! as boxes.

mod catalog;
mod coverage;
mod script;
mod source;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use ab_glyph::{Font as _, FontVec};

pub use catalog::{CATALOG, Family, FontSpec};
pub use script::HanVariant;
pub use source::{DirSource, FontError, FontSource, PinnedFonts};

use super::text::Weight;
use script::{Class, classify};

pub(in crate::sheet::layout) use script::{is_default_ignorable, is_neutral};

/// What a sheet may use beyond its embedded font: the language that picks the Han forms, and where fallback fonts
/// come from.
#[derive(Clone)]
pub struct FontConfig {
    han: HanVariant,
    source: Option<Arc<dyn FontSource>>,
}

impl FontConfig {
    /// The embedded Latin font only. Other scripts are reported as missing.
    pub fn latin_only() -> Self {
        Self { han: HanVariant::default(), source: None }
    }

    /// Fallback fonts from `source`, drawing shared Han ideographs in the forms of `han`.
    pub fn new(han: HanVariant, source: Arc<dyn FontSource>) -> Self {
        Self { han, source: Some(source) }
    }

    /// The Han forms the sheet draws.
    pub const fn han(&self) -> HanVariant {
        self.han
    }
}

impl Default for FontConfig {
    fn default() -> Self {
        Self::latin_only()
    }
}

/// A loaded fallback font: outlines for drawing and a shaper for joining and mark placement.
pub(super) struct Face {
    /// Rasterises glyph ids.
    pub(super) outlines: FontVec,
    /// Applies the OpenType layout (Arabic joining, Devanagari conjuncts, Thai marks).
    pub(super) shaper: harfrust::Font,
    /// Whether letters of this family join, in which case letter-spacing would break them apart.
    pub(super) connected: bool,
    spec: &'static FontSpec,
    sha256: String,
}

impl Face {
    fn parse(spec: &'static FontSpec, bytes: Vec<u8>) -> Result<Self, FontError> {
        let parse_error = |reason: String| FontError::Parse { file: spec.file_name.to_owned(), reason };
        let sha256 = source::hash_of(&bytes).to_string();
        let outlines = FontVec::try_from_vec(bytes.clone()).map_err(|e| parse_error(e.to_string()))?;
        let shared: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(bytes);
        let shaper = harfrust::Font::new(shared, 0).ok_or_else(|| parse_error("not an OpenType font".into()))?;
        let connected = matches!(spec.family, Family::Arabic | Family::Devanagari);
        Ok(Self { outlines, shaper, connected, spec, sha256 })
    }

    /// Whether this font has a glyph for `c`.
    pub(super) fn covers(&self, c: char) -> bool {
        self.outlines.glyph_id(c).0 != 0
    }
}

/// A font that was loaded for a sheet, with the hash of the bytes actually used (provenance, deterministic selection).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceUsed {
    /// The family.
    pub family: Family,
    /// The file name.
    pub file: &'static str,
    /// SHA-256 of the loaded bytes.
    pub sha256: String,
}

/// A character the sheet could not draw with any font.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingGlyph {
    /// The character; the sheet drew the missing-glyph box in its place.
    pub character: char,
    /// Why no font drew it.
    pub reason: String,
    /// Whether a font that could draw it exists but could not be loaded (offline, say), so connecting would help. `false`
    /// for a character no catalog font covers.
    pub downloadable: bool,
}

/// Which fonts a sheet used and which characters it could not draw. Meant for the sheet's provenance and for export to
/// warn before a box reaches a printed sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontReport {
    /// The Han forms the sheet was set to.
    pub han: HanVariant,
    /// The fallback fonts loaded so far, in catalog order.
    pub faces: Vec<FaceUsed>,
    /// The characters no font could draw, in code point order.
    pub missing: Vec<MissingGlyph>,
}

/// One font file, loaded on first use. Failure is remembered so a missing download is not retried per character.
struct Slot {
    spec: &'static FontSpec,
    face: OnceLock<Result<Arc<Face>, FontError>>,
}

/// The lazily loaded fallback faces of one sheet.
pub(super) struct Fallback {
    config: FontConfig,
    slots: Vec<Slot>,
    missing: Mutex<BTreeMap<char, (String, bool)>>,
}

impl Fallback {
    pub(super) fn new(config: FontConfig) -> Self {
        let slots = CATALOG.iter().map(|spec| Slot { spec, face: OnceLock::new() }).collect();
        Self { config, slots, missing: Mutex::new(BTreeMap::new()) }
    }

    /// The loaded face for `family` at `weight`, loading it if this is the first call.
    fn face(&self, family: Family, weight: Weight) -> Result<&Arc<Face>, FontError> {
        let index = catalog::index_of(family, weight).ok_or(FontError::NoSource)?;
        let slot = &self.slots[index];
        let loaded = slot.face.get_or_init(|| {
            let source = self.config.source.as_ref().ok_or(FontError::NoSource)?;
            let bytes = source.load(slot.spec)?;
            Face::parse(slot.spec, bytes).map(Arc::new)
        });
        loaded.as_ref().map_err(Clone::clone)
    }

    /// The fallback face that draws `c` at `weight`, trying the script's families in order. `None` records `c` as
    /// missing, with the reason a font could not be had.
    pub(super) fn face_for(&self, c: char, weight: Weight) -> Option<Arc<Face>> {
        let Class::Script(families) = classify(c, self.config.han) else {
            self.note_missing(c, "its script is not covered by any fallback font".to_owned(), false);
            return None;
        };
        // The coverage table names the one font that can draw `c`, so the others are never downloaded to ask.
        let mut reason = "no fallback font has a glyph for it".to_owned();
        let mut downloadable = false;
        for family in families.into_iter().filter(|f| coverage::covers(*f, c)) {
            match self.face(family, weight) {
                Ok(face) if face.covers(c) => return Some(Arc::clone(face)),
                Ok(_) => {}
                Err(e) => {
                    reason = format!("the {family:?} font could not be loaded: {e}");
                    downloadable = true;
                }
            }
        }
        self.note_missing(c, reason, downloadable);
        None
    }

    /// Remembers a character no font drew. The first reason wins, so the report is stable.
    pub(super) fn note_missing(&self, c: char, reason: String, downloadable: bool) {
        self.missing.lock().unwrap_or_else(PoisonError::into_inner).entry(c).or_insert((reason, downloadable));
    }

    pub(super) fn report(&self) -> FontReport {
        let faces = self
            .slots
            .iter()
            .filter_map(|s| s.face.get()?.as_ref().ok())
            .map(|f| FaceUsed { family: f.spec.family, file: f.spec.file_name, sha256: f.sha256.clone() })
            .collect();
        let missing = self.missing.lock().unwrap_or_else(PoisonError::into_inner);
        let missing = missing.iter().map(|(c, (r, d))| MissingGlyph { character: *c, reason: r.clone(), downloadable: *d }).collect();
        FontReport { han: self.config.han, faces, missing }
    }
}
