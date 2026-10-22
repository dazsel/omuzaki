//! The Theatre's pictures at their own size: a step's picture from History's thumbnailer, or an agent's render from the
//! store (a gate's evidence, a reference sheet), decoded once and kept as a texture with its pixels, so the compare
//! strip can say how much two of them differ.
//!
//! Like History's [`Atlas`](crate::shell::pictures::Atlas), it uploads at most [`UPLOADS_PER_FRAME`] pictures a frame
//! and keeps at most [`KEPT`], the oldest going first; a picture that won't load is tried once.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use omuzaki_ops_api::TxId;
use omuzaki_store_api::{ContentHash, Store};

use crate::shell::pictures::HistoryPictures;

/// The most pictures decoded and uploaded in one frame.
pub const UPLOADS_PER_FRAME: usize = 2;
/// The most pictures kept.
pub const KEPT: usize = 48;
/// The widest or tallest picture kept, in pixels: an agent's 4096-pixel render is shown, not kept, at full size.
pub const MAX_SIDE: u32 = 1024;
/// What a render's URI starts with (`omuzaki://render/<hash>.png`).
pub const RENDER_URI: &str = "omuzaki://render/";
/// What a step's picture is named by in the book's reference (`step:<tx>`).
pub const STEP_REF: &str = "step:";
/// A channel difference above this is a changed pixel: what a PNG round trip and the thumbnailer's antialiasing never
/// reach on an unchanged character.
pub const THRESHOLD: u8 = 16;

/// Where a picture comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// The picture History's thumbnailer took right after a step.
    Step(TxId),
    /// A render in the store, by its content hash.
    Render(ContentHash),
}

impl Source {
    /// The source a reference names: `step:<tx>`, a render URI, or a bare content hash.
    pub fn parse(reference: &str) -> Option<Self> {
        if let Some(tx) = reference.strip_prefix(STEP_REF) {
            return tx.parse().ok().map(|tx| Self::Step(TxId(tx)));
        }
        let hex = reference.strip_prefix(RENDER_URI).and_then(|r| r.strip_suffix(".png")).unwrap_or(reference);
        hex.parse().ok().map(Self::Render)
    }

    /// The reference that names it, as the book keeps it.
    pub fn reference(&self) -> String {
        match self {
            Self::Step(tx) => format!("{STEP_REF}{}", tx.0),
            Self::Render(hash) => format!("{RENDER_URI}{hash}.png"),
        }
    }
}

/// A loaded picture: its texture, and its pixels for comparing.
#[derive(Clone)]
pub struct Picture {
    /// The texture to draw.
    pub texture: TextureHandle,
    /// Straight-alpha RGBA, row by row.
    pub rgba: Arc<Vec<u8>>,
    /// Width and height in pixels.
    pub size: [usize; 2],
}

/// The pictures loaded so far.
#[derive(Default)]
pub struct Pictures {
    loaded: HashMap<Source, Option<Picture>>,
    order: VecDeque<Source>,
    frame: u64,
    uploads: usize,
}

impl std::fmt::Debug for Pictures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pictures").field("kept", &self.loaded.len()).finish_non_exhaustive()
    }
}

/// Where the Theatre's pictures are read from.
#[derive(Clone, Copy)]
pub struct Sources<'a> {
    /// History's pictures, when the app takes them.
    pub steps: Option<&'a HistoryPictures>,
    /// The render store, when the app has one.
    pub renders: Option<&'a Arc<dyn Store>>,
}

