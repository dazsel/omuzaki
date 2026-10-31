//! Matching a reference in the viewport: the parity target an agent or the person named (`parity.target.set`, under
//! `parity/targets/<name>`) laid over the character, and the count of pixels that still differ.
//!
//! The count is the stop rule's: a pixel is off when any byte of its colour differs from the reference's, after both
//! are composited over the target's backdrop, exactly as `parity.compare` counts `pixels_off` (the same
//! [`level_map`]), so the number the person watches is the number an agent's loop stops on. Parity is reached only at
//! 0. The viewport renders at the reference's own size while matching, so its pixels are the reference's pixels.
//!
//! The reference is only looked at and measured: nothing of it is drawn into the character.

use std::path::{Path, PathBuf};

use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use omuzaki_core::glam::UVec2;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_parity::image::{ImageRef, Rgba8, decode};
use omuzaki_parity::metrics::level_map;
use omuzaki_parity::ops::{Frames, TargetDef};
use omuzaki_parity::overlay::{self, Mode};
use omuzaki_parity::raster::LabImage;
use omuzaki_store_api::ContentHash;

/// Where the parity targets live in the document.
pub fn targets_path() -> DocPath {
    DocPath::root().join("parity").join("targets")
}

/// How the reference shows over the character.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Look {
    /// The reference at half strength over the character.
    #[default]
    Onion,
    /// The character on the left, the reference on the right of a line.
    Wipe,
    /// The exact difference map: black where every byte matches, red where any differs.
    Exact,
}

/// How far the character's picture is from the reference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    /// The pixels compared.
    pub pixels: usize,
    /// The pixels where any byte differs: parity is 0.
    pub off: usize,
    /// The largest byte difference of any channel of any pixel.
    pub max_level: u8,
}

impl Count {
    /// Whether every pixel matches.
    pub const fn done(&self) -> bool {
        self.off == 0
    }
}

/// A target's reference, decoded and ready to measure against.
pub struct Reference {
    /// The target's name.
    pub name: String,
    /// The reference's size in pixels: the viewport renders at it.
    pub size: UVec2,
    /// The backdrop both pictures are composited over.
    pub backdrop: [u8; 3],
    /// The reference as the measures see it.
    pub lab: LabImage,
    /// The node it was read from, to notice when the target changes.
    pub source: Value,
    /// Its texture, made on the first frame it is drawn.
    texture: Option<TextureHandle>,
}

impl std::fmt::Debug for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reference").field("name", &self.name).field("size", &self.size).finish_non_exhaustive()
    }
}

impl Reference {
    /// Reads target `name` from its document node (`parity/targets/<name>`): its first frame, from a file (relative
    /// paths from `project`) or inline data.
    ///
    /// # Errors
    /// In words: the node isn't a target, its first frame can't be read or decoded, or it is a store asset, which the
    /// viewport doesn't read yet.
    pub fn read(name: &str, node: &Value, project: Option<&Path>) -> Result<Self, String> {
        let def: TargetDef = serde_json::from_value(node.clone()).map_err(|e| format!("not a parity target: {e}"))?;
        let first = match &def.reference {
            Frames::List { frames } => frames.first().cloned().ok_or_else(|| "it has no frames".to_owned())?,
            Frames::Pattern { pattern, start, .. } => ImageRef::File { path: numbered(pattern, *start) },
            Frames::One(image) => image.clone(),
        };
        let bytes = match &first {
            ImageRef::File { path } => {
                let path = resolve(path, project);
                #[allow(clippy::disallowed_methods, reason = "the person's own reference, named in their project")]
                std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?
            }
            ImageRef::Data { data } => base64_decode(data)?,
            ImageRef::Asset { .. } => {
                return Err("its frames are store assets; give it a file or inline data for the viewport".to_owned());
            }
        };
        let image = decode(&bytes, ContentHash::of(&bytes), "reference").map_err(|e| e.message)?;
        let backdrop = def.backdrop.as_deref().and_then(omuzaki_parity::colour::parse_hex).unwrap_or([0, 0, 0]);
        Ok(Self::new(name, &image, backdrop, node.clone()))
    }

    /// A reference from decoded pixels.
    pub fn new(name: &str, image: &Rgba8, backdrop: [u8; 3], source: Value) -> Self {
        Self {
            name: name.to_owned(),
            size: UVec2::new(image.width, image.height),
            backdrop,
            lab: LabImage::from_rgba(image, backdrop),
            source,
            texture: None,
        }
    }

    /// Measures `rgba8` (straight sRGB, the reference's size) against the reference; `None` at another size.
    pub fn measure(&self, size: UVec2, rgba8: &[u8]) -> Option<(LabImage, Count)> {
        if size != self.size {
            return None;
        }
        let ours = Rgba8 { width: size.x, height: size.y, pixels: rgba8.to_vec(), hash: ContentHash::of(&[]) };
        let ours = LabImage::from_rgba(&ours, self.backdrop);
        let levels = level_map(&ours, &self.lab);
        let off = levels.v.iter().filter(|d| **d > 0.0).count();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a byte difference")]
        let max_level = levels.v.iter().fold(0.0f32, |m, d| m.max(*d)) as u8;
        Some((ours, Count { pixels: levels.v.len(), off, max_level }))
    }

