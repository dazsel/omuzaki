//! The expression player (spec FE-03): shows and hides a character's named expressions the way their
//! [`Playback`](omuzaki_face_api::Playback) says, and blends what is showing onto a bus.
//!
//! - **Fades.** A shown expression's weight rises to 1 over its `fade_in_s` and falls to 0 over its `fade_out_s`.
//! - **Auto-stop.** An expression with `auto_stop_s` starts fading out that long after it was last shown.
//! - **Exclusive groups.** Showing an expression fades out every other one in its group, so one mood shows at a time;
//!   the two cross-fade.
//! - **Layers.** Momentary expressions play over the held ones: on each channel a momentary one moves, the held ones
//!   step back by its weight and return as it fades, so a wink takes the eye from a smile and gives it back. Channels it
//!   leaves alone keep the held expression.
//! - **The default expression** fills in where nothing held shows: its weight is 1 less the strongest held weight.
//! - **Cuts.** [`ExpressionPlayer::cut`] switches faces on one frame whatever the fades say, as an edit does; an
//!   expression whose `fade_in_s` is 0 cuts in by itself.
//!
//! A short's expression cues play through [`CueTrack`] (`cues.rs`): the cues in time order, each applied at its own
//! time, frame-exact and the same on every run. The live triggers bake the same playback into their slots
//! (`omuzaki_anim::triggers`), since a hotkey's layer shows one expression at a time; lip sync and scores give weights
//! directly and don't go through here. [`ExpressionPlayer::step`] and [`ExpressionPlayer::blend`] don't allocate; showing an expression
//! may, once.

use omuzaki_anim_api::{CHANNEL_CAPACITY, ChannelBuf};
use omuzaki_face_api::{ExpressionId, ExpressionSet, Playback, PlaybackLayer, UnknownExpression, narrow};

mod cues;

pub use cues::{CueAction, CueError, CueTrack, ExpressionCue};

/// How many expressions may show (or fade) at once; showing one more drops the faintest.
pub const MAX_SHOWN: usize = 16;

/// The longest step [`ExpressionPlayer::step`] takes, so a stalled frame can't snap a fade to its end.
const MAX_DT: f32 = 0.25;

/// One expression that is showing or fading out.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    id: ExpressionId,
    fade_in: f32,
    fade_out: f32,
    auto_stop: Option<f32>,
    group: Option<String>,
    layer: PlaybackLayer,
    /// Its fade, `0..=1`.
    weight: f32,
    /// Whether it is fading in (or held at 1) rather than out.
    on: bool,
    /// Seconds since it was last shown.
    shown_for: f32,
}

impl Entry {
    fn new(id: &ExpressionId, playback: &Playback) -> Self {
        Self {
            id: id.clone(),
            fade_in: narrow(playback.fade_in_s).max(0.0),
            fade_out: narrow(playback.fade_out_s).max(0.0),
            auto_stop: playback.auto_stop_s.map(narrow),
            group: playback.group.clone(),
            layer: playback.layer,
            weight: 0.0,
            on: true,
            shown_for: 0.0,
        }
    }

    /// Moves the fade `dt` seconds toward its target.
    fn step(&mut self, dt: f32) {
        if self.on {
            self.shown_for += dt;
            if self.auto_stop.is_some_and(|stop| self.shown_for >= stop) {
                self.on = false;
            }
        }
        let (target, seconds) = if self.on { (1.0, self.fade_in) } else { (0.0, self.fade_out) };
        self.weight = if seconds <= 0.0 {
            target
        } else if self.on {
            (self.weight + dt / seconds).min(1.0)
        } else {
            (self.weight - dt / seconds).max(0.0)
        };
    }
}

/// Shows and hides one character's expressions and blends them (see the module docs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExpressionPlayer {
    entries: Vec<Entry>,
    default: Option<ExpressionId>,
}

impl ExpressionPlayer {
    /// A player with nothing showing, returning to `default` (the face's `default_expression`) when nothing held shows.
    pub fn new(default: Option<ExpressionId>) -> Self {
        Self { entries: Vec::with_capacity(MAX_SHOWN), default }
    }

    /// Changes the default expression.
    pub fn set_default(&mut self, default: Option<ExpressionId>) {
        self.default = default;
    }

