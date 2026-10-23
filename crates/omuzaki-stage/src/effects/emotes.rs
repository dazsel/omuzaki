//! Chat emote pictures (particles.md D13; #1276): fetched off the frame thread, cached in the app's cache folder,
//! uploaded into the atlas a little at a time, shown live and never stored.
//!
//! Three threads touch an emote:
//! - **The op thread** ([`EmotePipe::note`]) records each emote's picture URL under its key (a hash of the URL), from
//!   `stage.effect.burst`.
//! - **The frame thread** asks for a missing key when a burst first draws it ([`EmotePipe::want`], a `try_send` that
//!   never waits), and takes back at most [`UPLOADS_PER_FRAME`] finished pictures a frame ([`EmotePipe::take`],
//!   `try_lock` and `try_recv`). It allocates nothing: pictures arrive in one of [`BUFFERS`] buffers made at start and
//!   go back to the worker once uploaded.
//! - **One worker thread** reads the setting, checks the URL (HTTPS, a host on the list), reads the cache or fetches
//!   (at most [`MAX_BYTES`]), decodes the PNG within limits, fits it into a [`CELL_PX`] cell and premultiplies it.
//!   Anything refused or failed comes back as a [`Miss`], and the emote draws the placeholder heart.
//!
//! Nothing here writes to the project, the store, a take or an export: the pictures live in the cache folder and the
//! atlas texture only. A take records the emote's text reference (`ChatEmoteRef`), never its bytes.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use omuzaki_ai_api::Fetcher;
use omuzaki_settings_api::service::{Settings, SettingsExt};

use super::settings::ChatEmotes;

/// A cell's side in pixels: a picture is fitted into it, aspect kept and centred.
pub const CELL_PX: u32 = 128;
/// The most bytes a picture may have, fetched or cached.
pub const MAX_BYTES: u64 = 256 * 1024;
/// The largest picture side decoded.
pub const MAX_SIDE: u32 = 512;
/// The most memory one decode may allocate.
const MAX_DECODE_ALLOC: u64 = 4 * 1024 * 1024;
/// The requests waiting for the worker; more are refused and draw the placeholder.
pub const QUEUE: usize = 32;
/// The picture buffers, made at start and passed between the worker and the frame thread.
pub const BUFFERS: usize = 8;
/// The most pictures the frame thread uploads in one frame.
pub const UPLOADS_PER_FRAME: usize = 2;
/// The cache folder's bound.
pub const CACHE_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Cached pictures older than this are removed when the pipe starts.
pub const CACHE_MAX_AGE: Duration = Duration::from_hours(7 * 24);
/// The hosts the first-party chat adapters serve emote pictures from (the Twitch CDN and the `YouTube` one).
pub const FIRST_PARTY_HOSTS: [&str; 2] = ["static-cdn.jtvnw.net", "yt3.ggpht.com"];
/// The most URLs remembered; past it the table starts again (a picture already in the atlas stays).
const MAX_URLS: usize = 4096;
/// One picture's bytes in a buffer.
const CELL_BYTES: usize = (CELL_PX * CELL_PX * 4) as usize;

/// Where emote pictures come from, and what may be fetched.
#[derive(Clone)]
#[non_exhaustive]
pub struct EmoteSource {
    /// The HTTPS client. The app gives one with a 5 s timeout and no redirects, so a redirect never leaves the list.
    pub fetcher: Arc<dyn Fetcher>,
    /// The settings service, for `effects.chat_emotes`; without one the setting's default (on) holds.
    pub settings: Option<Arc<dyn Settings>>,
    /// The cache folder (`chat-emotes/` in the app's cache folder); `None` caches nothing.
    pub cache: Option<PathBuf>,
    /// The hosts pictures may come from: the first-party ones and any a plugin declares.
    pub hosts: Vec<String>,
}

impl EmoteSource {
    /// Pictures from `fetcher`, from the first-party hosts, with no settings service and no cache.
    pub fn new(fetcher: Arc<dyn Fetcher>) -> Self {
        Self {
            fetcher,
            settings: None,
            cache: None,
            hosts: FIRST_PARTY_HOSTS.iter().map(|h| (*h).to_owned()).collect(),
        }
    }

    /// The same, reading `effects.chat_emotes` from `settings`.
    #[must_use]
    pub fn with_settings(mut self, settings: Arc<dyn Settings>) -> Self {
        self.settings = Some(settings);
        self
    }

