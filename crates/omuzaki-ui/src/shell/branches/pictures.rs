//! Pictures of a state: a branch's head, the live document, the two sides of a merge, rendered at one shot so the
//! columns of a compare and the cards of a merge differ only in what the states hold (`docs/design/branches.md` 6.3).
//!
//! They render on a worker thread of their own, never on the UI thread, through the stage's
//! [`picture_of`](omuzaki_stage::ops::render::history::picture_of): `render.preview`'s own work over a copy of the
//! state's tree, outside the runner, so looking at a branch adds no step to anyone's history. Each picture is asked
//! for by a key that names its state and shot; asking again for a key already asked for costs nothing.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};

use omuzaki_doc_api::Value;
use omuzaki_stage::ops::render::preview::Preview;

/// Renders one picture of a state.
pub trait Painter: Send + Sync {
    /// The PNG of `shot` of the character as `tree` holds it, or why it can't be made.
    ///
    /// # Errors
    /// In words: the tree has no such character, or the render failed.
    fn paint(&self, tree: Arc<Value>, shot: &Preview) -> Result<Vec<u8>, String>;
}

/// A picture as it stands.
#[derive(Clone, Debug)]
pub enum Picture {
    /// Asked for and not made yet.
    Waiting,
    /// Made: straight RGBA8 and its size.
    Ready(Arc<egui::ColorImage>),
    /// It can't be made.
    Failed(String),
}

#[derive(Default)]
struct Queue {
    asked: VecDeque<(String, Arc<Value>, Preview)>,
    made: HashMap<String, Picture>,
    stop: bool,
    repaint: Option<egui::Context>,
}

struct Shared {
    queue: Mutex<Queue>,
    work: Condvar,
}

impl Shared {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The pictures the branch windows show. Clones share one worker, which stops when the last clone goes.
#[derive(Clone)]
pub struct StatePictures {
    shared: Arc<Shared>,
    _owner: Arc<Owner>,
}

impl std::fmt::Debug for StatePictures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatePictures").finish_non_exhaustive()
    }
}

/// Stops the worker when the last [`StatePictures`] goes.
struct Owner(Weak<Shared>);

impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(shared) = self.0.upgrade() {
            shared.queue().stop = true;
            shared.work.notify_all();
        }
    }
}

/// How many pictures are kept; the oldest go first.
const KEPT: usize = 48;

impl StatePictures {
    /// A worker painting with `painter`.
    ///
    /// # Errors
    /// The worker thread did not start.
    pub fn start(painter: Box<dyn Painter>) -> Result<Self, String> {
        let shared = Arc::new(Shared { queue: Mutex::default(), work: Condvar::new() });
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("branch-pictures".to_owned())
            .spawn(move || run(&worker, painter.as_ref()))
            .map_err(|e| e.to_string())?;
        Ok(Self { _owner: Arc::new(Owner(Arc::downgrade(&shared))), shared })
    }

    /// The picture for `key`, asking for it (`shot` of `tree`) when it hasn't been: `Waiting` until the worker makes
    /// it, then repaints `ctx`.
    pub fn picture(&self, ctx: &egui::Context, key: &str, tree: &Arc<Value>, shot: &Preview) -> Picture {
        let mut queue = self.shared.queue();
        if let Some(picture) = queue.made.get(key) {
            return picture.clone();
        }
        queue.repaint = Some(ctx.clone());
        queue.made.insert(key.to_owned(), Picture::Waiting);
        queue.asked.push_back((key.to_owned(), Arc::clone(tree), shot.clone()));
        drop(queue);
        self.shared.work.notify_one();
        Picture::Waiting
    }

    /// Waits until every picture asked for so far is made (tests and evidence).
    pub fn settle(&self) {
        let mut queue = self.shared.queue();
        while queue.made.values().any(|p| matches!(p, Picture::Waiting)) && !queue.stop {
            queue = self.shared.work.wait(queue).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

fn run(shared: &Shared, painter: &dyn Painter) {
    loop {
        let mut queue = shared.queue();
        let (key, tree, shot) = loop {
            if queue.stop {
                return;
            }
            if let Some(next) = queue.asked.pop_front() {
                break next;
            }
            queue = shared.work.wait(queue).unwrap_or_else(PoisonError::into_inner);
        };
        drop(queue);
        let picture = match painter.paint(tree, &shot).and_then(|png| decode(&png)) {
            Ok(image) => Picture::Ready(Arc::new(image)),
            Err(why) => Picture::Failed(why),
        };
        let mut queue = shared.queue();
        if queue.made.len() >= KEPT {
            let done: Vec<String> =
                queue.made.iter().filter(|(_, p)| !matches!(p, Picture::Waiting)).map(|(k, _)| k.clone()).collect();
            for old in done.into_iter().take(KEPT / 4) {
                queue.made.remove(&old);
            }
        }
        queue.made.insert(key, picture);
        let repaint = queue.repaint.clone();
        drop(queue);
        shared.work.notify_all();
        if let Some(ctx) = repaint {
            ctx.request_repaint();
        }
    }
}

fn decode(png: &[u8]) -> Result<egui::ColorImage, String> {
    let image = image::load_from_memory(png).map_err(|e| e.to_string())?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}

/// The studio's painter: `render.preview`'s work on the app's renderer, its pictures kept in `store`.
pub struct Rendered {
    services: omuzaki_ops_api::Services,
    store: Arc<dyn omuzaki_store_api::Store>,
}

impl Rendered {
    /// A painter over `renderer` (the app's own, on the device egui draws with), keeping pictures in `store`.
    pub fn new(renderer: Box<dyn omuzaki_render_api::Renderer>, store: Arc<dyn omuzaki_store_api::Store>) -> Self {
        use omuzaki_char_api::{BuildServices, CharacterBuilder, RegistryBuilder};
        let capabilities = renderer.capabilities().clone();
        let services = omuzaki_ops_api::ServicesBuilder::new()
            .insert::<Mutex<Box<dyn omuzaki_render_api::Renderer>>>(Arc::new(Mutex::new(renderer)))
            .insert::<omuzaki_render_api::Capabilities>(Arc::new(capabilities))
            .insert::<dyn CharacterBuilder>(Arc::new(RegistryBuilder))
            .insert::<BuildServices>(Arc::new(BuildServices::default()))
            .insert::<dyn omuzaki_store_api::Store>(Arc::clone(&store))
            .build();
        Self { services, store }
    }
}

impl Painter for Rendered {
    fn paint(&self, tree: Arc<Value>, shot: &Preview) -> Result<Vec<u8>, String> {
        let result = omuzaki_stage::ops::render::history::picture_of(tree, shot, &self.services)
            .map_err(|e| e.message)?;
        let hash = result.hash.parse().map_err(|e: omuzaki_store_api::HashParseError| e.0)?;
        self.store.get(&hash).map_err(|e| e.to_string())
    }
}
