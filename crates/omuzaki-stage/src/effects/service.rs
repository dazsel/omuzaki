//! The effect service (particles.md D12): [`StageEffects`] implements [`EffectService`] for the ops, and hands what
//! they ask for to the stage's frame step.
//!
//! - **Requests are resolved before they are queued.** A preset becomes a `&'static` reference into the embedded
//!   library, a bone name a [`HumanoidBone`], a member or prop id and an emote URL a 64-bit key, colours a
//!   [`ColourSet`]. What is queued is a fixed-size `Copy` [`Request`], so the render thread never allocates or frees.
//! - **The queue never blocks the render thread.** It holds [`MAX_PENDING_EFFECTS`] requests; an op thread locks it for
//!   a push, and the render thread only ever `try_lock`s it: when an op holds it, the frame drains on the next one.
//!   A full queue refuses with [`EffectError::Limited`].
//! - **Status and events come back the same way.** The frame step publishes a `Copy` status record and its outcomes
//!   with `try_lock`; [`StageEffects::status`] and [`StageEffects::take_events`] turn them into the contract's types
//!   on the caller's thread.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use omuzaki_anim_api::HumanoidBone;
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::ColourSet;
use omuzaki_stage_api::effects::{
    BurstAccepted, BurstEvent, BurstRequest, BurstShape, ChatEmoteRef, ClearEvent, EffectAt, EffectError,
    EffectEvent, EffectEventKind, EffectKind, EffectOrigin, EffectPreset, EffectService, EffectStatus, LiveWeather,
    MAX_BURST_EMOTES, MAX_CLEAR_FADE_S, MAX_PENDING_EFFECTS, PresetInfo, SpriteDef, WeatherSource,
};

use super::presets;

/// Outcomes kept for [`StageEffects::take_events`]; the oldest are dropped when nobody takes them.
pub const MAX_OUTCOMES: usize = 256;

/// The emote lists kept for events (one per burst that carried emotes); the oldest are dropped first.
const MAX_EMOTE_LISTS: usize = 128;

/// A 64-bit key of a text (FNV-1a): emote URLs, member and prop ids. Stable across runs and platforms.
pub fn key_of(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// A fixed ring of `Copy` items that never allocates.
#[derive(Debug)]
pub(crate) struct Ring<T: Copy, const N: usize> {
    items: [Option<T>; N],
    head: usize,
    len: usize,
}

impl<T: Copy, const N: usize> Ring<T, N> {
    pub(crate) const fn new() -> Self {
        Self { items: [None; N], head: 0, len: 0 }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Adds `item` at the back; false when full.
    pub(crate) fn push(&mut self, item: T) -> bool {
        if self.len == N {
            return false;
        }
        self.items[(self.head + self.len) % N] = Some(item);
        self.len += 1;
        true
    }

    /// Adds `item`, dropping the oldest when full.
    pub(crate) fn push_over(&mut self, item: T) {
        if self.len == N {
            self.pop();
        }
        self.push(item);
    }

    /// Takes the oldest.
    pub(crate) fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let item = self.items[self.head].take();
        self.head = (self.head + 1) % N;
        self.len -= 1;
        item
    }
}

/// Where a request's anchor is measured from, resolved to `Copy` values.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The scene's subject.
    Subject,
    /// The stage origin.
    Stage,
    /// The active camera.
    Camera,
    /// The world origin.
    World,
    /// A cast member, by the key of its id.
    Member(u64),
    /// A prop, by the key of its id.
    Prop(u64),
}

/// A point on the origin.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Point {
    /// The origin itself.
    Origin,
    /// A humanoid bone of a member.
    Bone(HumanoidBone),
    /// Just above a member's head, or a strip above the camera frame's top edge.
    Top,
    /// The middle of the camera's frame at the subject's distance.
    Centre,
}

/// An [`EffectAt`] resolved to `Copy` values.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct At {
    /// What it is measured from.
    pub origin: Origin,
    /// The point on it.
    pub point: Point,
    /// The offset: subject units for members, metres otherwise.
    pub offset: Vec3,
}

