//! Effect checkpoints (particles.md D14): everything a take needs to render from any time without simulating. A
//! take writes one at its start and every 10 s; to render from `t`, restore the newest at or before `t` and replay the
//! events after it.
//!
//! A checkpoint holds descriptions, never handles: each batch names its system by a stable key (a hash of the
//! system's serialised description), and the descriptions travel with it, so a restore into another renderer (or after
//! a weather system was released) makes what's missing and maps the keys to the new handles. Chat-emote batches hold
//! the placeholder cell until emote images are fetched (#1276), so they restore to it.

use std::collections::BTreeMap;

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{CellPick, ColourSet, EmitterShape, ParticleParams, ParticleSystemId, Renderer, SpawnBatch};
use omuzaki_stage_api::effects::{EffectKind, SpriteDef};
use serde::{Deserialize, Serialize};

use super::frame::{EffectsFrame, Live, WEATHER};
use super::log::{EffectLimits, EmissionLog, LogParts, MAX_STREAMS, Meta, Spawn, StreamKey, StreamState};
use super::presets;
use super::service::key_of;
use crate::error::StageError;

/// A system's description.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SystemRecord {
    /// Its stable key.
    pub key: u64,
    /// What it looks like.
    pub sprite: SpriteDef,
    /// How it moves.
    pub params: ParticleParams,
    /// A weather system.
    pub weather: bool,
}

impl SystemRecord {
    /// The record of a description, keyed by a hash of its serialised form.
    pub fn new(sprite: SpriteDef, params: &ParticleParams, weather: bool) -> Self {
        let text = serde_json::to_string(&(sprite, params, weather)).unwrap_or_default();
        Self { key: key_of(&text), sprite, params: params.clone(), weather }
    }
}

/// An emitter shape.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShapeRecord {
    /// The origin.
    Point,
    /// A ball.
    Sphere {
        /// Metres.
        radius: f32,
    },
    /// A box.
    Box {
        /// Half extents.
        half: [f32; 3],
    },
    /// A rectangle in XZ.
    Rect {
        /// Half extents along X and Z.
        half: [f32; 2],
    },
}

impl ShapeRecord {
    fn of(shape: EmitterShape) -> Self {
        match shape {
            EmitterShape::Sphere { radius, .. } => Self::Sphere { radius },
            EmitterShape::Box { half, .. } => Self::Box { half: half.to_array() },
            EmitterShape::Rect { half, .. } => Self::Rect { half: half.to_array() },
            EmitterShape::Point => Self::Point,
        }
    }

    fn shape(self) -> EmitterShape {
        match self {
            Self::Point => EmitterShape::Point,
            Self::Sphere { radius } => EmitterShape::sphere(radius),
            Self::Box { half } => EmitterShape::cuboid(Vec3::from(half)),
            Self::Rect { half } => EmitterShape::rect(Vec2::from(half)),
        }
    }
}

/// What a batch or a stream spawns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpawnRecord {
    /// The system's key.
    pub system: u64,
    /// Emitter to world, column-major.
    pub emitter: [f32; 16],
    /// Where in it.
    pub shape: ShapeRecord,
    /// The anchor's motion.
    pub velocity: [f32; 3],
    /// The seed.
    pub seed: u32,
    /// Atlas cells; empty for any.
    pub cells: Vec<u16>,
    /// A colour override; empty for none.
    pub colours: Vec<Colour>,
    /// The size multiplier.
    pub scale: f32,
}

/// One batch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchRecord {
    /// What it spawns.
    pub spawn: SpawnRecord,
    /// When its first particle was born.
    pub start: f64,
    /// Seconds between births.
    pub interval: f32,
    /// Its first particle's index.
    pub first: u32,
    /// Its particles.
    pub count: u32,
    /// The longest lifetime.
    pub life_max: f32,
    /// The stream slot whose open segment it is.
    pub open: Option<u8>,
    /// Counts against the weather share.
    pub weather: bool,
    /// Its scale before a clear started.
    pub fading: Option<f32>,
}

