//! Branches in the studio (`docs/design/branches.md` sections 5, 6.4, 7.8, 8.3 and 9; VC-02): the branch chip in the
//! status bar and its menu, the review cards for an agent's ready work, comparing branches side by side, and the merge
//! view.
//!
//! The app shares a [`Branching`] view model when a project is open: the app core's lane manager (every switch,
//! create, rename, delete and review goes through it, as a person) and a way to read a point's tree. Merging is the
//! op `branch.merge`, run through the runner like any edit, so a merge is one undo step in History and an agent sees
//! it in its log. Pictures of states come from [`pictures`], outside the runner.
//!
//! - [`menu`]: the chip, the menu and the review cards;
//! - [`compare`]: 2 to 6 branches at one camera with a shared orbit, and what each changes against a reference;
//! - [`merge`]: the merge preview, the choices per unit, and the merge.

pub mod compare;
pub mod menu;
pub mod merge;
pub mod pictures;

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use omuzaki_char_api::CharId;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_history_api::branch::{BranchEvent, BranchId, BranchInfo, Branches, PointId};
use omuzaki_ops_api::{Actor, Grants, Runner};
use omuzaki_stage::ops::render::preview::Preview;

pub use pictures::StatePictures;

/// Reads a point's tree: what a branch held at that point.
pub trait PointTrees: Send + Sync {
    /// The tree of `point`.
    ///
    /// # Errors
    /// In words, when the point isn't in the store or can't be read.
    fn tree(&self, point: &PointId) -> Result<Arc<Value>, String>;
}

/// What the branch windows work with, shared by the app as a view model while a project is open.
#[derive(Clone)]
pub struct Branching {
    /// The app core's lane manager.
    pub branches: Arc<dyn Branches>,
    /// Points' trees.
    pub trees: Arc<dyn PointTrees>,
    /// Pictures of states, when the app has a renderer for them.
    pub pictures: Option<StatePictures>,
}

impl std::fmt::Debug for Branching {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Branching").field("folder", &self.branches.folder()).finish_non_exhaustive()
    }
}

/// The person at the studio, as the lane manager records them.
pub(crate) fn person() -> Actor {
    Actor::Person { name: "you".to_owned() }
}

/// A state a window shows: the folder's live document, or a point of a branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    /// The live document of the folder branch.
    Now,
    /// That point.
    Point(PointId),
}

impl Shown {
    /// The state as the ops name it.
    pub fn state_ref(&self) -> omuzaki_history_api::state::StateRef {
        match self {
            Self::Now => omuzaki_history_api::state::StateRef::Now,
            Self::Point(id) => omuzaki_history_api::state::StateRef::point(*id),
        }
    }

    /// Its tree: the runner's document for now, the store's for a point.
    ///
    /// # Errors
    /// In words, when it can't be read.
    pub fn tree(&self, runner: &dyn Runner, trees: &dyn PointTrees) -> Result<Arc<Value>, String> {
        match self {
            Self::Now => runner
                .get(&DocPath::root(), &Grants::person())
                .map(|tree| Arc::new(tree.unwrap_or_default()))
                .map_err(|e| e.message),
            Self::Point(id) => trees.tree(id),
        }
    }
}

/// The shot every branch picture is taken with: the same camera for every state, so only the state differs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Degrees around the character.
    pub yaw: f32,
    /// Degrees from above.
    pub pitch: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { yaw: 20.0, pitch: 0.0 }
    }
}

/// The picture size: a portrait frame, small enough that six render quickly.
pub const PICTURE: [u32; 2] = [240, 320];

impl Camera {
    /// `render.preview`'s arguments for `character` at this camera.
    pub fn shot(&self, character: &CharId) -> Preview {
        Preview {
            character: character.clone(),
            state: None,
            variant: None,
            scene: None,
            camera: None,
            framing: Some(omuzaki_stage_api::Framing::Full),
            yaw: Some(self.yaw),
            pitch: Some(self.pitch),
            zoom: None,
            light_rig: None,
            expression: None,
            weight: 1.0,
            pose: None,
            size: PICTURE,
            quality: omuzaki_stage::ops::render::job::RenderQuality::Live,
            preset: None,
        }
    }

    /// The key part for this camera, so a turned camera asks for new pictures.
    pub fn key(&self, character: &CharId) -> String {
        format!("{character}@{:.0},{:.0}", self.yaw, self.pitch)
    }
}

/// The character the pictures show: the one open in the studio when the tree has it, else the tree's first.
pub fn subject(tree: &Value, open: Option<&CharId>) -> Option<CharId> {
    let characters = tree.get("characters")?.as_object()?;
    open.filter(|id| characters.contains_key(id.as_str()))
        .cloned()
        .or_else(|| characters.keys().find_map(|id| CharId::new(id).ok()))
}

/// The branches as the menu lists them, kept current from the lane manager's events.
#[derive(Default)]
pub struct Listing {
    /// Every branch, the bin's too, by name.
    pub branches: Vec<BranchInfo>,
    /// The folder branch.
    pub folder: Option<BranchId>,
    events: Option<Receiver<BranchEvent>>,
    stale: bool,
}

impl std::fmt::Debug for Listing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listing")
            .field("branches", &self.branches.len())
            .field("folder", &self.folder)
            .finish_non_exhaustive()
    }
}