impl At {
    /// Resolves `at`.
    ///
    /// # Errors
    /// [`EffectError::Invalid`] for a rule [`EffectAt::validate`] refuses or a bone the humanoid doesn't have.
    pub fn resolve(at: &EffectAt) -> Result<Self, EffectError> {
        at.validate()?;
        let origin = match &at.from {
            EffectOrigin::Subject => Origin::Subject,
            EffectOrigin::Stage => Origin::Stage,
            EffectOrigin::Camera => Origin::Camera,
            EffectOrigin::World => Origin::World,
            EffectOrigin::Member(id) => Origin::Member(key_of(id.as_str())),
            EffectOrigin::Prop(id) => Origin::Prop(key_of(id.as_str())),
        };
        let point = match at.point.as_deref() {
            None => Point::Origin,
            Some("top") => Point::Top,
            Some("centre") => Point::Centre,
            Some(name) => Point::Bone(HumanoidBone::from_name(name).ok_or_else(|| {
                let names: Vec<&str> = HumanoidBone::ALL.iter().map(|b| b.name()).collect();
                let near = presets::nearest(name, names.iter().map(|n| (*n).to_owned()));
                let hint = near.first().map_or(String::new(), |n| format!("; did you mean `{n}`?"));
                EffectError::Invalid(format!("at.point `{name}` is not a VRM humanoid bone{hint}"))
            })?),
        };
        Ok(Self { origin, point, offset: Vec3::from(at.offset) })
    }
}

/// A burst, resolved.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BurstRecord {
    /// The request's number, which its emote list in the event is found by.
    pub seq: u64,
    /// The preset.
    pub preset: &'static EffectPreset,
    /// Where.
    pub at: At,
    /// Particles, or particles per emote for an emote burst.
    pub count: u32,
    /// A colour override.
    pub colours: Option<ColourSet>,
    /// The size multiplier.
    pub scale: f32,
    /// The emotes' keys; only the first `emote_count` count.
    pub emotes: [u64; MAX_BURST_EMOTES],
    /// How many emotes.
    pub emote_count: u8,
    /// The seed.
    pub seed: u32,
    /// A replayed burst's recorded emitter, shape and velocity, used instead of resolving `at` again.
    pub fixed: Option<(Mat4, BurstShape, Vec3)>,
}

/// The live weather, resolved.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum WeatherRecord {
    /// A preset replaces the scene's weather.
    Preset {
        /// The weather preset.
        preset: &'static EffectPreset,
        /// `0..=1`.
        intensity: f32,
        /// Seconds until the scene's weather comes back.
        for_s: Option<f32>,
    },
    /// No weather until changed.
    None,
    /// The scene's own weather again.
    Scene,
}

impl WeatherRecord {
    /// The contract's form of this change.
    pub fn to_live(self) -> LiveWeather {
        match self {
            Self::Preset { preset, intensity, for_s } => LiveWeather::preset(preset.name.clone(), intensity, for_s),
            Self::None => LiveWeather::None,
            Self::Scene => LiveWeather::Scene,
        }
    }
}

/// One queued request.
#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant, reason = "a fixed-size record in a fixed ring; boxing would allocate per request")]
pub enum Request {
    /// A burst.
    Burst(BurstRecord),
    /// A change of the live weather.
    Weather(WeatherRecord),
    /// Everything shrinks away over `fade_s` seconds.
    Clear(f32),
}

/// What the frame step did with a request: the `Copy` form of an [`EffectEvent`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Outcome {
    /// The scene time it took effect.
    pub at: Seconds,
    /// What happened.
    pub kind: OutcomeKind,
}

/// What happened.
#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant, reason = "a fixed-size record in a fixed ring; boxing would allocate per frame")]
pub enum OutcomeKind {
    /// A burst spawned.
    Burst {
        /// The request.
        record: BurstRecord,
        /// The resolved emitter.
        emitter: Mat4,
        /// Where in it.
        shape: BurstShape,
        /// The colours it used, when not the preset's (the request's, or the anchor member's palette).
        colours: Option<ColourSet>,
        /// The anchor's velocity.
        velocity: Vec3,
        /// Particles spawned.
        count: u32,
        /// Particles the cap took away.
        thinned: u32,
    },
    /// The live weather changed.
    Weather(WeatherRecord),
    /// Everything was cleared.
    Clear(f32),
}

