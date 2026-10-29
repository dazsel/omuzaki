//! Apply on the render thread (design D10 steps 2 and 3, ADR 0024): a [`ResolvedScene`] reaches the renderer's
//! [`Scene`] without allocating, and what needs more than a copy is reported as work owed.
//!
//! Three pieces, none of which touch the renderer or the stage's character, so they can be tested and measured alone:
//! - [`Mailbox`]: a lock-free triple buffer, newest wins, so a caller on another thread gives the render thread a resolved
//!   scene without the render thread waiting for it.
//! - [`fill`]: copies the active camera, the lights, the background, the fog, the post look and the outline light into
//!   the [`Scene`] a frame renders. Hot: after [`reserve`] it allocates nothing.
//! - [`Applier`]: remembers the ground, the skin floor and the environment's source last committed and says, by value,
//!   which of them the next scene changes. They need renderer resources (a disc mesh, a material update, an
//!   environment), so they are [`Owed`] to whoever owns the renderer, only when they change, and stay owed until the
//!   caller commits that the work succeeded.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, PoisonError};

use omuzaki_render_api::Scene;
use omuzaki_render_api::limits::MAX_LIGHTS;
use omuzaki_stage_api::{EnvironmentSource, GroundKind};
use omuzaki_store_api::ContentHash;

use super::resolve::{ResolvedGround, ResolvedScene};
use omuzaki_render_api::Camera;

/// Makes room in `scene` for the most lights a scene may have, so [`fill`] never grows the list. Cold.
pub fn reserve(scene: &mut Scene) {
    scene.lights.reserve(MAX_LIGHTS.saturating_sub(scene.lights.len()));
}

/// Copies `resolved` into `scene`: the active camera, the lights, the background, the fog, the post look and the outline
/// light. The instances, the joints, the frame info and the environment (whose handle the stage holds) stay as they are.
///
/// A resolved scene without cameras leaves the camera of `scene` alone, so a scene with no camera keeps the one the
/// stage was made with. Allocation-free once [`reserve`] has run on `scene`.
pub fn fill(resolved: &ResolvedScene, scene: &mut Scene) {
    if let Some((_, camera)) = resolved.active.and_then(|i| resolved.cameras.get(i)) {
        scene.camera = *camera;
    }
    scene.lights.clear();
    scene.lights.extend_from_slice(resolved.lights.as_slice());
    scene.background = resolved.background;
    scene.fog = resolved.fog;
    scene.post = resolved.post;
    scene.outline_light = resolved.outline_light;
    scene.back_light_rim = resolved.back_light_rim;
}

/// What a scene change owes the renderer beyond [`fill`]'s copy.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Owed {
    /// The ground disc to have from now on. `None` when it is as before.
    pub ground: Option<ResolvedGround>,
    /// The override for every skin material's shadow floor: `Some(Some(v))` to set or change it, `Some(None)` to
    /// clear it (back to each material's own floor), `None` when it is as before.
    pub skin_floor: Option<Option<f32>>,
    /// The environment to make from now on: `Some(Some(source))` for a new sky or HDRI, `Some(None)` to release it,
    /// `None` when it is as before. Its intensity, turn and toon ramp are values and owe nothing.
    pub environment: Option<Option<EnvironmentSource>>,
    /// The backdrop picture to upload from now on: `Some(Some(asset))` for a new one, `Some(None)` to release it,
    /// `None` when it is as before. Its fit and bars are values and owe nothing.
    pub image: Option<Option<ContentHash>>,
    /// The generation of the prop builds to upload from now on ([`PropBuilds`](crate::props::staged::PropBuilds)),
    /// `None` when the renderer has them. Moving a prop is a value and owes nothing.
    pub props: Option<u64>,
}

impl Owed {
    /// Nothing is owed: the change was values only, or there was none.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What the render thread has applied so far. Starts at "no ground, no override, no environment", the state of a stage
/// that has not been given a scene.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Applier {
    ground: Option<ResolvedGround>,
    skin_floor: Option<f32>,
    environment: Option<EnvironmentSource>,
    image: Option<ContentHash>,
    props: u64,
}

