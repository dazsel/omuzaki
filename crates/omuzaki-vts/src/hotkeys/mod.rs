//! Hotkeys first, expressions only when mapped (#728).
//!
//! - [`VtsMapping`] maps an Omuzaki expression or trigger id to something in the current VTS model: a hotkey (the
//!   default) or, only by an explicit choice, an expression file.
//! - [`suggest`] proposes hotkey rows from a model's hotkey names; nothing is applied without a person's or an op's
//!   confirmation.
//! - [`HotkeyLimiter`] keeps the bridge inside VTS's limits so VTS never refuses a trigger: one hotkey is not sent
//!   again within 5 VTS frames (it waits), and VTS's queue of 32 is mirrored (overflow is dropped and counted).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::limits::{HOTKEY_MIN_FRAMES, HOTKEY_QUEUE};
use crate::protocol::Hotkey;

/// The frame rate assumed until VTS reports one.
pub const DEFAULT_FRAMERATE: u32 = 30;
/// The most triggers waiting for a cooldown; past it the newest is dropped and counted.
pub const PENDING_MAX: usize = 64;

/// What an Omuzaki id drives in a VTS model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    /// A hotkey of the model: VTS runs whatever it is bound to, and its own rules end it.
    Hotkey {
        /// The hotkey's unique id.
        id: String,
        /// Its name, for people.
        name: String,
    },
    /// An expression file switched on directly. Only ever made by an explicit choice: VTS's README warns that an
    /// expression switched on through the API can be left on with no way for the person to switch it off, so the
    /// bridge tracks every one it turns on and turns it off on disconnect, bridge stop and model change.
    Expression {
        /// The `.exp3.json` file.
        file: String,
        /// The fade, clamped to 0 to 2 s.
        fade_s: f64,
    },
}

/// One row: an Omuzaki expression or trigger id and its target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Row {
    /// The Omuzaki id (`smug`, `angry`, a trigger's id).
    pub omuzaki: String,
    /// What it drives.
    pub target: Target,
}

/// Rows per VTS model id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VtsMapping {
    /// The rows of each model, by VTS model id.
    pub models: BTreeMap<String, Vec<Row>>,
}

impl VtsMapping {
    /// The target of `omuzaki` in `model`.
    #[must_use]
    pub fn resolve(&self, model: &str, omuzaki: &str) -> Option<&Target> {
        self.models.get(model)?.iter().find(|r| r.omuzaki == omuzaki).map(|r| &r.target)
    }
}

/// Hotkey rows proposed for `ids` from a model's `hotkeys`: an id matches a hotkey whose name has it as a word (any
/// case), or a word one edit away. Ids with no match get no row.
#[must_use]
pub fn suggest(ids: &[&str], hotkeys: &[Hotkey]) -> Vec<Row> {
    let words: Vec<(usize, Vec<String>)> = hotkeys
        .iter()
        .enumerate()
        .map(|(i, h)| (i, h.name.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase).collect()))
        .collect();
    let mut rows = Vec::new();
    for id in ids {
        let wanted = id.to_lowercase();
        let exact = words.iter().find(|(_, w)| w.contains(&wanted));
        let close = || {
            let all = words.iter().flat_map(|(i, w)| w.iter().map(move |w| (*i, w.as_str())));
            all.filter(|(_, w)| w.len() > 2 && edit_distance(&wanted, w) <= 1).map(|(i, _)| i).next()
        };
        let Some(index) = exact.map(|(i, _)| *i).or_else(close) else { continue };
        let hotkey = &hotkeys[index];
        rows.push(Row {
            omuzaki: (*id).to_owned(),
            target: Target::Hotkey { id: hotkey.hotkey_id.clone(), name: hotkey.name.clone() },
        });
    }
    rows
}

/// Levenshtein distance over characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1).min(row[j] + 1).min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

/// VTS's hotkey limits, mirrored on the bridge's side.
#[derive(Debug, Clone)]
pub struct HotkeyLimiter {
    frame: Duration,
    /// When each hotkey was last sent.
    last: HashMap<String, Duration>,
    /// When each hotkey in VTS's queue is done, oldest first: one frame each, back to back.
    queue: VecDeque<Duration>,
    /// Triggers waiting for their hotkey's cooldown, in arrival order.
    pending: VecDeque<String>,
    dropped: u64,
}

impl Default for HotkeyLimiter {
    fn default() -> Self {
        Self::new(DEFAULT_FRAMERATE)
    }
}

impl HotkeyLimiter {
    /// A limiter for VTS running at `framerate` (0 counts as [`DEFAULT_FRAMERATE`]).
    #[must_use]
    pub fn new(framerate: u32) -> Self {
        let mut limiter = Self {
            frame: Duration::ZERO,
            last: HashMap::new(),
            queue: VecDeque::with_capacity(HOTKEY_QUEUE),
            pending: VecDeque::with_capacity(PENDING_MAX),
            dropped: 0,
        };
        limiter.set_framerate(framerate);
        limiter
    }

    /// VTS's frame rate, from `StatisticsRequest`.
    pub fn set_framerate(&mut self, framerate: u32) {
        let fps = u64::from(if framerate == 0 { DEFAULT_FRAMERATE } else { framerate });
        // Rounded up, so the bridge's frame is never shorter than VTS's and a trigger is never early.
        self.frame = Duration::from_nanos(1_000_000_000_u64.div_ceil(fps));
    }

    /// Asks for `hotkey` to be sent; [`due`](Self::due) says when.
    pub fn request(&mut self, hotkey: String) {
        if self.pending.len() >= PENDING_MAX {
            self.dropped += 1;
            return;
        }
        self.pending.push_back(hotkey);
    }

    /// Moves every trigger that may go now into `out`: one whose hotkey is cooling down waits; one that finds VTS's
    /// queue full is dropped and counted.
    pub fn due(&mut self, now: Duration, out: &mut Vec<String>) {
        while self.queue.front().is_some_and(|&done| done <= now) {
            self.queue.pop_front();
        }
        let cooldown = self.frame * HOTKEY_MIN_FRAMES;
        let mut waiting = VecDeque::with_capacity(self.pending.len());
        while let Some(hotkey) = self.pending.pop_front() {
            if self.last.get(&hotkey).is_some_and(|&at| now.saturating_sub(at) < cooldown) {
                waiting.push_back(hotkey);
                continue;
            }
            if self.queue.len() >= HOTKEY_QUEUE {
                self.dropped += 1;
                continue;
            }
            let start = self.queue.back().copied().unwrap_or(now).max(now);
            self.queue.push_back(start + self.frame);
            self.last.insert(hotkey.clone(), now);
            out.push(hotkey);
        }
        self.pending = waiting;
    }

    /// When the next waiting trigger's cooldown ends, if any waits.
    #[must_use]
    pub fn next_due(&self) -> Option<Duration> {
        let cooldown = self.frame * HOTKEY_MIN_FRAMES;
        self.pending.iter().filter_map(|h| self.last.get(h).map(|&at| at + cooldown)).min()
    }

    /// Triggers dropped so far.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Forgets the queue and every waiting trigger (a new session); the dropped count stays.
    pub fn clear(&mut self) {
        self.queue.clear();
        self.pending.clear();
        self.last.clear();
    }
}

#[cfg(test)]
mod tests;