/// Findings the status reports, one bit each.
pub mod finding {
    /// A member anchor named someone who isn't on the stage; the subject was used.
    pub const MEMBER: u32 = 1;
    /// A prop anchor; props aren't anchors yet, so the subject was used.
    pub const PROP: u32 = 1 << 1;
    /// A bone the character doesn't have; its root was used.
    pub const BONE: u32 = 1 << 2;
    /// A particle system couldn't be made; its effect was skipped.
    pub const SYSTEM: u32 = 1 << 3;
    /// Every system slot was in use; an effect was skipped.
    pub const SYSTEMS_FULL: u32 = 1 << 4;
    /// Chat emotes draw the placeholder (no emote images are fetched yet).
    pub const EMOTES: u32 = 1 << 5;

    /// Each finding's text.
    pub const TEXT: [(u32, &str); 6] = [
        (MEMBER, "a member anchor names someone not on this stage; the subject was used"),
        (PROP, "props are not anchors yet; the subject was used"),
        (BONE, "a bone anchor names a bone this character lacks; its root was used"),
        (SYSTEM, "a particle system could not be made; its effect was skipped"),
        (SYSTEMS_FULL, "every particle system slot is in use; an effect was skipped"),
        (EMOTES, "chat emotes draw the placeholder heart"),
    ];
}

/// The status the frame step publishes.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct StatusRecord {
    /// Live slots.
    pub slots: u32,
    /// The cap.
    pub cap: u32,
    /// Where the weather comes from.
    pub weather: WeatherSource,
    /// The density the scheduler set.
    pub density: f32,
    /// Whether the renderer draws particles.
    pub drawn: bool,
    /// Particles thinned so far.
    pub thinned: u64,
    /// [`finding`] bits.
    pub findings: u32,
}

/// The stage's effects as the ops reach them: a bounded queue into the frame step, the status it publishes, and the
/// events it records. Shared (`Arc`) between the app's services and the stage that runs the frames.
#[derive(Debug)]
pub struct StageEffects {
    queue: Mutex<Ring<Request, MAX_PENDING_EFFECTS>>,
    outcomes: Mutex<Ring<Outcome, MAX_OUTCOMES>>,
    emotes: Mutex<VecDeque<(u64, Vec<ChatEmoteRef>)>>,
    status: Mutex<StatusRecord>,
    session: u64,
    counter: AtomicU64,
}

impl StageEffects {
    /// A service whose drawn seeds come from `session` and a counter, so a session given the same seed draws the same
    /// bursts.
    pub fn new(session: u64) -> Self {
        Self {
            queue: Mutex::new(Ring::new()),
            outcomes: Mutex::new(Ring::new()),
            emotes: Mutex::new(VecDeque::with_capacity(MAX_EMOTE_LISTS)),
            status: Mutex::new(StatusRecord::default()),
            session,
            counter: AtomicU64::new(0),
        }
    }