    /// The same, caching in `folder`.
    #[must_use]
    pub fn with_cache(mut self, folder: PathBuf) -> Self {
        self.cache = Some(folder);
        self
    }

    /// The same, also allowing `host` (a plugin's declared host).
    #[must_use]
    pub fn with_host(mut self, host: &str) -> Self {
        self.hosts.push(host.to_ascii_lowercase());
        self
    }
}

/// Why an emote draws the placeholder.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Miss {
    /// `effects.chat_emotes` is off.
    Off,
    /// No picture URL is known for the key.
    NoUrl,
    /// The URL is not HTTPS, or its host is not on the list. Nothing was contacted.
    Refused,
    /// The fetch failed.
    Fetch(String),
    /// The picture is larger than [`MAX_BYTES`].
    TooLarge,
    /// The bytes are not a PNG.
    NotPng,
    /// The PNG could not be decoded within the limits.
    Decode(String),
}

/// A finished request, from the worker to the frame thread.
#[derive(Debug)]
pub enum Done {
    /// The picture of `key`, fitted, premultiplied, [`CELL_PX`] square; its buffer goes back with
    /// [`EmotePipe::give_back`].
    Loaded {
        /// The emote's key.
        key: u64,
        /// `CELL_PX * CELL_PX` RGBA8 pixels.
        pixels: Box<[u8]>,
    },
    /// `key` draws the placeholder.
    Failed {
        /// The emote's key.
        key: u64,
        /// Why.
        miss: Miss,
    },
}

/// What the pipe has done, for status and tests.
#[derive(Debug, Default)]
pub struct EmoteCounts {
    /// Requests the frame thread made.
    pub wanted: AtomicU64,
    /// Requests refused because the queue was full.
    pub dropped: AtomicU64,
    /// Network fetches started.
    pub fetched: AtomicU64,
    /// Pictures read from the cache.
    pub cached: AtomicU64,
    /// Pictures delivered.
    pub loaded: AtomicU64,
    /// Requests that ended in a [`Miss`].
    pub missed: AtomicU64,
}

/// The URLs the op thread recorded, by key.
type Urls = Arc<Mutex<HashMap<u64, String>>>;

/// The frame thread's and the op thread's ends of the emote pipeline. Dropping it ends the worker.
pub struct EmotePipe {
    urls: Urls,
    wants: SyncSender<u64>,
    done: Mutex<Receiver<Done>>,
    pool: SyncSender<Box<[u8]>>,
    counts: Arc<EmoteCounts>,
}

impl std::fmt::Debug for EmotePipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmotePipe").field("counts", &self.counts).finish_non_exhaustive()
    }
}

impl EmotePipe {
    /// Starts the worker over `source`, after removing stale cache entries. Cold: it allocates the buffers. If the
    /// thread can't start, every emote draws the placeholder.
    pub fn start(source: EmoteSource) -> Self {
        let urls: Urls = Arc::new(Mutex::new(HashMap::new()));
        let (wants, wanted) = mpsc::sync_channel::<u64>(QUEUE);
        let (finished, done) = mpsc::sync_channel::<Done>(QUEUE + BUFFERS);
        let (pool, buffers) = mpsc::sync_channel::<Box<[u8]>>(BUFFERS);
        for _ in 0..BUFFERS {
            // The receiver is alive and the channel has room for every buffer.
            let _ = pool.try_send(vec![0u8; CELL_BYTES].into_boxed_slice());
        }
        let counts = Arc::new(EmoteCounts::default());
        let worker = Worker { source, urls: Arc::clone(&urls), counts: Arc::clone(&counts), cache_bytes: 0 };
        let spawned =
            std::thread::Builder::new().name("omuzaki-emotes".into()).spawn(move || worker.run(&wanted, &finished, &buffers));
        if let Err(error) = spawned {
            tracing::warn!(%error, "the chat-emote worker did not start: emotes draw the placeholder");
        }
        Self { urls, wants, done: Mutex::new(done), pool, counts }
    }

    /// Records that `key` shows the picture at `url`. The op thread calls it for each emote of a burst.
    pub fn note(&self, key: u64, url: &str) {
        let mut urls = self.urls.lock().unwrap_or_else(PoisonError::into_inner);
        if urls.len() >= MAX_URLS && !urls.contains_key(&key) {
            urls.clear();
        }
        urls.entry(key).or_insert_with(|| url.to_owned());
    }

