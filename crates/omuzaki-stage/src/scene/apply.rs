//! Apply on the render thread (design D10 steps 2 and 3, ADR 0024): a [`ResolvedScene`] reaches the renderer's
//! [`Scene`] without allocating, and what needs more than a copy is reported as work owed.
//!
//! Three pieces, none of which touch the renderer or the stage's character, so they can be tested and measured alone:
//! - [`Mailbox`]: a one-slot hand-off, newest wins, so a caller on another thread gives the render thread a resolved
//!   scene without the render thread waiting for it.
//! - [`fill`]: copies the active camera, the lights, the background, the post look and the outline light into the
//!   [`Scene`] a frame renders. Hot: after [`reserve`] it allocates nothing.
//! - [`Applier`]: remembers the ground and the skin floor last applied and says, by value, which of them the next
//!   scene changes. Both need renderer resources (a disc mesh, a material update), so they are [`Owed`] to whoever
//!   owns the renderer, and only when they change.

use std::sync::{Mutex, PoisonError, TryLockError};

use omuzaki_render_api::Scene;
use omuzaki_render_api::limits::MAX_LIGHTS;
use omuzaki_stage_api::GroundKind;

use super::resolve::{ResolvedGround, ResolvedScene};

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

    /// Fills `scene` from `resolved` (see [`fill`]) and says what else the renderer is owed.
    ///
    /// The ground is owed when its kind, radius or opacity differs from the last applied (a `None` ground and a missing
    /// one are the same thing), and the skin floor when the override is set, changed or cleared. Allocation-free once
    /// [`reserve`] has run on `scene`.
    pub fn apply(&mut self, resolved: &ResolvedScene, scene: &mut Scene) -> Owed {
        fill(resolved, scene);
        let wanted_ground = (resolved.ground.kind != GroundKind::None).then_some(resolved.ground);
        let mut owed = Owed::default();
        if wanted_ground != self.ground {
            self.ground = wanted_ground;
            owed.ground = Some(wanted_ground.unwrap_or(ResolvedGround { kind: GroundKind::None, ..resolved.ground }));
        }
        if resolved.skin_floor != self.skin_floor {
            self.skin_floor = resolved.skin_floor;
            owed.skin_floor = Some(resolved.skin_floor);
        }
        owed
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
        let mut applier = Applier::new();
        let mut scene = Scene::new(omuzaki_render_api::Camera::default());
        let mut resolved = ResolvedScene::new();
        resolved.ground = ResolvedGround { kind: GroundKind::None, opacity: 0.5, radius_m: 3.0 };
        assert!(applier.apply(&resolved, &mut scene).is_empty(), "no ground to begin with, none wanted");
    }
}