/// One stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamRecord {
    /// Its slot.
    pub slot: u8,
    /// Its key.
    pub key: u32,
    /// What it spawns.
    pub spawn: SpawnRecord,
    /// Births per second asked for.
    pub rate: f32,
    /// Births per second after the share.
    pub effective: f32,
    /// The longest lifetime.
    pub life_max: f32,
    /// A weather stream.
    pub weather: bool,
    /// The next segment's first index.
    pub next_first: u32,
    /// The next segment's first birth.
    pub next_birth: Option<f64>,
}

/// The live weather.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveRecord {
    /// The scene's.
    Scene,
    /// None.
    None,
    /// A preset override.
    Preset {
        /// Its name.
        preset: String,
        /// `0..=1`.
        intensity: f32,
        /// When it ends.
        until: Option<f64>,
    },
}

/// Everything the effects hold at one time (D14).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectCheckpoint {
    /// The scene time it was written at.
    pub at: Seconds,
    /// The cap.
    pub max_slots: u32,
    /// Weather's share of it.
    pub weather_share: f32,
    /// The systems the batches and streams use.
    pub systems: Vec<SystemRecord>,
    /// The live batches.
    pub batches: Vec<BatchRecord>,
    /// The streams.
    pub streams: Vec<StreamRecord>,
    /// The live weather.
    pub live: LiveRecord,
    /// The weather stream's system, while there is one.
    pub weather: Option<u64>,
    /// An ongoing clear: its start and fade.
    pub clear: Option<(f64, f32)>,
    /// Particles thinned so far.
    pub thinned: u64,
}

impl EffectsFrame {
    /// The checkpoint of the effects at `at`. Cold: allocates.
    pub fn checkpoint(&self, at: Seconds) -> EffectCheckpoint {
        let records: Vec<(ParticleSystemId, SystemRecord)> =
            self.systems().iter().map(|s| (s.id, SystemRecord::new(s.sprite, &s.params, s.weather))).collect();
        let key = |id: ParticleSystemId| records.iter().find(|(i, _)| *i == id).map_or(0, |(_, r)| r.key);
        let spawn_record = |s: &Spawn| SpawnRecord {
            system: key(s.system),
            emitter: s.emitter.to_cols_array(),
            shape: ShapeRecord::of(s.shape),
            velocity: s.velocity.to_array(),
            seed: s.seed,
            cells: s.cells.as_slice().to_vec(),
            colours: s.colours.map(|c| c.as_slice().to_vec()).unwrap_or_default(),
            scale: s.scale,
        };
        let parts = self.log().parts();
        let batches = parts
            .batches
            .iter()
            .zip(&parts.meta)
            .map(|(b, m)| BatchRecord {
                spawn: spawn_record(&Spawn::of(b)),
                start: b.start.0,
                interval: b.interval,
                first: b.first,
                count: b.count,
                life_max: m.life_max,
                open: m.open,
                weather: m.weather,
                fading: m.fading,
            })
            .collect();
        let streams = parts
            .streams
            .iter()
            .enumerate()
            .filter_map(|(slot, s)| {
                let s = s.as_ref()?;
                Some(StreamRecord {
                    slot: u8::try_from(slot).ok()?,
                    key: s.key.0,
                    spawn: spawn_record(&s.spawn),
                    rate: s.rate,
                    effective: s.effective,
                    life_max: s.life_max,
                    weather: s.weather,
                    next_first: s.next_first,
                    next_birth: s.next_birth.map(|b| b.0),
                })
            })
            .collect();
        let live = match self.live() {
            Live::Scene => LiveRecord::Scene,
            Live::None => LiveRecord::None,
            Live::Preset { preset, intensity, until } => {
                LiveRecord::Preset { preset: preset.name.as_str().to_owned(), intensity, until: until.map(|u| u.0) }
            }
        };
        let weather = parts.streams.iter().flatten().find(|s| s.key == WEATHER).map(|s| key(s.spawn.system));
        EffectCheckpoint {
            at,
            max_slots: parts.limits.max_slots,
            weather_share: parts.limits.weather_share,
            systems: records.into_iter().map(|(_, r)| r).collect(),
            batches,
            streams,
            live,
            weather,
            clear: parts.clear.map(|(from, fade)| (from.0, fade)),
            thinned: parts.thinned,
        }
    }