    /// The reference as a texture.
    pub fn texture(&mut self, ctx: &egui::Context) -> TextureHandle {
        if let Some(texture) = &self.texture {
            return texture.clone();
        }
        let image = colour_image(self.lab.w, self.lab.h, &self.lab.rgb);
        let texture = ctx.load_texture(format!("parity-reference-{}", self.name), image, TextureOptions::NEAREST);
        self.texture = Some(texture.clone());
        texture
    }
}

/// The exact difference map of `ours` against the reference, as `parity.overlay`'s `exact` mode draws it.
pub fn exact_map(ours: &LabImage, reference: &LabImage) -> ColorImage {
    let picture = overlay::draw(&Mode::Exact, ours, reference, (&[], &[]), &[]);
    colour_image(picture.w, picture.h, &picture.rgb)
}

#[allow(clippy::cast_precision_loss, reason = "image sides are far below 2^24")]
fn colour_image(w: usize, h: usize, rgb: &[[u8; 3]]) -> ColorImage {
    let pixels = rgb.iter().map(|[r, g, b]| Color32::from_rgb(*r, *g, *b)).collect();
    ColorImage { size: [w, h], pixels, source_size: egui::vec2(w as f32, h as f32) }
}

/// The largest rect of `size`'s shape inside `within`, centred.
pub fn fit(within: egui::Rect, size: UVec2) -> egui::Rect {
    #[allow(clippy::cast_precision_loss, reason = "image sides are far below 2^24")]
    let aspect = size.x.max(1) as f32 / size.y.max(1) as f32;
    let (w, h) = (within.width(), within.height());
    let fitted = if w / h.max(1.0) > aspect { egui::vec2(h * aspect, h) } else { egui::vec2(w, w / aspect) };
    egui::Rect::from_center_size(within.center(), fitted)
}

/// A target name from a file's name: lower case, letters and digits, dashes between.
pub fn name_for(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut name = String::new();
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else if !name.ends_with('-') && !name.is_empty() {
            name.push('-');
        }
    }
    let name = name.trim_end_matches('-').to_owned();
    if name.is_empty() { "reference".to_owned() } else { name }
}

fn resolve(path: &str, project: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(path);
    match project {
        Some(dir) if path.is_relative() => dir.join(path),
        _ => path,
    }
}

/// `pattern` with its `%d` or `%0Nd` replaced by `n`.
fn numbered(pattern: &str, n: u32) -> String {
    let Some(at) = pattern.find('%') else { return pattern.to_owned() };
    let rest = &pattern[at + 1..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let Some(tail) = rest.get(digits.len()..).and_then(|t| t.strip_prefix('d')) else { return pattern.to_owned() };
    let width = digits.trim_start_matches('0').parse::<usize>().unwrap_or(0);
    format!("{}{n:0width$}{tail}", &pattern[..at])
}

fn base64_decode(data: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let data = data.rsplit_once(',').map_or(data, |(_, d)| d);
    base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|e| format!("its data isn't base64: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Rgba8 {
        let pixels = (0..w * h).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect();
        Rgba8 { width: w, height: h, pixels, hash: ContentHash::of(&[]) }
    }

    #[test]
    fn a_picture_equal_to_the_reference_is_done_and_one_changed_byte_is_one_pixel_off() {
        let reference = Reference::new("grey", &solid(4, 3, [120, 130, 140]), [0, 0, 0], Value::Null);
        let mut ours = solid(4, 3, [120, 130, 140]).pixels;
        let (_, count) = reference.measure(UVec2::new(4, 3), &ours).unwrap_or_else(|| panic!("same size"));
        assert_eq!(count, Count { pixels: 12, off: 0, max_level: 0 });
        assert!(count.done());
        ours[5 * 4 + 1] += 1;
        let (_, count) = reference.measure(UVec2::new(4, 3), &ours).unwrap_or_else(|| panic!("same size"));
        assert_eq!((count.off, count.max_level), (1, 1));
        assert!(reference.measure(UVec2::new(3, 4), &ours).is_none(), "another size isn't measured");
    }

    #[test]
    fn names_patterns_and_fitting() {
        assert_eq!(name_for(Path::new("/refs/Front View (2).PNG")), "front-view-2");
        assert_eq!(name_for(Path::new("___.png")), "reference");
        assert_eq!(numbered("shots/%04d.png", 7), "shots/0007.png");
        assert_eq!(numbered("f%d.png", 12), "f12.png");
        let rect = fit(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0)), UVec2::new(100, 100));
        assert_eq!((rect.width(), rect.height(), rect.center().x), (200.0, 200.0, 200.0));
    }
}