    /// Asks the worker for `key`. Never waits: `false` when the queue is full or the worker is gone, and the emote
    /// then draws the placeholder. Frame thread; allocation-free.
    pub fn want(&self, key: u64) -> bool {
        self.counts.wanted.fetch_add(1, Ordering::Relaxed);
        match self.wants.try_send(key) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.counts.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Hands at most `max` finished requests to `f`. Never waits: nothing when another thread holds the receiver.
    /// Frame thread; allocation-free.
    pub fn take(&self, max: usize, mut f: impl FnMut(Done)) {
        let Ok(done) = self.done.try_lock() else { return };
        for _ in 0..max {
            match done.try_recv() {
                Ok(item) => f(item),
                Err(_) => return,
            }
        }
    }

    /// Returns a picture's buffer to the worker once it is uploaded. Frame thread; allocation-free.
    pub fn give_back(&self, pixels: Box<[u8]>) {
        // There are exactly `BUFFERS` buffers and the channel holds as many, so it is never full; a gone worker just
        // drops it.
        let _ = self.pool.try_send(pixels);
    }

    /// What the pipe has done so far.
    pub fn counts(&self) -> &EmoteCounts {
        &self.counts
    }
}

/// The worker's state.
struct Worker {
    source: EmoteSource,
    urls: Urls,
    counts: Arc<EmoteCounts>,
    /// The cache folder's size as last counted.
    cache_bytes: u64,
}

impl Worker {
    fn run(mut self, wanted: &Receiver<u64>, finished: &SyncSender<Done>, buffers: &Receiver<Box<[u8]>>) {
        if let Some(folder) = self.source.cache.clone() {
            self.cache_bytes = prune_cache(&folder, CACHE_MAX_AGE, CACHE_MAX_BYTES);
        }
        while let Ok(key) = wanted.recv() {
            let (done, count) = match self.load(key) {
                Ok(fitted) => {
                    // Every buffer comes back once its picture is uploaded; none left means the stage is gone.
                    let Ok(mut pixels) = buffers.recv() else { return };
                    pixels.copy_from_slice(&fitted);
                    (Done::Loaded { key, pixels }, &self.counts.loaded)
                }
                Err(miss) => {
                    tracing::debug!(key, ?miss, "a chat emote draws the placeholder");
                    (Done::Failed { key, miss }, &self.counts.missed)
                }
            };
            if finished.send(done).is_err() {
                return;
            }
            // Counted once it is on its way, so a reader that sees the count can take the result.
            count.fetch_add(1, Ordering::Release);
        }
    }

    /// The fitted picture of `key`, or why not.
    fn load(&mut self, key: u64) -> Result<Vec<u8>, Miss> {
        if let Some(settings) = &self.source.settings
            && !settings.get::<ChatEmotes>().unwrap_or(true)
        {
            return Err(Miss::Off);
        }
        let url = self.urls.lock().unwrap_or_else(PoisonError::into_inner).get(&key).cloned().ok_or(Miss::NoUrl)?;
        if !allowed(&url, &self.source.hosts) {
            return Err(Miss::Refused);
        }
        let file = self.source.cache.as_ref().map(|folder| folder.join(format!("{key:016x}.png")));
        if let Some(bytes) = file.as_deref().and_then(read_cached) {
            self.counts.cached.fetch_add(1, Ordering::Relaxed);
            return fit(&bytes);
        }
        self.counts.fetched.fetch_add(1, Ordering::Relaxed);
        let bytes = fetch(self.source.fetcher.as_ref(), &url)?;
        let fitted = fit(&bytes)?;
        if let Some(file) = &file {
            self.store(file, &bytes);
        }
        Ok(fitted)
    }

    /// Keeps a fetched picture in the cache, pruning the folder when it outgrows its bound.
    fn store(&mut self, file: &Path, bytes: &[u8]) {
        let Some(folder) = file.parent() else { return };
        if let Err(error) = write_cached(folder, file, bytes) {
            tracing::debug!(%error, "a chat emote was not cached");
            return;
        }
        self.cache_bytes = self.cache_bytes.saturating_add(bytes.len() as u64);
        if self.cache_bytes > CACHE_MAX_BYTES {
            self.cache_bytes = prune_cache(folder, CACHE_MAX_AGE, CACHE_MAX_BYTES * 3 / 4);
        }
    }
}

/// Whether `url` is HTTPS to a host on `hosts` (no user, no port other than 443).
pub fn allowed(url: &str, hosts: &[String]) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return false;
    }
    let host = match authority.rsplit_once(':') {
        Some((host, "443")) => host,
        Some(_) => return false,
        None => authority,
    };
    let host = host.to_ascii_lowercase();
    hosts.iter().any(|allowed| allowed.eq_ignore_ascii_case(&host))
}