impl Applier {
    /// A new applier: nothing applied yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fills `scene` from `resolved` (see [`fill`]) and says what else the renderer is owed. It records nothing: the
    /// caller does the work, then calls [`commit`](Self::commit), so a renderer error leaves the same work owed on the
    /// next call instead of the applier believing it was done.
    ///
    /// The ground is owed when its kind, radius or opacity differs from the last committed (a `None` ground and a
    /// missing one are the same thing), the skin floor when the override is set, changed or cleared, and the
    /// environment when its source is added, changed or removed, and the backdrop picture when its asset is.
    /// Allocation-free once [`reserve`] has run on `scene`.
    pub fn apply(&self, resolved: &ResolvedScene, scene: &mut Scene) -> Owed {
        fill(resolved, scene);
        let wanted_ground = (resolved.ground.kind != GroundKind::None).then_some(resolved.ground);
        let mut owed = Owed::default();
        if wanted_ground != self.ground {
            owed.ground = Some(wanted_ground.unwrap_or(ResolvedGround { kind: GroundKind::None, ..resolved.ground }));
        }
        if resolved.skin_floor != self.skin_floor {
            owed.skin_floor = Some(resolved.skin_floor);
        }
        let source = resolved.environment.map(|e| e.source);
        if source != self.environment {
            owed.environment = Some(source);
        }
        let image = resolved.backdrop_image.map(|i| i.asset);
        if image != self.image {
            owed.image = Some(image);
        }
        if resolved.props.builds.generation != self.props {
            owed.props = Some(resolved.props.builds.generation);
        }
        owed
    }

    /// Records that everything in `owed` reached the renderer. Call it only after every operation succeeded; after a
    /// failure, call [`apply`](Self::apply) again and the same work is owed.
    pub fn commit(&mut self, owed: &Owed) {
        if let Some(ground) = owed.ground {
            self.ground = (ground.kind != GroundKind::None).then_some(ground);
        }
        if let Some(skin_floor) = owed.skin_floor {
            self.skin_floor = skin_floor;
        }
        if let Some(environment) = owed.environment {
            self.environment = environment;
        }
        if let Some(image) = owed.image {
            self.image = image;
        }
        if let Some(props) = owed.props {
            self.props = props;
        }
    }
}

/// A stage's side of the scene: what it has committed to the renderer, what the last scene still owes, and the
/// scratch to work that debt out again after a commit. Every stage (a kit character's, an imported avatar's) keeps
/// one, so the rules of [`Applier`] are written once.
///
/// Sized when it is made, so setting a scene allocates nothing, the first one included.
#[derive(Debug)]
pub(crate) struct SceneLink {
    applier: Applier,
    owed: Owed,
    /// What the last scene wants (its ground, skin floor, environment and backdrop picture, with their pixels), in a
    /// resolved scene that is never rendered, and a scene to work the debt out in. Behind a box so the stage stays
    /// small.
    debt: Box<Debt>,
}

#[derive(Debug)]
struct Debt {
    wanted: ResolvedScene,
    scratch: Scene,
    /// Whether a scene was ever set: until then nothing is wanted.
    set: bool,
}

impl SceneLink {
    /// Nothing committed, nothing owed. Cold: allocates the scratch.
    pub(crate) fn new() -> Self {
        Self {
            applier: Applier::new(),
            owed: Owed::default(),
            debt: Box::new(Debt { wanted: ResolvedScene::new(), scratch: Scene::new(Camera::default()), set: false }),
        }
    }

    /// Copies `resolved` into `scene` and returns what it owes the renderer beyond the copy, which stays owed until
    /// [`commit`](Self::commit). Allocation-free once [`reserve`] has run on `scene`.
    pub(crate) fn set(&mut self, resolved: &ResolvedScene, scene: &mut Scene) -> Owed {
        self.owed = self.applier.apply(resolved, scene);
        // Only what a debt is made of is kept, for `commit` to compare again and the stage to pay from. The pixels are
        // shared, so keeping them costs a reference count.
        self.debt.wanted.ground = resolved.ground;
        self.debt.wanted.skin_floor = resolved.skin_floor;
        self.debt.wanted.environment = resolved.environment;
        self.debt.wanted.hdri.clone_from(&resolved.hdri);
        self.debt.wanted.backdrop_image = resolved.backdrop_image;
        self.debt.wanted.image.clone_from(&resolved.image);
        self.debt.wanted.props.builds.clone_from(&resolved.props.builds);
        self.debt.set = true;
        self.owed
    }

    /// What the last [`set`](Self::set) wants: its ground, skin floor, environment, backdrop picture and their pixels,
    /// and its prop builds (nothing else of it is kept).
    pub(crate) fn wanted(&self) -> &ResolvedScene {
        &self.debt.wanted
    }

    /// What the last [`set`](Self::set) still owes the renderer.
    pub(crate) fn owed(&self) -> Owed {
        self.owed
    }

    /// Records that the renderer did the work in `owed` and works out again what the current scene still owes.
    pub(crate) fn commit(&mut self, owed: &Owed) {
        self.applier.commit(owed);
        if self.debt.set {
            self.owed = self.applier.apply(&self.debt.wanted, &mut self.debt.scratch);
        }
    }
}

