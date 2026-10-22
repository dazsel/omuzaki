//! Apply on the render thread (design D10 steps 2 and 3, ADR 0024): a [`ResolvedScene`] reaches the renderer's
//! [`Scene`] without allocating, and what needs more than a copy is reported as work owed.
//!
//! Three pieces, none of which touch the renderer or the stage's character, so they can be tested and measured alone:
//! - [`Mailbox`]: a one-slot hand-off, newest wins, so a caller on another thread gives the render thread a resolved
//!   scene without the render thread waiting for it.
//! - [`fill`]: copies the active camera, the lights, the background, the post look and the outline light into the
//!   [`Scene`] a frame renders. Hot: after [`reserve`] it allocates nothing.
//! - [`Applier`]: remembers the ground and the skin floor last committed and says, by value, which of them the next
//!   scene changes. Both need renderer resources (a disc mesh, a material update), so they are [`Owed`] to whoever
//!   owns the renderer, only when they change, and stay owed until the caller commits that the work succeeded.

use std::sync::{Mutex, PoisonError, TryLockError};

use omuzaki_render_api::Scene;
use omuzaki_render_api::limits::MAX_LIGHTS;
use omuzaki_stage_api::GroundKind;

use super::resolve::{ResolvedGround, ResolvedScene};
use omuzaki_render_api::Camera;

/// Makes room in `scene` for the most lights a scene may have, so [`fill`] never grows the list. Cold.
pub fn reserve(scene: &mut Scene) {
    scene.lights.reserve(MAX_LIGHTS.saturating_sub(scene.lights.len()));
}

/// Copies `resolved` into `scene`: the active camera, the lights, the background, the post look and the outline light.
/// The instances, the joints, the frame info and what a scene leaves to the caller (fog, environment) stay as they are.
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
    scene.post = resolved.post;
    scene.outline_light = resolved.outline_light;
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
}

impl Owed {
    /// Nothing is owed: the change was values only, or there was none.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What the render thread has applied so far. Starts at "no ground, no override", the state of a stage that has not
/// been given a scene.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Applier {
    ground: Option<ResolvedGround>,
    skin_floor: Option<f32>,
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
    /// missing one are the same thing), and the skin floor when the override is set, changed or cleared.
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
    /// What the last scene wants (its ground and skin floor), in a resolved scene that is never rendered, and a scene
    /// to work the debt out in. Behind a box so the stage stays small.
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
        // Only the two things a debt is made of are kept, for `commit` to compare again.
        self.debt.wanted.ground = resolved.ground;
        self.debt.wanted.skin_floor = resolved.skin_floor;
        self.debt.set = true;
        self.owed
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

/// A one-slot, newest-wins hand-off of resolved scenes from any thread to the render thread.
///
/// The caller [`post`](Self::post)s; the render thread [`take_into`](Self::take_into)s once per frame. Taking swaps the
/// posted scene with the render thread's own, so it neither allocates nor frees, and it never waits: a poster holding
/// the slot makes the take report nothing new, and the next frame tries again.
#[derive(Debug, Default)]
pub struct Mailbox {
    slot: Mutex<Posted>,
}

#[derive(Debug, Default)]
struct Posted {
    scene: ResolvedScene,
    fresh: bool,
}

impl Mailbox {
    /// An empty mailbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Posts `scene`, replacing one the render thread has not taken yet. Called off the render thread: it may wait for
    /// the render thread's short take, and frees the scene it replaces.
    pub fn post(&self, scene: ResolvedScene) {
        let mut posted = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        posted.scene = scene;
        posted.fresh = true;
    }

    /// Moves a newly posted scene into `mine` and returns true; false when nothing new was posted or a poster holds the
    /// slot right now. `mine` goes back into the slot as the stale scene the next [`post`](Self::post) replaces.
    /// Never blocks and never allocates.
    pub fn take_into(&self, mine: &mut ResolvedScene) -> bool {
        let mut posted = match self.slot.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return false,
        };
        if !posted.fresh {
            return false;
        }
        std::mem::swap(&mut posted.scene, mine);
        posted.fresh = false;
        true
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
        resolved.ground = ResolvedGround { kind: GroundKind::None, opacity: 0.5, radius_m: 3.0 };
        assert!(applier.apply(&resolved, &mut scene).is_empty(), "no ground to begin with, none wanted");
    }
}