/// At most [`MAX_BYTES`] of `url`.
fn fetch(fetcher: &dyn Fetcher, url: &str) -> Result<Vec<u8>, Miss> {
    let body = fetcher.open(url, 0).map_err(|e| Miss::Fetch(e.to_string()))?;
    let mut bytes = Vec::with_capacity(16 * 1024);
    body.reader.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|e| Miss::Fetch(e.to_string()))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Miss::TooLarge);
    }
    Ok(bytes)
}

/// The PNG `bytes` decoded within the limits, fitted into a cell (aspect kept, centred, transparent around it) and
/// premultiplied in its sRGB encoding, as the atlas holds it.
///
/// # Errors
/// [`Miss::NotPng`] for other bytes, [`Miss::Decode`] for a PNG that breaks the limits or doesn't decode.
pub fn fit(bytes: &[u8]) -> Result<Vec<u8>, Miss> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if !bytes.starts_with(&SIGNATURE) {
        return Err(Miss::NotPng);
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let picture = reader.decode().map_err(|e| Miss::Decode(e.to_string()))?.to_rgba8();
    let (width, height) = picture.dimensions();
    if width == 0 || height == 0 {
        return Err(Miss::Decode("an empty picture".into()));
    }
    // The longer side fills the cell; integer arithmetic, rounded, at least one pixel.
    let long = width.max(height);
    let side = |n: u32| ((n * CELL_PX + long / 2) / long).clamp(1, CELL_PX);
    let (fitted_w, fitted_h) = (side(width), side(height));
    let fitted = image::imageops::resize(&picture, fitted_w, fitted_h, image::imageops::FilterType::CatmullRom);
    let mut cell = vec![0u8; CELL_BYTES];
    let (left, top) = ((CELL_PX - fitted_w) / 2, (CELL_PX - fitted_h) / 2);
    for (x, y, pixel) in fitted.enumerate_pixels() {
        let [red, green, blue, alpha] = pixel.0;
        let premultiply = |c: u8| u8::try_from((u16::from(c) * u16::from(alpha) + 127) / 255).unwrap_or(u8::MAX);
        let at = (((top + y) * CELL_PX + left + x) * 4) as usize;
        cell[at..at + 4].copy_from_slice(&[premultiply(red), premultiply(green), premultiply(blue), alpha]);
    }
    Ok(cell)
}

/// A cached picture, if there is one no larger than [`MAX_BYTES`].
#[allow(clippy::disallowed_methods, reason = "the emote cache is the app's own cache folder, never a project file (D13)")]
fn read_cached(file: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(file).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return None;
    }
    std::fs::read(file).ok()
}

/// Writes `bytes` to `file` through a temporary file and a rename, so a reader never sees half a picture.
#[allow(clippy::disallowed_methods, reason = "the emote cache is the app's own cache folder, never a project file (D13)")]
fn write_cached(folder: &Path, file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    let partial = file.with_extension("part");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, file)
}

/// Removes cached pictures older than `max_age`, then the oldest until the folder holds at most `keep` bytes. Returns
/// the bytes left. A missing folder holds nothing.
#[allow(clippy::disallowed_methods, reason = "the emote cache is the app's own cache folder, never a project file (D13)")]
pub fn prune_cache(folder: &Path, max_age: Duration, keep: u64) -> u64 {
    let Ok(entries) = std::fs::read_dir(folder) else { return 0 };
    let now = SystemTime::now();
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(now);
        let stale = now.duration_since(modified).is_ok_and(|age| age > max_age);
        let ours = path.extension().is_some_and(|e| e == "png" || e == "part");
        if ours && stale {
            let _ = std::fs::remove_file(&path);
        } else if ours {
            files.push((modified, meta.len(), path));
        }
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    for (_, len, path) in &files {
        if total <= keep {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(*len);
        }
    }
    total
}