/// A newest-wins hand-off of resolved scenes from any thread to the render thread, lock-free on the render side.
///
/// A triple buffer: three scenes, one held by the posting side, one by the render thread and one between them, with the
/// index of the one between and whether it is fresh in one atomic. The caller [`post`](Self::post)s (posts take turns
/// behind a lock, off the render thread); the render thread [`take_into`](Self::take_into)s once per frame, with two
/// atomic exchanges and a swap of two scenes: it never waits, locks, allocates or frees.
pub struct Mailbox {
    /// The three scenes. Each index belongs to exactly one of `poster`, `middle` and `taker` at any time, and only the
    /// holder of an index touches its scene.
    slots: [UnsafeCell<ResolvedScene>; 3],
    /// The index of the scene between the two sides, with [`FRESH`] while it holds a post not taken yet.
    middle: AtomicU8,
    /// The posting side's index. The lock makes posts from several threads take turns; the render thread never takes it.
    poster: Mutex<u8>,
    /// The render thread's index, with [`TAKING`] while a take runs, so a second taker backs off instead of sharing it.
    taker: AtomicU8,
}

/// The bits of an index.
const INDEX: u8 = 0b11;
/// In `middle`: the scene there was posted and not taken.
const FRESH: u8 = 0b100;
/// In `taker`: a take is running.
const TAKING: u8 = 0b1000;

// SAFETY: every scene is touched only by the holder of its index (the poster under its lock, the taker under `TAKING`),
// indices change hands only through `middle`'s atomic exchanges, which order each side's writes before the other side's
// reads, and `ResolvedScene` is `Send`.
#[allow(
    unsafe_code,
    reason = "the triple buffer shares its scenes by index; the SAFETY comment says why that is sound"
)]
unsafe impl Sync for Mailbox {}

impl std::fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailbox").field("fresh", &self.has_posted()).finish_non_exhaustive()
    }
}

impl Default for Mailbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Mailbox {
    /// An empty mailbox: three empty scenes, each with room for every camera, so a take never grows the render
    /// thread's scene.
    pub fn new() -> Self {
        Self {
            slots: [(); 3].map(|()| UnsafeCell::new(ResolvedScene::new())),
            middle: AtomicU8::new(1),
            poster: Mutex::new(0),
            taker: AtomicU8::new(2),
        }
    }

    /// Whether a scene is posted and not taken yet.
    pub fn has_posted(&self) -> bool {
        self.middle.load(Ordering::Acquire) & FRESH != 0
    }

    /// Posts `scene`, replacing one the render thread has not taken yet. Called off the render thread: it waits only
    /// for another post, and frees the scene it replaces.
    #[allow(unsafe_code, reason = "the triple buffer shares its scenes by index; each use says why it is sound")]
    pub fn post(&self, scene: ResolvedScene) {
        let mut mine = self.poster.lock().unwrap_or_else(PoisonError::into_inner);
        let at = usize::from(*mine & INDEX);
        // SAFETY: the poster's index is held under the lock, so nothing else touches this scene; the scene it replaces
        // is dropped here, off the render thread.
        unsafe { *self.slots[at].get() = scene };
        // Release: the write above is visible to the taker that acquires this index. Acquire: the scene that comes back
        // is no longer read by the taker that handed it over.
        let back = self.middle.swap(*mine | FRESH, Ordering::AcqRel);
        *mine = back & INDEX;
    }

    /// Swaps a newly posted scene with `mine` and returns true; false when nothing new was posted (or another thread is
    /// taking right now). `mine` goes back to the posting side, which drops it with its next post. Render thread:
    /// never blocks, locks, allocates or frees.
    #[allow(unsafe_code, reason = "the triple buffer shares its scenes by index; each use says why it is sound")]
    pub fn take_into(&self, mine: &mut ResolvedScene) -> bool {
        let held = self.taker.fetch_or(TAKING, Ordering::Acquire);
        if held & TAKING != 0 {
            return false;
        }
        let mut index = held & INDEX;
        let taken = self.has_posted();
        if taken {
            // Only a take clears FRESH, and this one holds TAKING, so the scene between is still the fresh one.
            index = self.middle.swap(index, Ordering::AcqRel) & INDEX;
            // SAFETY: the exchange handed this index to the taker, which alone uses it while TAKING is set; the
            // poster's writes to it happened before its release, which the exchange acquired.
            std::mem::swap(mine, unsafe { &mut *self.slots[usize::from(index)].get() });
        }
        self.taker.store(index, Ordering::Release);
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_ground_and_a_none_ground_are_the_same_state() {
        let applier = Applier::new();
        let mut scene = Scene::new(omuzaki_render_api::Camera::default());
        let mut resolved = ResolvedScene::new();
        resolved.ground =
            ResolvedGround { kind: GroundKind::None, opacity: 0.5, radius_m: 3.0, ..ResolvedGround::default() };
        assert!(applier.apply(&resolved, &mut scene).is_empty(), "no ground to begin with, none wanted");
    }
}