    fn next(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::Relaxed)
    }

    /// A seed from the session and the counter (`SplitMix64`), so consecutive bursts differ.
    fn draw_seed(&self, n: u64) -> u32 {
        let mut z = self.session ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        // The top bits, as a u32.
        u32::try_from((z ^ (z >> 31)) >> 32).unwrap_or(0)
    }

    fn push(&self, request: Request) -> Result<(), EffectError> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.push(request) { Ok(()) } else { Err(EffectError::Limited) }
    }

    /// Resolves a burst request into the record the frame step takes, drawing a seed when none is given. `None` for
    /// an emote burst with no emotes that asks for nothing then (`only_with_emotes`).
    ///
    /// # Errors
    /// As [`check_burst`].
    pub fn resolve_burst(&self, request: &BurstRequest) -> Result<Option<BurstRecord>, EffectError> {
        let (preset, at) = check_burst(request)?;
        if request.only_with_emotes && request.emotes.is_empty() {
            return Ok(None);
        }
        let seq = self.next();
        let mut emotes = [0; MAX_BURST_EMOTES];
        for (slot, emote) in emotes.iter_mut().zip(&request.emotes) {
            *slot = key_of(emote.image.as_deref().unwrap_or(&emote.id));
        }
        let count = request.count.or(preset.burst.as_ref().map(|b| b.count)).unwrap_or(1);
        Ok(Some(BurstRecord {
            seq,
            preset,
            at,
            count,
            colours: request.colours.as_deref().and_then(ColourSet::new),
            scale: request.scale.unwrap_or(1.0),
            emotes,
            emote_count: u8::try_from(request.emotes.len().min(MAX_BURST_EMOTES)).unwrap_or(0),
            seed: request.seed.unwrap_or_else(|| self.draw_seed(seq)),
            fixed: None,
        }))
    }

    /// Queues a resolved request.
    ///
    /// # Errors
    /// [`EffectError::Limited`] when the queue is full.
    pub fn queue(&self, request: Request) -> Result<(), EffectError> {
        self.push(request)
    }

    /// Requests queued for the next frame.
    pub fn pending(&self) -> usize {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Moves up to `out.len()` queued requests into `out` without blocking: none when an op holds the queue. Returns
    /// how many. The render thread's call; allocation-free.
    pub(crate) fn drain(&self, out: &mut [Option<Request>]) -> usize {
        let Ok(mut queue) = self.queue.try_lock() else { return 0 };
        let mut n = 0;
        for slot in out.iter_mut() {
            let Some(request) = queue.pop() else { break };
            *slot = Some(request);
            n += 1;
        }
        n
    }

    /// Publishes the frame's status and hands over its outcomes, without blocking: what can't be handed over now
    /// stays in `outbox` for the next frame. The render thread's call; allocation-free.
    pub(crate) fn publish(&self, status: StatusRecord, outbox: &mut Ring<Outcome, MAX_PENDING_EFFECTS>) {
        if let Ok(mut slot) = self.status.try_lock() {
            *slot = status;
        }
        if outbox.len() > 0
            && let Ok(mut outcomes) = self.outcomes.try_lock()
        {
            while let Some(outcome) = outbox.pop() {
                outcomes.push_over(outcome);
            }
        }
    }

    /// The effect events since the last call, oldest first: what a take records (D14). Cold.
    pub fn take_events(&self) -> Vec<EffectEvent> {
        let mut taken = Vec::new();
        {
            let mut outcomes = self.outcomes.lock().unwrap_or_else(PoisonError::into_inner);
            while let Some(outcome) = outcomes.pop() {
                taken.push(outcome);
            }
        }
        let emotes = self.emotes.lock().unwrap_or_else(PoisonError::into_inner);
        taken
            .into_iter()
            .map(|o| {
                let kind = match o.kind {
                    OutcomeKind::Burst { record, emitter, shape, colours, velocity, count, thinned } => {
                        EffectEventKind::Burst(BurstEvent {
                            preset: record.preset.name.clone(),
                            emitter: emitter.to_cols_array(),
                            shape,
                            velocity: velocity.to_array(),
                            count,
                            thinned,
                            seed: record.seed,
                            colours: colours.map(|c| c.as_slice().to_vec()).unwrap_or_default(),
                            scale: record.scale,
                            emotes: emotes
                                .iter()
                                .find(|(seq, _)| *seq == record.seq)
                                .map(|(_, list)| list.clone())
                                .unwrap_or_default(),
                        })
                    }
                    OutcomeKind::Weather(w) => EffectEventKind::Weather(w.to_live()),
                    OutcomeKind::Clear(fade_s) => EffectEventKind::Clear(ClearEvent { fade_s }),
                };
                EffectEvent { at: o.at, kind }
            })
            .collect()
    }

    fn remember_emotes(&self, seq: u64, list: &[ChatEmoteRef]) {
        if list.is_empty() {
            return;
        }
        let mut emotes = self.emotes.lock().unwrap_or_else(PoisonError::into_inner);
        if emotes.len() == MAX_EMOTE_LISTS {
            emotes.pop_front();
        }
        emotes.push_back((seq, list.to_vec()));
    }
}

/// Checks a burst request against its rules and the presets, without queueing anything: what a dry run does.
/// Returns the preset and the resolved anchor (the request's, else the preset's).
///
/// # Errors
/// [`EffectError::UnknownPreset`] with the nearest names, [`EffectError::Invalid`] for a weather preset, a range or
/// an anchor the request breaks, or emotes on a preset that doesn't draw them.
pub fn check_burst(request: &BurstRequest) -> Result<(&'static EffectPreset, At), EffectError> {
    request.validate()?;
    let preset = presets::find_kind(request.preset.as_str(), EffectKind::Burst)?;
    if !request.emotes.is_empty() && !matches!(preset.sprite, SpriteDef::Atlas(_)) {
        return Err(EffectError::Invalid(format!(
            "{} draws shapes, not chat emotes; emote-rain shows emotes",
            preset.name
        )));
    }
    let default_at = preset.burst.as_ref().map(|b| &b.at);
    let at = match (&request.at, default_at) {
        (Some(at), _) | (None, Some(at)) => At::resolve(at)?,
        (None, None) => At::resolve(&EffectAt::default())?,
    };
    Ok((preset, at))
}

