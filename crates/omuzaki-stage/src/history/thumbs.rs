//! History thumbnails (spec VC-01, design-system section 9 rule 7): after each committed transaction that changed the
//! document, a 96x96 picture (rendered at 2x) of the character it touched, in its default framing at live quality.
//!
//! [`Thumbnails`] subscribes to a runner's events and renders on a thread of its own, one picture at a time, never on
//! the UI or the frame thread. Commits coalesce: a burst renders once, [`COALESCE`] after its last commit (or
//! [`MAX_WAIT`] after its first, under a steady stream of edits), and every transaction of the burst points at that
//! one picture. The pictures go to the asset store through `render.preview`'s work, outside the runner, so a
//! thumbnail never adds a step to the history it illustrates; the index, transaction id to hash, is
//! [`Index`](super::index::Index).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_char_api::CharId;
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, Patch, Value, lookup};
use omuzaki_ops_api::{
    Actor, CancelFlag, Event, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpError, Runner, Scope, Services, TxId,
};

use super::index::Index;
use crate::ops::render::job::RenderQuality;
use crate::ops::render::preview::{Preview, PreviewResult};

/// How long after the last commit of a burst its thumbnail renders.
pub const COALESCE: Duration = Duration::from_millis(250);
/// The longest a committed transaction waits for its thumbnail while commits keep coming.
pub const MAX_WAIT: Duration = Duration::from_secs(2);
/// The rendered size: 96x96 shown at 2x.
pub const SIZE: [u32; 2] = [192, 192];
/// The worker's thread name.
pub const THREAD: &str = "omuzaki-thumbs";
/// How often an idle worker looks for a stop.
const IDLE_POLL: Duration = Duration::from_millis(100);

/// Why the thumbnailer did not start.
#[derive(Debug, thiserror::Error)]
pub enum ThumbsError {
    /// The runner refused the event stream.
    #[error("subscribing to the history: {0}")]
    Subscribe(OpError),
    /// The worker thread did not start.
    #[error("starting the thumbnail worker: {0}")]
    Spawn(std::io::Error),
}

/// What the worker and the handles share.
struct Shared {
    index: Mutex<Index>,
    /// Signalled after each batch lands in the index.
    landed: Condvar,
    renders: AtomicU64,
    stop: AtomicBool,
}

impl Shared {
    fn index(&self) -> MutexGuard<'_, Index> {
        self.index.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The history's thumbnailer: a worker that renders a picture for each committed transaction and an index of them.
/// Dropping it (or [`stop`](Self::stop)) renders what is pending and joins the worker.
pub struct Thumbnails {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Thumbnails {
    /// Starts thumbnailing `runner`'s transactions from now on, rendering with `services` (the renderer, the character
    /// builder and its services, and the asset store the pictures go to) and keeping the index in `project`'s
    /// `.omuzaki/history/`. Transactions already in the index keep their thumbnails.
    ///
    /// # Errors
    /// The runner refuses the event stream, or the worker thread does not start.
    pub fn start(runner: Arc<dyn Runner>, services: Services, project: &Path) -> Result<Self, ThumbsError> {
        let grants = Grants::of([Scope::Read]);
        let events = runner.subscribe(&grants).map_err(ThumbsError::Subscribe)?;
        let shared = Arc::new(Shared {
            index: Mutex::new(Index::load(project)),
            landed: Condvar::new(),
            renders: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });
        let worker = Worker {
            runner,
            grants,
            services,
            events,
            shared: shared.clone(),
            open: BTreeMap::new(),
            pending: Vec::new(),
            first: None,
            last: None,
            version: DocVersion(0),
            recent: None,
        };
        let worker = std::thread::Builder::new()
            .name(THREAD.to_owned())
            .spawn(move || worker.run())
            .map_err(ThumbsError::Spawn)?;
        Ok(Self { shared, worker: Some(worker) })
    }

