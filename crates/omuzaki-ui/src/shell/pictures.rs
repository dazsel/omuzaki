//! Each History row's picture (`shell.md` section 9, `design-system.md` section 9 rule 7, #446): the character as it
//! looked right after the step, so a person finds a state by looking, not by reading op names.
//!
//! The pictures are taken by the stage's thumbnailer (`omuzaki_stage::history::thumbs`, #233), off the UI and frame
//! threads, a moment after a burst of edits settles; the app hands History a [`HistoryPictures`] view model that finds
//! one by transaction. The panel asks only for the rows it draws, uploads at most [`UPLOADS_PER_FRAME`] pictures a
//! frame, and keeps at most [`KEPT`] textures, so a long history costs what its visible rows do. A step with no
//! picture (it changed no character, or its picture has not landed yet) keeps the same empty square, so no row shifts
//! when one arrives.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{ColorImage, Rect, TextureHandle, TextureOptions, Ui};
use omuzaki_ops_api::TxId;

use crate::ds::theme::ds;

/// Where History's pictures come from.
pub trait StepPictures: Send + Sync {
    /// The PNG of the picture taken right after transaction `tx`, once there is one, or why it can't be read.
    fn png(&self, tx: TxId) -> Option<Result<Vec<u8>, String>>;

    /// Waits up to `timeout` for `tx`'s picture; true once it is there. A source that takes no pictures of its own
    /// answers at once.
    fn wait(&self, tx: TxId, timeout: Duration) -> bool {
        let _ = timeout;
        self.png(tx).is_some()
    }
}

/// The pictures History shows beside its rows: a view model the app inserts when it takes pictures. Clones share one
/// source; dropping the last one drops the source (and with it the app's thumbnailer).
#[derive(Clone)]
pub struct HistoryPictures(Arc<dyn StepPictures>);

impl std::fmt::Debug for HistoryPictures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryPictures").finish_non_exhaustive()
    }
}

impl HistoryPictures {
    /// Pictures from `source`.
    pub fn new(source: Arc<dyn StepPictures>) -> Self {
        Self(source)
    }

    /// The PNG of `tx`'s picture, once there is one, or why it can't be read: for a view that decodes it at its own
    /// size (the Theatre's compare strip).
    pub fn png(&self, tx: TxId) -> Option<Result<Vec<u8>, String>> {
        self.0.png(tx)
    }

    /// Waits up to `timeout` for `tx`'s picture; true once it is there. For tests and scripts: the panel never waits.
    pub fn wait_for(&self, tx: TxId, timeout: Duration) -> bool {
        self.0.wait(tx, timeout)
    }
}

/// The most pictures uploaded to the GPU in one frame: scrolling fast through a long history never stalls a frame.
pub const UPLOADS_PER_FRAME: usize = 2;
/// The most pictures kept as textures; the oldest loaded goes first.
pub const KEPT: usize = 128;
/// The side of a kept picture, in pixels: a 44 pt square at 2x, whatever the picture was taken at.
const SIDE: u32 = 88;
/// How long after a step lands its picture is still expected: the thumbnailer's longest wait and its render.
const EXPECTED_WITHIN: Duration = Duration::from_secs(4);
/// How often a row still waiting for its picture looks again.
const LOOK_AGAIN: Duration = Duration::from_millis(300);

/// The textures of the pictures History has shown, by transaction.
#[derive(Default)]
pub struct Atlas {
    /// A texture per transaction; `None` for a picture that would not load, so it is not tried again.
    textures: HashMap<TxId, Option<TextureHandle>>,
    /// The transactions in the order their pictures loaded, for dropping the oldest.
    order: VecDeque<TxId>,
    /// The frame the upload count is for, and the count.
    frame: u64,
    uploads: usize,
}

impl Atlas {
    /// Draws `tx`'s picture into `rect`, or the empty square while there is none. `seen` is when the studio heard of
    /// the step: a recent one is still expecting its picture, so the panel looks again shortly.
    pub fn draw(&mut self, ui: &Ui, pictures: &HistoryPictures, tx: TxId, seen: Option<Instant>, rect: Rect) {
        let theme = &ds(ui.ctx()).theme;
        let texture = self.texture(ui.ctx(), pictures, tx, seen);
        let radius = 4.0;
        match texture {
            Some(texture) => {
                egui::Image::new((texture, rect.size())).corner_radius(radius).paint_at(ui, rect);
            }
            None => {
                ui.painter().rect_filled(rect, radius, theme.bg2);
            }
        }
    }

    /// How many pictures are kept as textures.
    pub fn kept(&self) -> usize {
        self.textures.values().filter(|t| t.is_some()).count()
    }

