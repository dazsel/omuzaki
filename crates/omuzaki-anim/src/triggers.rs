//! Expression triggers (spec PF-25, FE-03 fades): a hotkey shows an expression, faded in and out, written into
//! [`LayerStack::trigger`](omuzaki_anim_api::LayerStack).
//!
//! [`Triggers`] holds up to [`MAX_TRIGGERS`] slots, each an expression already baked to a target buffer. One slot shows
//! at a time. Going from A to B fades A out completely and then fades B in, starting on the step A ends, so the layer
//! never carries values from two expressions, and two hard cuts (fades of 0) switch faces on one step. The layer's
//! weight is the fade, which hands channel ownership over at [`OWN_THRESHOLD`](omuzaki_anim_api::OWN_THRESHOLD)
//! (`blend.rs`). Which keys press which slot is up to the caller.
//!
//! A slot plays the way its expression's [`Playback`](omuzaki_face_api::Playback) says ([`expression_slot`]): its own
//! fades, an auto-stop that hides it on its own, and the momentary layer: a momentary slot shown over a held one gives
//! the face back to the held one when it ends, as the expression player does for a wink over a smile.

use omuzaki_anim_api::{BlendMode, ChannelBuf, ChannelMask, Fade, Layer};
use omuzaki_face_api::{ExpressionId, ExpressionSet, PlaybackLayer, UnknownExpression, narrow};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How many slots there are.
pub const MAX_TRIGGERS: usize = 16;

/// The longest step [`Triggers::step`] takes, so a stalled frame cannot snap a fade to its end.
const MAX_DT: f32 = 0.25;

/// What a key does.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriggerMode {
    /// Each press shows the slot, or hides it if it is already the one shown.
    Toggle,
    /// The slot shows while the key is down.
    Hold,
}

/// Fade times in seconds. Default: in 0.12, out 0.2.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerFades {
    /// Seconds to fade a slot from 0 to 1.
    pub in_s: f32,
    /// Seconds to fade a slot from 1 to 0.
    pub out_s: f32,
}

impl Default for TriggerFades {
    fn default() -> Self {
        Self { in_s: 0.12, out_s: 0.2 }
    }
}

/// One slot: the channels to show (absolute values) and which of them the trigger overrides.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TriggerSlot {
    /// The values to show; only channels in `mask` are used.
    pub target: ChannelBuf,
    /// The channels the trigger overrides.
    pub mask: ChannelMask,
    /// What a key does.
    pub mode: TriggerMode,
    /// Its own fades; `None` uses the ones the triggers were made with.
    pub fades: Option<TriggerFades>,
    /// Seconds after it starts showing that it hides on its own; `None` stays until the key hides it.
    pub auto_stop_s: Option<f32>,
    /// It plays over a held slot: shown while another (held) slot is wanted, it gives the face back to that one when
    /// it ends.
    pub momentary: bool,
}

/// Why a slot could not be set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TriggerError {
    /// The slot index is not below [`MAX_TRIGGERS`].
    #[error("trigger slot {slot} is out of range (0..{MAX_TRIGGERS})")]
    SlotOutOfRange {
        /// The index given.
        slot: usize,
    },
    /// The slot's mask has no channels, so it would show nothing.
    #[error("a trigger must override at least one channel")]
    EmptyMask,
}

/// The slot on the layer, with its own copy of the data so replacing the slot never cuts the fade-out short.
#[derive(Copy, Clone, Debug)]
struct Shown {
    index: usize,
    slot: TriggerSlot,
    /// The slot was set again since this copy was taken: the copy only fades out, never back in, and whatever the
    /// slot holds then comes in from zero.
    replaced: bool,
    /// Seconds it has been wanted, for its auto-stop.
    shown_for: f32,
}

/// Up to [`MAX_TRIGGERS`] expression slots, one shown at a time, faded.
#[derive(Clone, Debug)]
pub struct Triggers {
    slots: [Option<TriggerSlot>; MAX_TRIGGERS],
    fades: TriggerFades,
    shown: Option<Shown>,
    wanted: Option<usize>,
    /// The held slot a momentary one took the face from, given back when the momentary one ends.
    resume: Option<usize>,
    fade: Fade,
    /// How many times a slot has come onto the layer: a caller that mirrors which slot shows tells a new showing
    /// from the same slot still showing.
    shows: u64,
}

impl Triggers {
    /// No slots set, nothing shown.
    pub fn new(fades: TriggerFades) -> Self {
        Self { slots: [None; MAX_TRIGGERS], fades, shown: None, wanted: None, resume: None, fade: Fade(0.0), shows: 0 }
    }

    /// Replaces a slot (`None` empties it). If it is shown, the old values fade out completely first, even if the
    /// slot is pressed again meanwhile; then the new values fade in.
    ///
    /// # Errors
    /// [`TriggerError::SlotOutOfRange`] for a slot past the last, [`TriggerError::EmptyMask`] for a slot that overrides
    /// nothing.
    pub fn set_slot(&mut self, slot: usize, value: Option<TriggerSlot>) -> Result<(), TriggerError> {
        let entry = self.slots.get_mut(slot).ok_or(TriggerError::SlotOutOfRange { slot })?;
        if value.is_some_and(|v| v.mask.is_empty()) {
            return Err(TriggerError::EmptyMask);
        }
        *entry = value;
        if let Some(shown) = self.shown.as_mut()
            && shown.index == slot
        {
            shown.replaced = true;
        }
        if self.wanted == Some(slot) {
            self.wanted = None;
        }
        if self.resume == Some(slot) {
            self.resume = None;
        }
        Ok(())
    }