impl Pictures {
    /// The picture `source` names, once it is loaded; it loads this frame when the frame's uploads allow. A picture
    /// not there yet asks for a repaint, so it shows once it lands.
    pub fn get(&mut self, ctx: &Context, sources: Sources<'_>, source: &Source) -> Option<Picture> {
        if let Some(picture) = self.loaded.get(source) {
            return picture.clone();
        }
        let frame = ctx.cumulative_frame_nr();
        if self.frame != frame {
            (self.frame, self.uploads) = (frame, 0);
        }
        if self.uploads >= UPLOADS_PER_FRAME {
            ctx.request_repaint();
            return None;
        }
        let png = match source {
            Source::Step(tx) => sources.steps.and_then(|steps| steps.png(*tx)),
            Source::Render(hash) => sources.renders.map(|store| store.get(hash).map_err(|e| e.to_string())),
        };
        let Some(png) = png else {
            // Not taken yet: a step's picture lands a moment after the step.
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
            return None;
        };
        self.uploads += 1;
        let picture = match png.and_then(|png| decode(&png)) {
            Ok((image, rgba)) => {
                let size = image.size;
                let name = format!("theatre-picture-{}", source.reference());
                Some(Picture { texture: ctx.load_texture(name, image, TextureOptions::LINEAR), rgba, size })
            }
            Err(error) => {
                tracing::warn!(%error, source = %source.reference(), "a Theatre picture did not load");
                None
            }
        };
        self.loaded.insert(source.clone(), picture.clone());
        self.order.push_back(source.clone());
        while self.order.len() > KEPT {
            if let Some(oldest) = self.order.pop_front() {
                self.loaded.remove(&oldest);
            }
        }
        picture
    }
}

/// `png` as an image no larger than [`MAX_SIDE`], and its pixels.
fn decode(png: &[u8]) -> Result<(ColorImage, Arc<Vec<u8>>), String> {
    let picture = image::load_from_memory_with_format(png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    let picture = if picture.width() > MAX_SIDE || picture.height() > MAX_SIDE {
        picture.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Triangle)
    } else {
        picture
    };
    let rgba = picture.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    Ok((image, Arc::new(rgba.into_raw())))
}

/// The share of pixels that differ between `a` and `b` (0 to 1), compared premultiplied by alpha as they composite, so
/// a colour change under no coverage is no change; `None` when they are not the same size.
pub fn changed(a: &Picture, b: &Picture) -> Option<f32> {
    if a.size != b.size || a.rgba.len() != b.rgba.len() || a.rgba.is_empty() {
        return None;
    }
    let premultiplied = |p: &[u8; 4]| {
        let alpha = u16::from(p[3]);
        [p[0], p[1], p[2]]
            .map(|c| u8::try_from(u16::from(c) * alpha / 255).unwrap_or(u8::MAX))
            .into_iter()
            .chain([p[3]])
    };
    let (pixels_a, pixels_b) = (a.rgba.as_chunks::<4>().0, b.rgba.as_chunks::<4>().0);
    let differ = pixels_a
        .iter()
        .zip(pixels_b)
        .filter(|(pa, pb)| premultiplied(pa).zip(premultiplied(pb)).any(|(x, y)| x.abs_diff(y) > THRESHOLD))
        .count();
    #[allow(clippy::cast_precision_loss, reason = "a picture's pixel count is far below 2^24")]
    Some(differ as f32 / pixels_a.len() as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_name_steps_and_renders() {
        assert_eq!(Source::parse("step:12"), Some(Source::Step(TxId(12))));
        let hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let render = Source::parse(&format!("omuzaki://render/{hash}.png"));
        assert_eq!(render.as_ref().map(Source::reference), Some(format!("omuzaki://render/{hash}.png")));
        assert_eq!(Source::parse(hash), render);
        assert_eq!(Source::parse("step:x"), None);
        assert_eq!(Source::parse("a sheet"), None);
    }

    #[test]
    fn changed_counts_covered_pixels_that_moved() {
        let ctx = Context::default();
        let picture = |pixels: Vec<u8>| Picture {
            texture: ctx.load_texture("t", ColorImage::filled([2, 1], egui::Color32::BLACK), TextureOptions::LINEAR),
            rgba: Arc::new(pixels),
            size: [2, 1],
        };
        let a = picture(vec![255, 0, 0, 255, 0, 0, 0, 0]);
        let b = picture(vec![0, 0, 255, 255, 255, 255, 255, 0]);
        // The first pixel turned blue; the second changed colour under no coverage.
        assert_eq!(changed(&a, &b), Some(0.5));
        assert_eq!(changed(&a, &a), Some(0.0));
        let other = Picture { size: [1, 2], ..a.clone() };
        assert_eq!(changed(&a, &other), None);
    }
}