    /// Shows `id` with its playback from `set`: it fades in (or keeps showing, its auto-stop restarted), and every other
    /// expression in its group fades out.
    ///
    /// # Errors
    /// [`UnknownExpression`] when `set` has no such expression; nothing changes.
    pub fn show(&mut self, set: &ExpressionSet, id: &ExpressionId) -> Result<(), UnknownExpression> {
        let playback = &set.0.get(id).ok_or_else(|| UnknownExpression(id.clone()))?.playback;
        if let Some(group) = &playback.group {
            for other in self.entries.iter_mut().filter(|e| e.id != *id && e.group.as_ref() == Some(group)) {
                other.on = false;
            }
        }
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == *id) {
            let weight = entry.weight;
            *entry = Entry { weight, ..Entry::new(id, playback) };
            return Ok(());
        }
        if self.entries.len() >= MAX_SHOWN {
            let faintest =
                self.entries.iter().enumerate().min_by(|a, b| a.1.weight.total_cmp(&b.1.weight)).map_or(0, |(i, _)| i);
            self.entries.swap_remove(faintest);
        }
        self.entries.push(Entry::new(id, playback));
        Ok(())
    }

    /// Cuts to `id`: it shows at full weight at once, and whatever it takes over from is gone at once: every other
    /// expression in its group, or, for a held expression in no group, every other held one. A momentary one in no
    /// group cuts in over the held ones and leaves them be.
    ///
    /// # Errors
    /// [`UnknownExpression`] when `set` has no such expression; nothing changes.
    pub fn cut(&mut self, set: &ExpressionSet, id: &ExpressionId) -> Result<(), UnknownExpression> {
        let playback = &set.0.get(id).ok_or_else(|| UnknownExpression(id.clone()))?.playback;
        let (group, layer) = (playback.group.clone(), playback.layer);
        self.entries.retain(|e| {
            let replaced = match &group {
                Some(group) => e.group.as_ref() == Some(group),
                None => layer == PlaybackLayer::Held && e.layer == PlaybackLayer::Held,
            };
            e.id == *id || !replaced
        });
        self.show(set, id)?;
        for entry in self.entries.iter_mut().filter(|e| e.id == *id) {
            entry.weight = 1.0;
        }
        Ok(())
    }

    /// Fades `id` out, if it shows.
    pub fn hide(&mut self, id: &ExpressionId) {
        for entry in self.entries.iter_mut().filter(|e| e.id == *id) {
            entry.on = false;
        }
    }

    /// Fades everything out.
    pub fn hide_all(&mut self) {
        for entry in &mut self.entries {
            entry.on = false;
        }
    }

    /// Advances every fade and auto-stop by `dt` seconds (at most 0.25 per call), and forgets what has faded out.
    pub fn step(&mut self, dt: f32) {
        let dt = if dt.is_finite() { dt.clamp(0.0, MAX_DT) } else { 0.0 };
        for entry in &mut self.entries {
            entry.step(dt);
        }
        self.entries.retain(|e| e.on || e.weight > 0.0);
    }

    /// The fade of `id` (`0` when it isn't showing), before layers step it back.
    pub fn weight(&self, id: &ExpressionId) -> f32 {
        self.entries.iter().find(|e| e.id == *id).map_or(0.0, |e| e.weight)
    }

    /// Whether `id` is showing or fading in (not fading out).
    pub fn is_shown(&self, id: &ExpressionId) -> bool {
        self.entries.iter().any(|e| e.id == *id && e.on)
    }

    /// The default expression's weight now: 1 less the strongest held expression.
    pub fn default_weight(&self) -> f32 {
        let held = self
            .entries
            .iter()
            .filter(|e| e.layer == PlaybackLayer::Held && Some(&e.id) != self.default.as_ref())
            .fold(0.0_f32, |m, e| m.max(e.weight));
        if self.default.is_some() { 1.0 - held } else { 0.0 }
    }

    /// `base` plus what is showing, each expression scaled by its curve, held ones stepped back on the channels a
    /// momentary one moves, then sanitised. An expression that has left `set` since it was shown adds nothing.
    pub fn blend(&self, set: &ExpressionSet, base: &ChannelBuf) -> ChannelBuf {
        // How far the momentary expressions cover each channel.
        let mut cover = [0.0_f32; CHANNEL_CAPACITY];
        for entry in self.entries.iter().filter(|e| e.layer == PlaybackLayer::Momentary) {
            let Some(expression) = set.0.get(&entry.id) else { continue };
            for channel in expression.deltas.0.keys() {
                let c = &mut cover[channel.index()];
                *c = c.max(entry.weight);
            }
        }
        let mut out = *base;
        let mut add = |id: &ExpressionId, weight: f32, layer: PlaybackLayer| {
            let Some(expression) = set.0.get(id) else { return };
            let t = expression.curve.eval(weight);
            for (channel, delta) in &expression.deltas.0 {
                let keep = if layer == PlaybackLayer::Held { 1.0 - cover[channel.index()] } else { 1.0 };
                out[*channel] += narrow(*delta) * t * keep;
            }
        };
        if let Some(default) = &self.default {
            let shown = self.entries.iter().any(|e| e.id == *default);
            let weight = self.default_weight();
            if !shown && weight > 0.0 {
                add(default, weight, PlaybackLayer::Held);
            }
        }
        for entry in &self.entries {
            add(&entry.id, entry.weight, entry.layer);
        }
        out.sanitize();
        out
    }
}