    /// Puts the effects back as `checkpoint` holds them: makes the systems it names that `renderer` lacks, maps its
    /// keys to their handles, and replaces the log and the live weather. Works into a fresh renderer and stage. Cold.
    ///
    /// # Errors
    /// [`StageError::Rebuild`] for a system that can't be made, a key no system has, or a preset that no longer exists.
    pub fn restore(&mut self, renderer: &mut dyn Renderer, checkpoint: &EffectCheckpoint) -> Result<(), StageError> {
        let bad = |reason: String| StageError::Rebuild { reason };
        let mut ids = BTreeMap::new();
        for record in &checkpoint.systems {
            let id = self
                .system_for_restore(renderer, record.sprite, &record.params, record.weather)
                .ok_or_else(|| bad(format!("the effect system {:016x} could not be made", record.key)))?;
            ids.insert(record.key, (id, matches!(record.sprite, SpriteDef::Atlas(_))));
        }
        let spawn = |r: &SpawnRecord| -> Result<Spawn, StageError> {
            let (system, atlas) =
                ids.get(&r.system).copied().ok_or_else(|| bad(format!("no system has the key {:016x}", r.system)))?;
            // An emote batch restores to the placeholder: the only cell drawn until emote images are fetched.
            let cells = if atlas { &[0][..] } else { &r.cells[..] };
            Ok(Spawn {
                shape: r.shape.shape(),
                velocity: Vec3::from(r.velocity),
                seed: r.seed,
                cells: CellPick::list(cells).unwrap_or(CellPick::All),
                colours: ColourSet::new(&r.colours),
                scale: r.scale,
                ..Spawn::new(system, Mat4::from_cols_array(&r.emitter))
            })
        };
        let mut batches = Vec::with_capacity(checkpoint.batches.len());
        let mut meta = Vec::with_capacity(checkpoint.batches.len());
        for b in &checkpoint.batches {
            let s = spawn(&b.spawn)?;
            batches.push(SpawnBatch {
                shape: s.shape,
                velocity: s.velocity,
                start: Seconds(b.start),
                interval: b.interval,
                first: b.first,
                count: b.count,
                seed: s.seed,
                cells: s.cells,
                colours: s.colours,
                scale: s.scale,
                ..SpawnBatch::new(s.system, s.emitter)
            });
            meta.push(Meta { life_max: b.life_max, open: b.open, weather: b.weather, fading: b.fading });
        }
        let mut streams = [None; MAX_STREAMS];
        for s in &checkpoint.streams {
            if let Some(slot) = streams.get_mut(usize::from(s.slot)) {
                *slot = Some(StreamState {
                    key: StreamKey(s.key),
                    spawn: spawn(&s.spawn)?,
                    rate: s.rate,
                    effective: s.effective,
                    life_max: s.life_max,
                    weather: s.weather,
                    next_first: s.next_first,
                    next_birth: s.next_birth.map(Seconds),
                });
            }
        }
        let live = match &checkpoint.live {
            LiveRecord::Scene => Live::Scene,
            LiveRecord::None => Live::None,
            LiveRecord::Preset { preset, intensity, until } => Live::Preset {
                preset: presets::find_kind(preset, EffectKind::Weather).map_err(|e| bad(e.to_string()))?,
                intensity: *intensity,
                until: until.map(Seconds),
            },
        };
        let log = EmissionLog::from_parts(&LogParts {
            limits: EffectLimits { max_slots: checkpoint.max_slots, weather_share: checkpoint.weather_share },
            batches,
            meta,
            streams,
            clear: checkpoint.clear.map(|(from, fade)| (Seconds(from), fade)),
            thinned: checkpoint.thinned,
        });
        let weather = checkpoint.weather.and_then(|k| ids.get(&k).map(|(id, _)| *id));
        self.restore_parts(log, live, weather);
        Ok(())
    }
}
