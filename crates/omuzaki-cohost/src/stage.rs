//! [`PerformStage`]: the performer's [`Stage`] over the perform host.
//!
//! An `emote` holds the character's own expression trigger for its `hold_s` (bound on first use under an `ai:` key, in
//! `Hold` mode, so it fades in and out with the expression's own playback); one AI face shows at a time, and a cut
//! (`release`) lets go of it at once. The inventory is the character's emotions and accents. Gestures, clips, looks
//! and walks need the score layer's live cues (anim-api §10) and are refused as unsupported until it lands; the
//! performer then lists no clips, so the model is never offered one.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_anim::TriggerMode;
use omuzaki_anim_api::ExpressionId;
use omuzaki_core::time::Seconds;
use omuzaki_face_api::ExpressionKind;
use omuzaki_performer_api::intent::Intent;
use omuzaki_performer_api::stage::{Inventory, Refusal, Stage, StageError};
use omuzaki_stage::perform::host::{PerformError, PerformHost};
use omuzaki_stage::perform::triggers::TriggerBinding;
use omuzaki_stage_api::ids::MemberId;

/// The prefix of the trigger keys the performer binds, so they never take a person's key.
pub const KEY_PREFIX: &str = "ai:";
/// Trigger keys are at most this many characters.
const KEY_CHARS: usize = 32;

#[derive(Default)]
struct State {
    /// Expressions with a bound `ai:` key.
    bound: BTreeSet<ExpressionId>,
    /// The key held down, and until when.
    down: Option<(String, f64)>,
    /// Emotes applied ahead of their time: `(at, key, until)`.
    queued: VecDeque<(f64, String, f64)>,
}

/// The performer's stage for one cast member on one perform host.
pub struct PerformStage {
    host: Arc<dyn PerformHost>,
    member: MemberId,
    state: Mutex<State>,
}

impl PerformStage {
    /// The stage for `member`, played by `host`.
    pub fn new(host: Arc<dyn PerformHost>, member: MemberId) -> Self {
        Self { host, member, state: Mutex::new(State::default()) }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Presses what is due and lets go of what has been held long enough. Called every step.
    pub fn tick(&self, now: Seconds) {
        let mut state = self.lock();
        while state.queued.front().is_some_and(|(at, ..)| *at <= now.0) {
            if let Some((_, key, until)) = state.queued.pop_front() {
                self.hold(&mut state, key, until);
            }
        }
        if state.down.as_ref().is_some_and(|(_, until)| *until <= now.0)
            && let Some((key, _)) = state.down.take()
        {
            let _ = self.host.press(&key, false);
        }
    }

    /// Lets go of every key and unbinds them, so the person's trigger slots are free again (the performer stopped).
    pub fn clear(&self) {
        let mut state = self.lock();
        if let Some((key, _)) = state.down.take() {
            let _ = self.host.press(&key, false);
        }
        state.queued.clear();
        for expression in std::mem::take(&mut state.bound) {
            let _ = self.host.unbind_trigger(&key_of(&expression));
        }
    }

    fn hold(&self, state: &mut State, key: String, until: f64) {
        if let Some((old, _)) = state.down.take()
            && old != key
        {
            let _ = self.host.press(&old, false);
        }
        if self.host.press(&key, true).is_ok() {
            state.down = Some((key, until));
        }
    }

    fn emote(&self, expression: &ExpressionId, hold_s: f32, at: Seconds) -> Result<(), StageError> {
        let key = key_of(expression);
        let mut state = self.lock();
        if !state.bound.contains(expression) {
            let binding = TriggerBinding { key: key.clone(), expression: expression.clone(), mode: TriggerMode::Hold };
            self.host.bind_trigger_checked(binding).map_err(stage_error)?;
            state.bound.insert(expression.clone());
        }
        let until = at.0 + f64::from(hold_s);
        state.queued.push_back((at.0, key, until));
        state.queued.make_contiguous().sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(())
    }
}

impl Stage for PerformStage {
    fn apply(&self, member: &MemberId, intent: &Intent, at: Seconds) -> Result<(), StageError> {
        if member != &self.member {
            return Err(StageError::unknown_member(member));
        }
        let inventory = self.inventory(member)?;
        inventory.check(intent).map_err(StageError::Refused)?;
        match intent {
            Intent::Emote { expression, hold_s, .. } => self.emote(expression, *hold_s, at),
            _ => Err(StageError::Refused(Refusal::Unsupported)),
        }
    }

    fn release(&self, member: &MemberId, from: Seconds) {
        if member != &self.member {
            return;
        }
        let mut state = self.lock();
        state.queued.retain(|(at, ..)| *at < from.0);
        if let Some((key, _)) = state.down.take() {
            let _ = self.host.press(&key, false);
        }
    }

    fn inventory(&self, member: &MemberId) -> Result<Inventory, StageError> {
        if member != &self.member {
            return Err(StageError::unknown_member(member));
        }
        let expressions = self
            .host
            .expressions()
            .0
            .into_iter()
            .filter(|(_, e)| matches!(e.kind, ExpressionKind::Emotion | ExpressionKind::Accent))
            .map(|(id, _)| id)
            .collect();
        Ok(Inventory::new(expressions, Vec::new(), Vec::new(), Vec::new()))
    }
}

impl std::fmt::Debug for PerformStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerformStage").field("member", &self.member).finish_non_exhaustive()
    }
}

/// The trigger key for `expression`.
pub fn key_of(expression: &ExpressionId) -> String {
    format!("{KEY_PREFIX}{expression}").chars().take(KEY_CHARS).collect()
}

fn stage_error(e: PerformError) -> StageError {
    match e {
        PerformError::UnknownExpression => StageError::Refused(Refusal::UnknownExpression),
        PerformError::Disconnected => StageError::NotLive,
        _ => StageError::Refused(Refusal::Unsupported),
    }
}
