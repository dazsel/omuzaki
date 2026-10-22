//! Parameter injection (#727): Omuzaki's channels into `VTube Studio`, one batched `InjectParameterDataRequest` per
//! frame.
//!
//! - [`ChannelMap`] says what is fed, from `data/channel-map.toml`: one custom parameter per channel (the default; the
//!   person maps them in VTS) and VTS's own input parameters, each opt-in, with a linear formula over channels.
//! - [`Injector::push`] runs on the frame thread: it evaluates the map into a fixed-size latest-wins slot (a seqlock
//!   over atomics) with no allocation, no lock and no system call.
//! - [`Pacer`] decides when the bridge sends: once per new frame at most 60 times a second, and the last values again
//!   whenever 500 ms pass without one, inside VTS's 1 s keep-alive.
//! - [`Batches`] builds the requests: VTS's defaults go in `set` mode until VTS says another plugin owns one, which then
//!   moves to `add` for the rest of the session and is listed as a conflict.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::time::Duration;

use omuzaki_anim_api::channel::{CHANNELS, Channel, ChannelBuf, Region};
use serde::Deserialize;

use crate::limits::{CustomParamName, LimitError, MAX_PLUGIN_PARAMS, ParamValue};
use crate::protocol::{InjectMode, InjectParameterDataRequest, InjectedValue, ParameterCreationRequest};

/// The most parameters one map feeds: VTS's cap on a plugin's custom parameters plus room for its defaults.
pub const MAX_PARAMS: usize = 128;
/// The shortest time between two frame sends: 60 a second.
pub const MIN_SEND_GAP: Duration = Duration::from_nanos(1_000_000_000 / 60);
/// The longest time between two sends: half of VTS's 1 s keep-alive, so a late wake-up never lets a parameter lapse.
pub const KEEPALIVE_SEND: Duration = Duration::from_millis(500);

/// The built-in map, `data/channel-map.toml`.
const BUILTIN: &str = include_str!("../../data/channel-map.toml");