    /// Key down. Toggle: shows the slot, or hides it if it is the one wanted. Hold: shows it.
    /// An empty or out-of-range slot is ignored. A momentary slot shown over a held one keeps the held one to give the
    /// face back to; any other slot forgets it.
    pub fn press(&mut self, slot: usize) {
        let Some(Some(s)) = self.slots.get(slot) else { return };
        if s.mode == TriggerMode::Toggle && self.wanted == Some(slot) {
            self.hide_wanted();
            return;
        }
        if s.momentary {
            let held = self.wanted.filter(|&w| w != slot && matches!(self.slots.get(w), Some(Some(o)) if !o.momentary));
            self.resume = held.or(self.resume);
        } else {
            self.resume = None;
        }
        self.wanted = Some(slot);
        if let Some(shown) = self.shown.as_mut().filter(|shown| shown.index == slot) {
            // Pressed again while it shows: its auto-stop starts over.
            shown.shown_for = 0.0;
        }
    }

    /// Key up. Hold: hides the slot if it is the one wanted. Toggle: nothing.
    pub fn release(&mut self, slot: usize) {
        let held = matches!(self.slots.get(slot), Some(Some(s)) if s.mode == TriggerMode::Hold);
        if held && self.wanted == Some(slot) {
            self.hide_wanted();
        }
    }

    /// Hides the wanted slot: back to the held slot a momentary one took over from, if any, else nothing.
    fn hide_wanted(&mut self) {
        self.wanted = self.resume.take().filter(|&r| matches!(self.slots.get(r), Some(Some(_))));
    }

    /// Puts the wanted slot on the layer, from zero, when nothing shows.
    fn bring_in(&mut self) {
        if self.shown.is_none()
            && let Some(index) = self.wanted
            && let Some(Some(slot)) = self.slots.get(index)
        {
            self.shown = Some(Shown { index, slot: *slot, replaced: false, shown_for: 0.0 });
            self.fade = Fade(0.0);
            self.shows = self.shows.wrapping_add(1);
        }
    }

    /// Advances the fade by `dt` seconds (clamped to `0..=0.25`, NaN counts as 0) and writes `out`: the shown slot's
    /// values and mask, [`BlendMode::Override`], weight = the fade. Clears `out` when nothing shows. Hot path: no
    /// allocation.
    pub fn step(&mut self, dt: f32, out: &mut Layer) {
        let dt = if dt.is_nan() { 0.0 } else { dt.clamp(0.0, MAX_DT) };
        self.bring_in();
        if let Some(shown) = self.shown.as_mut()
            && self.wanted == Some(shown.index)
            && !shown.replaced
        {
            shown.shown_for += dt;
            if shown.slot.auto_stop_s.is_some_and(|stop| shown.shown_for >= stop) {
                self.hide_wanted();
            }
        }
        if let Some(shown) = self.shown {
            let fades = shown.slot.fades.unwrap_or(self.fades);
            if self.wanted == Some(shown.index) && !shown.replaced {
                self.fade.toward(1.0, dt, fades.in_s);
            } else if self.fade.toward(0.0, dt, fades.out_s) <= 0.0 {
                // Fully out: whatever is wanted comes in on this step, from zero, so the layer holds one expression
                // at a time and a cut needs no empty step between the two faces.
                self.shown = None;
                self.bring_in();
                if let Some(next) = self.shown {
                    self.fade.toward(1.0, dt, next.slot.fades.unwrap_or(self.fades).in_s);
                }
            }
        }
        match self.shown {
            Some(shown) if self.fade.0 > 0.0 => {
                out.values = shown.slot.target;
                out.mask = shown.slot.mask;
                out.mode = BlendMode::Override;
                out.weight = self.fade.0;
            }
            _ => out.clear(),
        }
    }

    /// The slot being faded toward, else the one fading out; None when idle.
    pub fn active(&self) -> Option<usize> {
        self.wanted.or(self.shown.map(|s| s.index))
    }

    /// The slot being faded toward or held, if any.
    pub fn wanted(&self) -> Option<usize> {
        self.wanted
    }

    /// The slot on the layer (fading in, held or fading out), if any.
    pub fn shown(&self) -> Option<usize> {
        self.shown.map(|s| s.index)
    }

    /// How many times a slot has come onto the layer so far: it changes exactly when a new showing starts.
    pub fn shows(&self) -> u64 {
        self.shows
    }

    /// Fades out whatever is shown.
    pub fn clear(&mut self) {
        self.wanted = None;
        self.resume = None;
    }
}

/// A slot that shows `id` at full weight: target = `set.blend(rest, &[(id, 1.0)])`, mask = the channels in the
/// expression's deltas, and its fades, auto-stop and layer from the expression's playback (a `fade_in_s` of 0 is a hard
/// cut).
///
/// # Errors
/// [`UnknownExpression`] if `set` has no expression `id`.
pub fn expression_slot(
    set: &ExpressionSet,
    id: &ExpressionId,
    rest: &ChannelBuf,
    mode: TriggerMode,
) -> Result<TriggerSlot, UnknownExpression> {
    let target = set.blend(rest, &[(id, 1.0)])?;
    let expression = set.0.get(id).ok_or_else(|| UnknownExpression(id.clone()))?;
    let mask = expression.deltas.0.keys().copied().collect();
    let playback = &expression.playback;
    Ok(TriggerSlot {
        target,
        mask,
        mode,
        fades: Some(TriggerFades { in_s: narrow(playback.fade_in_s), out_s: narrow(playback.fade_out_s) }),
        auto_stop_s: playback.auto_stop_s.map(narrow),
        momentary: playback.layer == PlaybackLayer::Momentary,
    })
}