/// Checks a live weather request, without queueing anything.
///
/// # Errors
/// [`EffectError::UnknownPreset`], or [`EffectError::Invalid`] for a burst preset or a range.
pub fn check_weather(request: &LiveWeather) -> Result<WeatherRecord, EffectError> {
    request.validate()?;
    Ok(match request {
        LiveWeather::Preset(o) => WeatherRecord::Preset {
            preset: presets::find_kind(o.preset.as_str(), EffectKind::Weather)?,
            intensity: o.intensity,
            for_s: o.for_s,
        },
        LiveWeather::Scene => WeatherRecord::Scene,
        LiveWeather::None => WeatherRecord::None,
    })
}

/// Checks a clear's fade.
///
/// # Errors
/// [`EffectError::Invalid`] for a fade outside `0..=5` seconds.
pub fn check_clear(fade_s: f32) -> Result<(), EffectError> {
    if fade_s.is_finite() && (0.0..=MAX_CLEAR_FADE_S).contains(&fade_s) {
        Ok(())
    } else {
        Err(EffectError::Invalid(format!("fade_s must be in 0..={MAX_CLEAR_FADE_S}, got {fade_s}")))
    }
}

impl EffectService for StageEffects {
    fn presets(&self) -> &[PresetInfo] {
        presets::infos()
    }

    fn burst(&self, request: &BurstRequest) -> Result<BurstAccepted, EffectError> {
        let Some(record) = self.resolve_burst(request)? else {
            return Ok(BurstAccepted { preset: request.preset.clone(), count: 0, seed: request.seed.unwrap_or(0) });
        };
        self.push(Request::Burst(record))?;
        self.remember_emotes(record.seq, &request.emotes);
        let per = u32::from(record.emote_count.max(1));
        Ok(BurstAccepted { preset: record.preset.name.clone(), count: record.count * per, seed: record.seed })
    }

    fn live_weather(&self, request: &LiveWeather) -> Result<(), EffectError> {
        let record = check_weather(request)?;
        self.push(Request::Weather(record))
    }

    fn clear(&self, fade_s: f32) -> Result<(), EffectError> {
        check_clear(fade_s)?;
        self.push(Request::Clear(fade_s))
    }

    fn status(&self) -> EffectStatus {
        let record = *self.status.lock().unwrap_or_else(PoisonError::into_inner);
        let findings = finding::TEXT
            .iter()
            .filter(|(bit, _)| record.findings & bit != 0)
            .map(|(_, text)| (*text).to_owned())
            .collect();
        EffectStatus {
            slots: record.slots,
            cap: record.cap,
            weather: record.weather,
            density: record.density,
            pending: u32::try_from(self.pending()).unwrap_or(u32::MAX),
            drawn: record.drawn,
            thinned: record.thinned,
            findings,
        }
    }

    fn replay(&self, event: &EffectEvent) -> Result<(), EffectError> {
        match &event.kind {
            EffectEventKind::Burst(b) => {
                let preset = presets::find_kind(b.preset.as_str(), EffectKind::Burst)?;
                let seq = self.next();
                let mut emotes = [0; MAX_BURST_EMOTES];
                for (slot, emote) in emotes.iter_mut().zip(&b.emotes) {
                    *slot = key_of(emote.image.as_deref().unwrap_or(&emote.id));
                }
                let emote_count = u8::try_from(b.emotes.len().min(MAX_BURST_EMOTES)).unwrap_or(0);
                let per = u32::from(emote_count.max(1));
                let record = BurstRecord {
                    seq,
                    preset,
                    at: At { origin: Origin::World, point: Point::Origin, offset: Vec3::ZERO },
                    count: b.count / per,
                    colours: ColourSet::new(&b.colours),
                    scale: b.scale,
                    emotes,
                    emote_count,
                    seed: b.seed,
                    fixed: Some((Mat4::from_cols_array(&b.emitter), b.shape, Vec3::from(b.velocity))),
                };
                self.push(Request::Burst(record))?;
                self.remember_emotes(seq, &b.emotes);
                Ok(())
            }
            EffectEventKind::Weather(w) => self.live_weather(w),
            EffectEventKind::Clear(c) => self.clear(c.fade_s),
        }
    }
}