    /// `tx`'s picture as a texture, once it is there and this frame may upload it, for a widget that draws it itself
    /// (a step card, the Theatre's compare strip); `seen` as for [`draw`](Self::draw).
    pub fn texture(
        &mut self,
        ctx: &egui::Context,
        pictures: &HistoryPictures,
        tx: TxId,
        seen: Option<Instant>,
    ) -> Option<egui::TextureId> {
        if let Some(texture) = self.textures.get(&tx) {
            return texture.as_ref().map(TextureHandle::id);
        }
        let frame = ctx.cumulative_frame_nr();
        if self.frame != frame {
            self.frame = frame;
            self.uploads = 0;
        }
        if self.uploads >= UPLOADS_PER_FRAME {
            ctx.request_repaint();
            return None;
        }
        let Some(png) = pictures.0.png(tx) else {
            if seen.is_some_and(|at| at.elapsed() < EXPECTED_WITHIN) {
                ctx.request_repaint_after(LOOK_AGAIN);
            }
            return None;
        };
        self.uploads += 1;
        let texture = match png.and_then(|png| decode(&png).map_err(|e| e.to_string())) {
            Ok(image) => Some(ctx.load_texture(format!("history-picture-{}", tx.0), image, TextureOptions::LINEAR)),
            Err(error) => {
                tracing::warn!(%error, tx = tx.0, "a history picture did not load; its row shows none");
                None
            }
        };
        let id = texture.as_ref().map(TextureHandle::id);
        self.textures.insert(tx, texture);
        self.order.push_back(tx);
        while self.order.len() > KEPT {
            if let Some(oldest) = self.order.pop_front() {
                self.textures.remove(&oldest);
            }
        }
        id
    }
}

/// `png` as a [`SIDE`]-pixel square image.
fn decode(png: &[u8]) -> Result<ColorImage, image::ImageError> {
    let picture = image::load_from_memory_with_format(png, image::ImageFormat::Png)?;
    let small = picture.resize_to_fill(SIDE, SIDE, image::imageops::FilterType::Triangle).to_rgba8();
    let size = [small.width() as usize, small.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, small.as_raw()))
}

/// The folder under a project's `.omuzaki/` that holds History's pictures: a content-addressed store, never committed
/// (spec section 6: `.omuzaki/` keeps the cache, the op log and thumbnails).
pub const PICTURES_DIR: &str = ".omuzaki/history/pictures";

/// The pictures the stage's thumbnailer takes of `runner`'s steps in the project in `dir`, rendered by `renderer` (the
/// app's own, on the device egui draws with) into the store in [`PICTURES_DIR`]. Dropping the last clone stops the
/// thumbnailer.
///
/// # Errors
/// What went wrong, in words: the store could not be opened or the thumbnailer did not start. History then shows no
/// pictures.
pub fn taken(
    runner: Arc<dyn omuzaki_ops_api::Runner>,
    renderer: Box<dyn omuzaki_render_api::Renderer>,
    dir: &std::path::Path,
) -> Result<HistoryPictures, String> {
    use omuzaki_char_api::{BuildServices, CharacterBuilder, RegistryBuilder};
    use omuzaki_store_api::Store;
    let store = Arc::new(omuzaki_store::DiskStore::open(dir.join(PICTURES_DIR)).map_err(|e| e.to_string())?);
    let capabilities = renderer.capabilities().clone();
    let services = omuzaki_ops_api::ServicesBuilder::new()
        .insert::<std::sync::Mutex<Box<dyn omuzaki_render_api::Renderer>>>(Arc::new(std::sync::Mutex::new(renderer)))
        .insert::<omuzaki_render_api::Capabilities>(Arc::new(capabilities))
        .insert::<dyn CharacterBuilder>(Arc::new(RegistryBuilder))
        .insert::<BuildServices>(Arc::new(BuildServices::default()))
        .insert::<dyn Store>(store.clone())
        .build();
    let thumbs = omuzaki_stage::history::thumbs::Thumbnails::start(runner, services, dir).map_err(|e| e.to_string())?;
    Ok(HistoryPictures::new(Arc::new(Taken { thumbs, store })))
}

/// The thumbnailer's pictures, read back from its store.
struct Taken {
    thumbs: omuzaki_stage::history::thumbs::Thumbnails,
    store: Arc<omuzaki_store::DiskStore>,
}

impl StepPictures for Taken {
    fn png(&self, tx: TxId) -> Option<Result<Vec<u8>, String>> {
        use omuzaki_store_api::Store as _;
        let hash = self.thumbs.get(tx)?;
        Some(
            hash.parse()
                .map_err(|e: omuzaki_store_api::HashParseError| e.0)
                .and_then(|hash| self.store.get(&hash).map_err(|e| e.to_string())),
        )
    }

    fn wait(&self, tx: TxId, timeout: Duration) -> bool {
        self.thumbs.wait_for(tx, timeout).is_some()
    }
}