impl Listing {
    /// Reads the branches again when anything happened to them since the last read; true when it did.
    pub fn follow(&mut self, branching: &Branching) -> bool {
        let events = self.events.get_or_insert_with(|| {
            self.stale = true;
            branching.branches.subscribe()
        });
        while events.try_recv().is_ok() {
            self.stale = true;
        }
        if !self.stale {
            return false;
        }
        self.stale = false;
        let mut branches = branching.branches.list();
        branches.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        self.branches = branches;
        self.folder = Some(branching.branches.folder());
        true
    }

    /// Reads them again on the next [`follow`](Self::follow).
    pub fn refresh(&mut self) {
        self.stale = true;
    }

    /// The folder branch's ref.
    pub fn folder_branch(&self) -> Option<&BranchInfo> {
        let folder = self.folder.as_ref()?;
        self.branches.iter().find(|b| &b.id == folder)
    }

    /// The folder branch's name: its ref's, or, before the project's first branch writes `main` down, its id's.
    pub fn folder_name(&self) -> String {
        self.folder_branch()
            .map(|b| b.name.to_string())
            .or_else(|| self.folder.as_ref().map(|id| id.as_str().to_owned()))
            .unwrap_or_default()
    }

    /// The branch called `name`.
    pub fn named(&self, name: &str) -> Option<&BranchInfo> {
        self.branches.iter().find(|b| b.name.as_str() == name)
    }

    /// The live branches whose latest review waits for the person.
    pub fn ready(&self) -> impl Iterator<Item = &BranchInfo> {
        self.branches.iter().filter(|b| {
            b.deleted.is_none()
                && b.review.as_ref().is_some_and(|r| r.state == omuzaki_history_api::branch::ReviewState::Ready)
        })
    }
}

/// What a branch window asks the shell to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    /// The folder moved to another branch: every panel reads it again.
    Switched,
    /// Compare these branches.
    Compare(Vec<BranchId>),
    /// Merge this branch, at this point (a reviewed one) or its head, into the folder branch.
    Merge(BranchId, Option<PointId>),
}

/// The branch windows the shell holds: the menu, and the compare and merge windows while they are open.
#[derive(Debug, Default)]
pub struct BranchUi {
    /// The chip's menu.
    pub menu: menu::Menu,
    compare: Option<compare::BranchCompare>,
    merge: Option<merge::MergeView>,
}

impl BranchUi {
    /// The compare window, while it is open.
    pub const fn compare(&self) -> Option<&compare::BranchCompare> {
        self.compare.as_ref()
    }

    /// The merge view, while it is open.
    pub const fn merge(&self) -> Option<&merge::MergeView> {
        self.merge.as_ref()
    }

    /// The merge view, while it is open, to act on it.
    pub const fn merge_mut(&mut self) -> Option<&mut merge::MergeView> {
        self.merge.as_mut()
    }

    /// Draws whatever is open; true when the folder moved to another branch, so the shell follows it.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        branching: &Branching,
        runner: &dyn Runner,
        focus: Option<&crate::panels::edit::EditFocus>,
    ) -> bool {
        let mut asked = self.menu.show(ctx, branching);
        if let Some(window) = &mut self.compare {
            let listing = self.menu.listing(branching);
            let (from_compare, open) = window.show(ctx, branching, listing, runner, focus);
            if !open {
                self.compare = None;
            }
            asked = asked.or(from_compare);
        }
        if let Some(view) = &mut self.merge
            && !view.show(ctx, branching, runner)
        {
            self.merge = None;
        }
        match asked {
            Some(Asked::Switched) => {
                self.compare = None;
                self.merge = None;
                return true;
            }
            Some(Asked::Compare(ids)) => {
                let listing = self.menu.listing(branching);
                self.compare = Some(compare::BranchCompare::open(&ids, listing, branching, runner));
            }
            Some(Asked::Merge(id, at)) => {
                let listing = self.menu.listing(branching);
                let into = listing.folder_name();
                if let Some(from) = listing.branches.iter().find(|b| b.id == id).cloned() {
                    self.merge = Some(merge::MergeView::open(from, at, into, branching, runner));
                }
            }
            None => {}
        }
        false
    }

    /// Opens the compare of `ids` (tests, and the History panel's "Compare with a branch").
    pub fn open_compare(&mut self, ids: &[BranchId], branching: &Branching, runner: &dyn Runner) {
        let listing = self.menu.listing(branching);
        self.compare = Some(compare::BranchCompare::open(ids, listing, branching, runner));
    }

    /// Opens the merge view of `from` at `at` into the folder branch.
    pub fn open_merge(&mut self, from: &BranchId, at: Option<PointId>, branching: &Branching, runner: &dyn Runner) {
        let listing = self.menu.listing(branching);
        let into = listing.folder_name();
        if let Some(branch) = listing.branches.iter().find(|b| &b.id == from).cloned() {
            self.merge = Some(merge::MergeView::open(branch, at, into, branching, runner));
        }
    }
}

/// A value short enough for a row.
pub fn short(value: Option<&Value>) -> String {
    let text = match value {
        None => "\u{2205}".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.as_f64().map_or_else(|| n.to_string(), |f| format!("{f:.3}")),
        Some(other) => other.to_string(),
    };
    if text.chars().count() > 32 { format!("{}\u{2026}", text.chars().take(31).collect::<String>()) } else { text }
}