    /// `tx`'s thumbnail: the PNG's hash in the asset store, once it has rendered.
    #[must_use]
    pub fn get(&self, tx: TxId) -> Option<String> {
        self.shared.index().get(tx).map(ToOwned::to_owned)
    }

    /// Waits up to `timeout` for `tx`'s thumbnail. Batches land in commit order, so once a transaction has its
    /// thumbnail every transaction committed before it has one too.
    #[must_use]
    pub fn wait_for(&self, tx: TxId, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut index = self.shared.index();
        loop {
            if let Some(hash) = index.get(tx) {
                return Some(hash.to_owned());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            index = self.shared.landed.wait_timeout(index, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// How many pictures the worker has rendered since it started.
    #[must_use]
    pub fn renders(&self) -> u64 {
        self.shared.renders.load(Ordering::Relaxed)
    }

    /// Renders what is pending, then stops the worker.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("the history thumbnail worker panicked");
        }
    }
}

impl Drop for Thumbnails {
    fn drop(&mut self) {
        self.halt();
    }
}

/// The worker's state: transactions open and waiting, and the batch it is gathering.
struct Worker {
    runner: Arc<dyn Runner>,
    grants: Grants,
    services: Services,
    events: Receiver<Event>,
    shared: Arc<Shared>,
    /// Transactions with applied changes, not yet committed, and the first character each touched.
    open: BTreeMap<TxId, Option<CharId>>,
    /// Committed transactions waiting for the batch's picture.
    pending: Vec<(TxId, Option<CharId>)>,
    first: Option<Instant>,
    last: Option<Instant>,
    /// The document version of the last applied op, for the snapshot a picture renders from.
    version: DocVersion,
    /// The character last pictured, for a transaction that touched none.
    recent: Option<CharId>,
}

impl Worker {
    fn run(mut self) {
        loop {
            if self.shared.stop.load(Ordering::Relaxed) {
                // Whatever is in the channel was committed before the stop: it gets its picture too.
                while let Ok(event) = self.events.try_recv() {
                    self.take(event);
                }
                self.flush();
                return;
            }
            let wait = self.due().map_or(IDLE_POLL, |due| due.saturating_duration_since(Instant::now()).min(IDLE_POLL));
            match self.events.recv_timeout(wait) {
                Ok(event) => self.take(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.flush();
                    return;
                }
            }
            if self.due().is_some_and(|due| Instant::now() >= due) {
                self.flush();
            }
        }
    }

    /// When the gathered batch renders.
    fn due(&self) -> Option<Instant> {
        Some((self.last? + COALESCE).min(self.first? + MAX_WAIT))
    }

    fn take(&mut self, event: Event) {
        match event {
            Event::OpApplied { record, .. } => {
                self.version = record.version;
                if record.patch.is_empty() {
                    return;
                }
                let touched = character_of(&record.patch);
                let entry = self.open.entry(record.tx).or_insert(None);
                if entry.is_none() {
                    *entry = touched;
                }
            }
            Event::TxCommitted { tx, .. } => {
                // A transaction that changed nothing (a render job, a query) shows nothing new.
                if let Some(character) = self.open.remove(&tx) {
                    let now = Instant::now();
                    self.pending.push((tx, character));
                    self.first.get_or_insert(now);
                    self.last = Some(now);
                }
            }
            Event::TxRolledBack { tx, .. } => {
                self.open.remove(&tx);
            }
            _ => {}
        }
    }

    /// Renders the batch from the document as it is now and points each of its transactions at the picture.
    fn flush(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        (self.first, self.last) = (None, None);
        if pending.is_empty() {
            return;
        }
        let snapshot = match self.runner.get(&DocPath::root(), &self.grants) {
            Ok(Some(root)) => Snapshot { root, version: self.version },
            Ok(None) => return,
            Err(e) => {
                tracing::warn!("reading the document for the history thumbnails: {e}");
                return;
            }
        };
        let mut pictures: BTreeMap<CharId, Option<String>> = BTreeMap::new();
        let mut landed = Vec::with_capacity(pending.len());
        for (tx, touched) in pending {
            let Some(character) = self.subject(&snapshot, touched) else { continue };
            let hash = if let Some(hash) = pictures.get(&character) {
                hash.clone()
            } else {
                let hash = self.render(&snapshot, &character);
                pictures.insert(character.clone(), hash.clone());
                hash
            };
            self.recent = Some(character);
            if let Some(hash) = hash {
                landed.push((tx, hash));
            }
        }
        let mut index = self.shared.index();
        for (tx, hash) in landed {
            index.insert(tx, hash);
        }
        if let Err(e) = index.save() {
            tracing::warn!("writing the history thumbnail index: {e}");
        }
        drop(index);
        self.shared.landed.notify_all();
    }

    /// The character a transaction's picture shows: the one it touched, else the one last pictured, else the
    /// project's first; `None` in a project without characters.
    fn subject(&self, snapshot: &Snapshot, touched: Option<CharId>) -> Option<CharId> {
        let exists = |id: &CharId| snapshot.root.get("characters").and_then(|c| c.get(id.as_str())).is_some();
        touched
            .filter(exists)
            .or_else(|| self.recent.clone().filter(exists))
            .or_else(|| {
                let characters = snapshot.root.get("characters")?.as_object()?;
                characters.keys().find_map(|id| CharId::new(id).ok())
            })
    }

    /// `character`'s picture from `snapshot`, stored; `None` (with a warning) when it does not render.
    fn render(&self, snapshot: &Snapshot, character: &CharId) -> Option<String> {
        let preview = Preview {
            character: character.clone(),
            variant: None,
            scene: None,
            camera: None,
            framing: None,
            yaw: None,
            pitch: None,
            zoom: None,
            light_rig: None,
            expression: None,
            weight: 1.0,
            pose: None,
            size: SIZE,
            quality: RenderQuality::Live,
            preset: None,
        };
        let actor = Actor::System;
        let signal = Signal;
        let job = JobCtx::new(JobId(0), &actor, snapshot, &signal).with_services(&self.services);
        let result = preview.work(&job).and_then(|value| {
            serde_json::from_value::<PreviewResult>(value)
                .map_err(|e| OpError::internal(format!("reading the preview back: {e}")))
        });
        self.shared.renders.fetch_add(1, Ordering::Relaxed);
        match result {
            Ok(result) => Some(result.hash),
            Err(e) => {
                tracing::warn!("rendering the history thumbnail of '{character}': {e}");
                None
            }
        }
    }
}

/// The first character `patch` changes something of, by its `characters/<id>` path.
fn character_of(patch: &Patch) -> Option<CharId> {
    patch.changes.iter().filter(|c| c.before != c.after).find_map(|change| match change.path.segments() {
        [table, id, ..] if table == "characters" => CharId::new(id).ok(),
        _ => None,
    })
}

/// The document as it was when a batch rendered.
struct Snapshot {
    root: Value,
    version: DocVersion,
}

impl DocRead for Snapshot {
    fn get(&self, path: &DocPath) -> Option<&Value> {
        lookup(&self.root, path)
    }

    fn version(&self) -> DocVersion {
        self.version
    }
}

impl JobDoc for Snapshot {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(self);
    }
}

/// A thumbnail's job signal: never cancelled (the last batch renders on the way out), and it spends nothing.
struct Signal;

impl JobSignal for Signal {
    fn progress(&self, _stage: &str, _fraction: f32) {}

    fn is_cancelled(&self) -> bool {
        false
    }

    fn cancel_flag(&self) -> CancelFlag {
        CancelFlag::new()
    }

    fn spend(&self, _usd_micros: u64) -> Result<(), OpError> {
        Err(OpError::denied("a history thumbnail spends nothing"))
    }
}