/// Why a map can't be used.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum MapError {
    /// The TOML does not parse or has an unknown field.
    #[error("the channel map does not parse: {0}")]
    Parse(String),
    /// A row names a channel the table does not have.
    #[error("the channel map names `{name}`, which is not a channel{}", nearest.as_ref().map(|n| format!(" (did you mean `{n}`?)")).unwrap_or_default())]
    UnknownChannel {
        /// The name as written.
        name: String,
        /// The closest channel name.
        nearest: Option<String>,
    },
    /// A parameter name breaks VTS's rules.
    #[error("the channel map's parameter `{name}` is not valid: {source}")]
    BadName {
        /// The name.
        name: String,
        /// The rule it breaks.
        source: LimitError,
    },
    /// More parameters than VTS lets a plugin create, or than an injector holds.
    #[error("the channel map feeds {0} parameters; at most {MAX_PLUGIN_PARAMS} custom and {MAX_PARAMS} in all")]
    TooMany(usize),
    /// `enable_default` named a parameter the map has no row for.
    #[error("the channel map has no VTube Studio parameter `{0}`")]
    UnknownDefault(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MapFile {
    custom: CustomSection,
    #[serde(default, rename = "default")]
    defaults: Vec<DefaultRowFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomSection {
    prefix: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultRowFile {
    param: String,
    #[serde(default)]
    offset: f32,
    terms: Vec<TermFile>,
    #[serde(default)]
    min: Option<f32>,
    #[serde(default)]
    max: Option<f32>,
    #[serde(default)]
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TermFile {
    channel: String,
    weight: f32,
}

/// One parameter the bridge feeds, and how its value comes from the channels.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// The VTS parameter name.
    pub name: String,
    /// True for one of VTS's own input parameters (fed in `set`, may conflict); false for an Omuzaki custom one.
    pub default_input: bool,
    /// `offset + sum(weight * channel)`.
    pub offset: f32,
    /// The channels and their weights.
    pub terms: Vec<(Channel, f32)>,
    /// The smallest value sent.
    pub min: f32,
    /// The largest value sent.
    pub max: f32,
    /// The value with every channel at rest, for a custom parameter's creation.
    pub rest: f32,
}

impl Param {
    fn eval(&self, buf: &ChannelBuf) -> f32 {
        let mut v = self.offset;
        for &(channel, weight) in &self.terms {
            v += weight * buf.get(channel);
        }
        if v.is_finite() { v.clamp(self.min, self.max) } else { self.rest }
    }
}

/// What the bridge feeds: the custom parameters and the switched-on VTS defaults, in send order.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelMap {
    custom: Vec<Param>,
    defaults: Vec<(Param, bool)>,
}

impl ChannelMap {
    /// The built-in map: every table channel as a custom parameter, every VTS default off.
    ///
    /// # Errors
    /// [`MapError`] if the built-in file is broken (a test keeps it parsing).
    pub fn builtin() -> Result<Self, MapError> {
        Self::parse(BUILTIN)
    }

    /// A map from TOML in `data/channel-map.toml`'s shape.
    ///
    /// # Errors
    /// [`MapError`] for a parse error, an unknown channel, a bad name or too many parameters.
    pub fn parse(text: &str) -> Result<Self, MapError> {
        let file: MapFile = toml::from_str(text).map_err(|e| MapError::Parse(e.message().to_owned()))?;
        let mut custom = Vec::new();
        for info in CHANNELS.iter().filter(|c| c.region != Region::Custom) {
            let name = custom_name(&file.custom.prefix, info.name);
            CustomParamName::new(name.clone()).map_err(|source| MapError::BadName { name: name.clone(), source })?;
            let rest = 0.0f32.clamp(info.min, info.max);
            custom.push(Param {
                name,
                default_input: false,
                offset: 0.0,
                terms: vec![(info.channel, 1.0)],
                min: info.min,
                max: info.max,
                rest,
            });
        }
        let mut defaults = Vec::with_capacity(file.defaults.len());
        for row in file.defaults {
            let mut terms = Vec::with_capacity(row.terms.len());
            for term in row.terms {
                let channel = Channel::from_name(&term.channel).ok_or_else(|| MapError::UnknownChannel {
                    nearest: omuzaki_core::text::nearest(&term.channel, CHANNELS.iter().map(|c| c.name))
                        .map(str::to_owned),
                    name: term.channel.clone(),
                })?;
                terms.push((channel, term.weight));
            }
            let (min, max) = (row.min.unwrap_or(0.0), row.max.unwrap_or(1.0));
            let rest = row.offset.clamp(min, max);
            let param = Param { name: row.param, default_input: true, offset: row.offset, terms, min, max, rest };
            defaults.push((param, row.enabled));
        }
        let map = Self { custom, defaults };
        map.check()?;
        Ok(map)
    }

    fn check(&self) -> Result<(), MapError> {
        let total = self.params().count();
        if self.custom.len() > MAX_PLUGIN_PARAMS || total > MAX_PARAMS {
            return Err(MapError::TooMany(total));
        }
        Ok(())
    }

    /// Switches one of VTS's default parameters on or off.
    ///
    /// # Errors
    /// [`MapError::UnknownDefault`] when the map has no row for `param`.
    pub fn enable_default(&mut self, param: &str, on: bool) -> Result<(), MapError> {
        let row = self.defaults.iter_mut().find(|(p, _)| p.name == param);
        let (_, enabled) = row.ok_or_else(|| MapError::UnknownDefault(param.to_owned()))?;
        *enabled = on;
        self.check()
    }

    /// Every parameter fed, custom first, then the switched-on defaults.
    pub fn params(&self) -> impl Iterator<Item = &Param> {
        self.custom.iter().chain(self.defaults.iter().filter(|(_, on)| *on).map(|(p, _)| p))
    }

    /// The VTS defaults the map can feed, and whether each is on.
    pub fn defaults(&self) -> impl Iterator<Item = (&str, bool)> {
        self.defaults.iter().map(|(p, on)| (p.name.as_str(), *on))
    }

    /// The `ParameterCreationRequest` for every custom parameter, sent on connect (repeating one updates its range).
    pub fn creations(&self) -> Vec<ParameterCreationRequest> {
        self.custom
            .iter()
            .filter_map(|p| {
                Some(ParameterCreationRequest {
                    parameter_name: CustomParamName::new(p.name.clone()).ok()?,
                    explanation: Some(format!("Omuzaki's {} channel", p.terms.first()?.0.name())),
                    min: ParamValue::new(f64::from(p.min)).ok()?,
                    max: ParamValue::new(f64::from(p.max)).ok()?,
                    default_value: ParamValue::new(f64::from(p.rest)).ok()?,
                })
            })
            .collect()
    }
}

/// `OmuzakiJawOpen` for `jawOpen`.
fn custom_name(prefix: &str, channel: &str) -> String {
    let mut chars = channel.chars();
    let first = chars.next().map(|c| c.to_ascii_uppercase()).into_iter();
    prefix.chars().chain(first).chain(chars).collect()
}

/// The latest values pushed, as [`Injector::read`] copies them out.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Counts pushes: a new value means a new frame.
    pub seq: u64,
    /// The tracker saw a face in that frame.
    pub face_found: bool,
    /// One value per [`ChannelMap::params`] entry, in that order; the rest are 0.
    pub values: [f32; MAX_PARAMS],
}

impl Default for Snapshot {
    fn default() -> Self {
        Self { seq: 0, face_found: false, values: [0.0; MAX_PARAMS] }
    }
}

/// The frame thread's side of injection: [`push`](Self::push) a frame's channels, latest wins.
#[derive(Debug)]
pub struct Injector {
    params: Vec<Param>,
    /// Even when the values are whole; odd while a push writes them. Counts two per push.
    seq: AtomicU64,
    /// Set while a push writes, so a second, concurrent pusher skips its frame instead of tearing this one.
    writing: AtomicBool,
    face: AtomicBool,
    values: [AtomicU32; MAX_PARAMS],
}

impl Injector {
    /// An injector feeding `map`'s parameters.
    #[must_use]
    pub fn new(map: &ChannelMap) -> Self {
        Self {
            params: map.params().take(MAX_PARAMS).cloned().collect(),
            seq: AtomicU64::new(0),
            writing: AtomicBool::new(false),
            face: AtomicBool::new(false),
            values: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }

    /// The parameters fed, in value order.
    pub fn params(&self) -> &[Param] {
        &self.params
    }

    /// Publishes one frame. Called on the frame thread: no allocation, no lock, no system call. A push made while
    /// another thread's push is still writing is skipped (the other frame wins), never torn.
    pub fn push(&self, buf: &ChannelBuf, face_found: bool) {
        if self.writing.swap(true, Ordering::Acquire) {
            return;
        }
        let start = self.seq.load(Ordering::Relaxed);
        self.seq.store(start.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        for (slot, param) in self.values.iter().zip(&self.params) {
            slot.store(param.eval(buf).to_bits(), Ordering::Relaxed);
        }
        self.face.store(face_found, Ordering::Relaxed);
        self.seq.store(start.wrapping_add(2), Ordering::Release);
        self.writing.store(false, Ordering::Release);
    }

    /// Copies the latest frame into `out` when it is newer than `out.seq`; true when it did.
    pub fn read(&self, out: &mut Snapshot) -> bool {
        loop {
            let before = self.seq.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let pushes = before / 2;
            if pushes == out.seq {
                return false;
            }
            for (value, slot) in out.values.iter_mut().zip(&self.values) {
                *value = f32::from_bits(slot.load(Ordering::Relaxed));
            }
            let face_found = self.face.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                (out.seq, out.face_found) = (pushes, face_found);
                return true;
            }
        }
    }
}

/// Why the bridge sends now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// A new frame.
    Frame,
    /// No new frame for [`KEEPALIVE_SEND`]: the last values again.
    KeepAlive,
}

/// When to send: a new frame at most every [`MIN_SEND_GAP`], else the last values every [`KEEPALIVE_SEND`].
#[derive(Debug, Clone, Default)]
pub struct Pacer {
    last: Option<Duration>,
}

impl Pacer {
    /// Whether to send at `now`, given whether a frame newer than the last one sent is waiting.
    #[must_use]
    pub fn due(&self, now: Duration, new_frame: bool) -> Option<Due> {
        let since = self.last.map(|last| now.saturating_sub(last));
        match since {
            None => new_frame.then_some(Due::Frame),
            Some(gap) if new_frame && gap >= MIN_SEND_GAP => Some(Due::Frame),
            Some(gap) if gap >= KEEPALIVE_SEND => Some(Due::KeepAlive),
            Some(_) => None,
        }
    }

    /// Records a send at `now`.
    pub fn sent(&mut self, now: Duration) {
        self.last = Some(now);
    }

    /// Forgets the last send (a new session).
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// Builds the requests for one send, and remembers which defaults moved to `add`.
#[derive(Debug, Clone, Default)]
pub struct Batches {
    /// Indexes into the injector's params now fed in `add`.
    conflicts: BTreeSet<usize>,
}

impl Batches {
    /// The requests for `snap`: one in `set` with every parameter not in conflict, and one in `add` when any is.
    #[must_use]
    pub fn requests(&self, params: &[Param], snap: &Snapshot) -> Vec<InjectParameterDataRequest> {
        let value = |i: usize, p: &Param| InjectedValue {
            id: p.name.clone(),
            value: ParamValue::saturating(f64::from(snap.values[i])),
            weight: None,
        };
        let (mut set, mut add) = (Vec::with_capacity(params.len()), Vec::new());
        for (i, p) in params.iter().enumerate() {
            if self.conflicts.contains(&i) { add.push(value(i, p)) } else { set.push(value(i, p)) }
        }
        let mut out = Vec::with_capacity(2);
        for (mode, values) in [(InjectMode::Set, set), (InjectMode::Add, add)] {
            if !values.is_empty() {
                out.push(InjectParameterDataRequest { face_found: snap.face_found, mode, parameter_values: values });
            }
        }
        out
    }

    /// VTS refused a `set` batch because another plugin owns a parameter: moves the one its message names to `add`
    /// (every default in the batch when the message names none, since only defaults can be owned by another plugin).
    /// Returns whether anything moved.
    pub fn owned(&mut self, params: &[Param], message: &str) -> bool {
        let named = params
            .iter()
            .enumerate()
            .filter(|(_, p)| p.default_input && mentions(message, &p.name))
            .max_by_key(|(_, p)| p.name.len())
            .map(|(i, _)| i);
        let before = self.conflicts.len();
        match named {
            Some(i) => {
                self.conflicts.insert(i);
            }
            None => self.conflicts.extend(params.iter().enumerate().filter(|(_, p)| p.default_input).map(|(i, _)| i)),
        }
        self.conflicts.len() > before
    }

    /// The parameters moved to `add`, by name.
    #[must_use]
    pub fn conflicts(&self, params: &[Param]) -> Vec<String> {
        self.conflicts.iter().filter_map(|&i| params.get(i)).map(|p| p.name.clone()).collect()
    }

    /// Forgets every conflict (a new session).
    pub fn reset(&mut self) {
        self.conflicts.clear();
    }
}

/// Whether `message` names `param` as a whole word.
fn mentions(message: &str, param: &str) -> bool {
    message.match_indices(param).any(|(at, _)| {
        let before = message[..at].chars().next_back();
        let after = message[at + param.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

#[cfg(test)]
mod tests;
