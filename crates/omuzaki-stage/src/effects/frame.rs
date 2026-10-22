//! The effects' frame step (particles.md D12): on the render thread, before the scene is rendered, it drains the
//! service's queue, resolves anchors and palette colours, appends bursts, runs the weather stream, prunes, and fills
//! `Scene::particles`. Allocation-free after a preset's first use (whose system the renderer creates then).
//!
//! **Systems.** A burst preset's system is made on its first use and kept until the stage drops. A weather system is
//! immutable once in use: a weather change makes a new system (or reuses a live one with the same description) and
//! the stream's next segment uses it, while the old segment keeps its own system and finishes with its old values (the
//! D10 cross-fade). A weather system is released once no batch uses it.

use std::sync::Arc;

use omuzaki_anim_api::Pose;
use omuzaki_char_api::CharacterRig;
use omuzaki_core::glam::{Mat4, UVec2, Vec2};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::Palette;
use omuzaki_render_api::limits::MAX_SPAWN_BATCHES;
use omuzaki_render_api::{
    AlphaEncoding, CellPick, Colour, ColourSet, EmitterShape, ParticleParams, ParticleSystemDesc, ParticleSystemId,
    Pass, PixelRect, RenderError, Renderer, RendererExt, ResourceRef, SpawnBatch, Sprite, TextureDesc, TextureId,
};
use omuzaki_stage_api::effects::{
    BurstShape, ColourSource, EffectPreset, MAX_PENDING_EFFECTS, SpriteDef, WeatherSource,
};

use super::anchor::{Anchors, Figure};
use super::atlas::{Cell, EmoteAtlas, MAX_PICK, Request as CellRequest};
use super::emotes::{CELL_PX, Done, UPLOADS_PER_FRAME};
use super::log::{EffectLimits, EmissionLog, Spawn, StreamKey};
use super::service::{
    BurstRecord, Outcome, OutcomeKind, Request, Ring, StageEffects, StatusRecord, WeatherRecord, finding,
};
use super::weather::{SceneEffects, WeatherVolume};

/// The particle systems the stage keeps at once.
pub const MAX_SYSTEMS: usize = 64;
/// The weather's stream.
pub const WEATHER: StreamKey = StreamKey(0);
/// The weather stream's seed: fixed, so the same scene rains the same drops on every run.
pub const WEATHER_SEED: u32 = 0x5EED_0001;
/// The chat-emote atlas: 8 x 8 cells of 128 px.
pub const ATLAS_CELLS: u16 = 8;
const ATLAS_CELL_PX: u32 = 128;
/// The placeholder heart's colour.
const HEART: Colour = Colour::rgb(255, 92, 138);

/// The palette slots `colours = "palette"` takes (LK-03), plus white.
const PALETTE_SLOTS: [&str; 5] = ["hair_mid", "outfit_a", "outfit_b", "trim", "eyes"];

/// One particle system on the renderer.
#[derive(Clone, Debug, PartialEq)]
pub struct System {
    /// Its handle.
    pub id: ParticleSystemId,
    /// What it looks like.
    pub sprite: SpriteDef,
    /// How it moves.
    pub params: ParticleParams,
    /// A weather system (released once unused).
    pub weather: bool,
}

/// The live weather in force.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Live {
    /// The scene's own.
    Scene,
    /// None until changed.
    None,
    /// A preset, until a time or until changed.
    Preset {
        /// The preset.
        preset: &'static EffectPreset,
        /// `0..=1`.
        intensity: f32,
        /// When the scene's weather comes back.
        until: Option<Seconds>,
    },
}

/// What one frame's step reads from the stage.
#[derive(Copy, Clone)]
pub struct FrameCtx<'a> {
    /// The frame's scene time.
    pub t: Seconds,
    /// Seconds since the last frame.
    pub dt: f32,
    /// The character's world matrix.
    pub root: Mat4,
    /// Every bone's model-space transform this frame.
    pub world: &'a Pose,
    /// The character's skeleton.
    pub rig: &'a CharacterRig,
    /// The frame's camera.
    pub camera: omuzaki_render_api::Camera,
    /// The particle density the scheduler set.
    pub density: f32,
}

/// The effects' state on the render thread.
pub struct EffectsFrame {
    service: Arc<StageEffects>,
    log: EmissionLog,
    systems: Vec<System>,
    drained: [Option<Request>; MAX_PENDING_EFFECTS],
    outbox: Ring<Outcome, MAX_PENDING_EFFECTS>,
    scene: SceneEffects,
    live: Live,
    weather: Option<ParticleSystemId>,
    prewarm: bool,
    palette: Option<ColourSet>,
    before_root: Mat4,
    before_world: Box<Pose>,
    before_camera: Mat4,
    has_before: bool,
    findings: u32,
    drawn: Option<bool>,
    atlas: Option<TextureId>,
    cells: EmoteAtlas,
    frame: u64,
    density: f32,
}

impl std::fmt::Debug for EffectsFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectsFrame")
            .field("slots", &self.log.slots())
            .field("systems", &self.systems.len())
            .field("live", &self.live)
            .finish_non_exhaustive()
    }
}

/// The palette's `colours = "palette"` set: its LK-03 slots that it has, plus white.
pub fn palette_colours(palette: &Palette) -> Option<ColourSet> {
    let mut colours = [Colour::WHITE; 6];
    let mut n = 0;
    for name in PALETTE_SLOTS {
        if let Some((_, colour)) = palette.0.iter().find(|(slot, _)| slot.as_str() == name) {
            colours[n] = *colour;
            n += 1;
        }
    }
    colours[n] = Colour::WHITE;
    ColourSet::new(&colours[..=n])
}

impl EffectsFrame {
    /// The frame state of `service` under `limits`, with `palette` for palette colours. Cold.
    pub fn new(service: Arc<StageEffects>, limits: EffectLimits, palette: Option<ColourSet>) -> Self {
        Self {
            service,
            log: EmissionLog::new(limits),
            systems: Vec::with_capacity(MAX_SYSTEMS),
            drained: [None; MAX_PENDING_EFFECTS],
            outbox: Ring::new(),
            scene: SceneEffects::default(),
            live: Live::Scene,
            weather: None,
            prewarm: true,
            palette,
            before_root: Mat4::IDENTITY,
            before_world: Box::new(Pose::IDENTITY),
            before_camera: Mat4::IDENTITY,
            has_before: false,
            findings: 0,
            drawn: None,
            atlas: None,
            cells: EmoteAtlas::chat(),
            frame: 0,
            density: 1.0,
        }
    }

    /// The service the ops reach.
    pub fn service(&self) -> &Arc<StageEffects> {
        &self.service
    }

    /// The emission log.
    pub fn log(&self) -> &EmissionLog {
        &self.log
    }

    /// The systems on the renderer.
    pub fn systems(&self) -> &[System] {
        &self.systems
    }

    /// The live weather in force.
    pub fn live(&self) -> Live {
        self.live
    }

    /// The scene's effects as last set.
    pub fn scene(&self) -> &SceneEffects {
        &self.scene
    }

    /// Takes the scene's effects (its weather, unit and floor bounds), for the next frame. Allocation-free.
    pub fn set_scene(&mut self, scene: &SceneEffects) {
        self.scene.clone_from(scene);
    }

    /// The scene was switched: the live override ends and the next weather is pre-warmed (D10).
    pub fn scene_switched(&mut self) {
        self.live = Live::Scene;
        self.prewarm = true;
        self.findings = 0;
    }

    /// The palette's colours changed (a variant swap).
    pub fn set_palette(&mut self, palette: Option<ColourSet>) {
        self.palette = palette;
    }

    /// The cap: [`EffectLimits::live`] while performing, [`EffectLimits::offline`] for renders.
    pub fn set_limits(&mut self, limits: EffectLimits) {
        self.log.set_limits(limits);
    }

    /// Where the weather now drawn comes from.
    pub fn weather_source(&self) -> WeatherSource {
        match self.live {
            Live::Scene if self.scene.weather.is_some() => WeatherSource::Scene,
            Live::Scene | Live::None => WeatherSource::None,
            Live::Preset { .. } => WeatherSource::Override,
        }
    }

    /// Runs one frame: drains the queue, applies its requests, runs the weather, prunes, and refills `particles`
    /// with the live batches. Allocation-free after each preset's first use.
    pub fn step(&mut self, renderer: &mut dyn Renderer, ctx: &FrameCtx<'_>, particles: &mut Vec<SpawnBatch>) {
        let drawn = *self.drawn.get_or_insert_with(|| renderer.capabilities().supports(Pass::Particles));
        if !drawn && !self.systems.is_empty() {
            self.systems.clear();
        }
        self.density = ctx.density;
        self.frame = self.frame.wrapping_add(1);
        self.upload(renderer);
        let t = ctx.t;
        if let Live::Preset { until: Some(until), .. } = self.live
            && t.0 >= until.0
        {
            self.live = Live::Scene;
        }
        let n = self.service.drain(&mut self.drained);
        for i in 0..n {
            if let Some(request) = self.drained[i].take() {
                self.apply(renderer, ctx, request);
            }
        }
        self.run_weather(renderer, t);
        self.log.prune(t);
        self.pin_cells();
        self.release_unused(renderer);
        particles.clear();
        if self.drawn == Some(true) {
            self.log.fill(particles);
        }
        self.remember(ctx);
        self.publish();
    }

    /// Publishes the status and hands the outcomes over.
    fn publish(&mut self) {
        let status = StatusRecord {
            slots: self.log.slots(),
            cap: self.log.limits().max_slots,
            weather: self.weather_source(),
            density: self.density,
            drawn: self.drawn.unwrap_or(false),
            thinned: self.log.thinned(),
            findings: self.findings,
        };
        self.service.publish(status, &mut self.outbox);
    }

    fn remember(&mut self, ctx: &FrameCtx<'_>) {
        self.before_root = ctx.root;
        (*self.before_world).clone_from(ctx.world);
        self.before_camera = ctx.camera.transform;
        self.has_before = true;
    }

    fn apply(&mut self, renderer: &mut dyn Renderer, ctx: &FrameCtx<'_>, request: Request) {
        let t = ctx.t;
        match request {
            Request::Burst(record) => self.burst(renderer, ctx, &record),
            Request::Weather(weather) => {
                self.live = match weather {
                    WeatherRecord::Preset { preset, intensity, for_s } => Live::Preset {
                        preset,
                        intensity,
                        until: for_s.map(|s| Seconds(t.0 + f64::from(s))),
                    },
                    WeatherRecord::None => Live::None,
                    WeatherRecord::Scene => Live::Scene,
                };
                self.outbox.push_over(Outcome { at: t, kind: OutcomeKind::Weather(weather) });
            }
            Request::Clear(fade_s) => {
                self.log.clear(t, fade_s);
                self.live = Live::Scene;
                self.outbox.push_over(Outcome { at: t, kind: OutcomeKind::Clear(fade_s) });
            }
        }
    }

    fn burst(&mut self, renderer: &mut dyn Renderer, ctx: &FrameCtx<'_>, record: &BurstRecord) {
        let preset = record.preset;
        // A replayed burst carries what was resolved when it happened (its colours included).
        let (emitter, shape, velocity, member) = if let Some((emitter, shape, velocity)) = record.fixed {
            (emitter, shape, velocity, false)
        } else {
            let anchors = Anchors {
                rig: ctx.rig,
                now: Figure { root: ctx.root, world: ctx.world },
                before: self.has_before.then_some(Figure { root: self.before_root, world: &self.before_world }),
                camera: ctx.camera,
                camera_before: self.has_before.then_some(self.before_camera),
                dt: ctx.dt,
                unit: self.scene.unit,
                aspect: self.scene.aspect,
                subject: self.scene.subject,
            };
            let at = anchors.resolve(record.at);
            self.findings |= at.findings;
            (at.emitter, at.shape, at.velocity, at.member)
        };
        let count = record.count.saturating_mul(u32::from(record.emote_count.max(1)));
        let system = self.system_for(renderer, preset.sprite, &preset.particles, false);
        let palette = preset.burst.as_ref().is_some_and(|b| b.colours == ColourSource::Palette) && member;
        let colours = record.colours.or(if palette { self.palette } else { None });
        let outcome = match system {
            Some(system) => {
                let atlas = matches!(preset.sprite, SpriteDef::Atlas(_));
                let cells = if atlas { self.emote_cells(record) } else { CellPick::All };
                let spawn = Spawn {
                    shape: emitter_shape(shape),
                    velocity,
                    seed: record.seed,
                    cells,
                    colours,
                    scale: record.scale,
                    ..Spawn::new(system, emitter)
                };
                self.log.burst(ctx.t, &spawn, count, preset.particles.lifetime.max)
            }
            None => super::log::BurstOutcome { count: 0, thinned: 0 },
        };
        self.outbox.push_over(Outcome {
            at: ctx.t,
            kind: OutcomeKind::Burst {
                record: *record,
                emitter,
                shape,
                colours,
                velocity,
                count: outcome.count,
                thinned: outcome.thinned,
            },
        });
    }

    /// The atlas cells a burst's emotes draw: each ready emote's own cell, the placeholder for the rest. A missing
    /// emote is asked for, so a later burst shows its picture; a batch keeps the cells it was born with (D13).
    fn emote_cells(&mut self, record: &BurstRecord) -> CellPick {
        let keys = &record.emotes[..usize::from(record.emote_count).min(MAX_PICK)];
        if keys.is_empty() {
            return CellPick::list(&[0]).unwrap_or(CellPick::All);
        }
        for &key in keys {
            if self.cells.lookup(key, self.frame) == Cell::Missing && self.cells.request(key) == CellRequest::Started {
                let asked = self.service.emotes().is_some_and(|pipe| pipe.want(key));
                if !asked {
                    self.cells.cancel(key);
                }
            }
        }
        let mut picked = [0u16; MAX_PICK];
        let n = self.cells.cells_for(keys, self.frame, &mut picked);
        if picked[..n].contains(&0) {
            self.findings |= finding::EMOTES;
        }
        CellPick::list(&picked[..n]).unwrap_or(CellPick::All)
    }

    /// Uploads at most two finished emote pictures into the atlas. Never waits; allocation-free.
    fn upload(&mut self, renderer: &mut dyn Renderer) {
        let service = Arc::clone(&self.service);
        let Some(pipe) = service.emotes() else { return };
        let (atlas, cells, frame) = (self.atlas, &mut self.cells, self.frame);
        pipe.take(UPLOADS_PER_FRAME, |done| match done {
            Done::Loaded { key, pixels } => {
                match (atlas, cells.insert(key, frame)) {
                    (Some(texture), Some(cell)) => {
                        let columns = u32::from(ATLAS_CELLS);
                        let rect = PixelRect {
                            x: u32::from(cell) % columns * CELL_PX,
                            y: u32::from(cell) / columns * CELL_PX,
                            width: CELL_PX,
                            height: CELL_PX,
                        };
                        if let Err(error) = renderer.update_texture(texture, rect, &pixels) {
                            tracing::warn!(%error, "a chat emote's picture could not be uploaded");
                            cells.forget(key);
                        }
                    }
                    // No atlas yet, or every cell pinned: the emote stays the placeholder until asked again.
                    (None, Some(_)) => cells.forget(key),
                    (_, None) => {}
                }
                pipe.give_back(pixels);
            }
            Done::Failed { key, .. } => cells.cancel(key),
        });
    }

    /// Pins the cells the live emote batches draw, so a picture never changes in mid-air (D13).
    fn pin_cells(&mut self) {
        self.cells.clear_pins();
        for batch in self.log.batches() {
            if let CellPick::List { .. } = batch.cells {
                for &cell in batch.cells.as_slice() {
                    if cell != 0 {
                        self.cells.pin(cell);
                    }
                }
            }
        }
    }

    /// The weather to draw now: its look, its rate and where.
    fn wanted_weather(&self) -> Option<(SpriteDef, ParticleParams, f32, WeatherVolume)> {
        let unit = self.scene.unit;
        match self.live {
            Live::Scene => self.scene.weather.as_ref().map(|w| {
                let volume = w.volume.unwrap_or_else(|| self.scene.auto.at(w.height * unit));
                (w.sprite, w.params.clone(), w.density * volume.area_m2() * w.intensity, volume)
            }),
            Live::None => None,
            Live::Preset { preset, intensity, .. } => preset.weather.as_ref().map(|w| {
                let volume = self.scene.auto.at(w.height * unit);
                (preset.sprite, preset.particles.clone(), w.density * volume.area_m2() * intensity, volume)
            }),
        }
    }

    fn run_weather(&mut self, renderer: &mut dyn Renderer, t: Seconds) {
        let wanted = if self.drawn == Some(true) { self.wanted_weather() } else { None };
        let Some((sprite, params, rate, volume)) = wanted else {
            if self.weather.take().is_some() {
                self.log.forget_stream(WEATHER);
            }
            return;
        };
        let Some(system) = self.system_for(renderer, sprite, &params, true) else { return };
        let spawn = Spawn {
            shape: EmitterShape::rect(volume.half),
            seed: WEATHER_SEED,
            ..Spawn::new(system, volume.emitter)
        };
        let life_max = params.lifetime.max;
        if self.prewarm {
            self.log.prewarm(WEATHER, t, &spawn, rate, life_max, true);
            self.prewarm = false;
        } else {
            self.log.stream(WEATHER, t, &spawn, rate, life_max, true);
        }
        self.weather = Some(system);
    }

    /// The system drawing `sprite` and `params`: a live one with that description, else a new one. `None` when it
    /// can't be made (the renderer draws no particles, it refused, or every slot is used).
    fn system_for(
        &mut self,
        renderer: &mut dyn Renderer,
        sprite: SpriteDef,
        params: &ParticleParams,
        weather: bool,
    ) -> Option<ParticleSystemId> {
        if self.drawn != Some(true) {
            return None;
        }
        if let Some(system) =
            self.systems.iter().find(|s| s.weather == weather && s.sprite == sprite && s.params == *params)
        {
            return Some(system.id);
        }
        if self.systems.len() == MAX_SYSTEMS {
            self.findings |= finding::SYSTEMS_FULL;
            return None;
        }
        let drawn_sprite = match sprite {
            SpriteDef::Shape(shape) => Sprite::Shape(shape),
            SpriteDef::Atlas(_) => Sprite::atlas(self.atlas(renderer)?, ATLAS_CELLS, ATLAS_CELLS),
        };
        let desc = ParticleSystemDesc { sprite: drawn_sprite, params: params.clone() };
        match renderer.create_particle_system(&desc) {
            Ok(id) => {
                self.systems.push(System { id, sprite, params: params.clone(), weather });
                Some(id)
            }
            Err(RenderError::Unsupported(_)) => {
                tracing::info!("the renderer draws no particles; effects are accepted and not drawn");
                self.drawn = Some(false);
                None
            }
            Err(error) => {
                tracing::warn!(%error, "a particle system could not be made");
                self.findings |= finding::SYSTEM;
                None
            }
        }
    }

    /// The chat-emote atlas, made on first use with the placeholder heart in cell 0.
    fn atlas(&mut self, renderer: &mut dyn Renderer) -> Option<TextureId> {
        if let Some(atlas) = self.atlas {
            return Some(atlas);
        }
        let side = u32::from(ATLAS_CELLS) * ATLAS_CELL_PX;
        let mut desc = TextureDesc::colour(UVec2::splat(side)).dynamic();
        desc.alpha = AlphaEncoding::Premultiplied;
        let pixels = atlas_pixels(side);
        match renderer.create_texture(&desc, Some(&pixels)) {
            Ok(atlas) => {
                self.atlas = Some(atlas);
                Some(atlas)
            }
            Err(error) => {
                tracing::warn!(%error, "the chat-emote atlas could not be made");
                self.findings |= finding::SYSTEM;
                None
            }
        }
    }

    /// Releases the weather systems no batch uses any more, other than the one in use.
    fn release_unused(&mut self, renderer: &mut dyn Renderer) {
        let mut i = 0;
        while i < self.systems.len() {
            let system = &self.systems[i];
            if system.weather && Some(system.id) != self.weather && !self.log.uses(system.id) {
                if let Err(error) = renderer.release(ResourceRef::ParticleSystem(system.id)) {
                    tracing::warn!(%error, "releasing a weather system failed");
                }
                self.systems.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Restores a log and the live weather, and the systems they use: what a checkpoint restore does. Cold.
    pub(crate) fn restore_parts(&mut self, log: EmissionLog, live: Live, weather: Option<ParticleSystemId>) {
        self.log = log;
        self.live = live;
        self.weather = weather;
        self.prewarm = false;
    }

    /// The system for a checkpoint's description, made if missing. Cold.
    pub(crate) fn system_for_restore(
        &mut self,
        renderer: &mut dyn Renderer,
        sprite: SpriteDef,
        params: &ParticleParams,
        weather: bool,
    ) -> Option<ParticleSystemId> {
        if self.drawn.is_none() {
            self.drawn = Some(renderer.capabilities().supports(Pass::Particles));
        }
        self.system_for(renderer, sprite, params, weather)
    }

    /// Gives every system and the atlas back to `renderer`.
    ///
    /// # Errors
    /// The first release the renderer refused.
    pub fn release(&mut self, renderer: &mut dyn Renderer) -> Result<(), RenderError> {
        let mut first = Ok(());
        for system in self.systems.drain(..) {
            let released = renderer.release(ResourceRef::ParticleSystem(system.id));
            first = first.and(released);
        }
        if let Some(atlas) = self.atlas.take() {
            first = first.and(renderer.release(ResourceRef::Texture(atlas)));
        }
        self.weather = None;
        self.log = EmissionLog::new(self.log.limits());
        first
    }
}

/// The render contract's shape of a recorded one.
pub fn emitter_shape(shape: BurstShape) -> EmitterShape {
    match shape {
        BurstShape::Rect { half, .. } => EmitterShape::rect(Vec2::from(half)),
        BurstShape::Point => EmitterShape::Point,
    }
}

/// The atlas's first pixels: transparent, with the placeholder heart in cell 0, premultiplied. Cold.
fn atlas_pixels(side: u32) -> Vec<u8> {
    let side = side as usize;
    let mut rgba = vec![0u8; side * side * 4];
    let cell = ATLAS_CELL_PX as usize;
    let linear = HEART;
    // Colour is sRGB bytes; the atlas is sRGB, premultiplied in its own encoding as the renderer's atlases are.
    // Four samples a pixel for a soft edge.
    let offsets = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];
    for y in 0..cell {
        for x in 0..cell {
            let covered = offsets
                .iter()
                .filter(|(dx, dy)| {
                    // The classic heart curve, (x² + y² − 1)³ − x²y³ ≤ 0, in a box of ±1.3 with +y up.
                    #[allow(clippy::cast_precision_loss)] // a 128 px cell
                    let (u, v) = (
                        ((x as f32 + dx) / cell as f32 * 2.0 - 1.0) * 1.3,
                        (1.0 - (y as f32 + dy) / cell as f32 * 2.0) * 1.3 + 0.1,
                    );
                    let a = u * u + v * v - 1.0;
                    a * a * a - u * u * v * v * v <= 0.0
                })
                .count();
            if covered == 0 {
                continue;
            }
            #[allow(clippy::cast_precision_loss)] // 0 to 4
            let alpha = covered as f32 / 4.0;
            let i = (y * side + x) * 4;
            let Colour { r, g, b, .. } = linear;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=255
            let premultiply = |c: u8| (f32::from(c) * alpha).round() as u8;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=255
            let a8 = (alpha * 255.0).round() as u8;
            rgba[i..i + 4].copy_from_slice(&[premultiply(r), premultiply(g), premultiply(b), a8]);
        }
    }
    rgba
}

/// The batches' system ids in `particles` are all live in `frame`: a stage invariant the tests check.
pub fn systems_live(frame: &EffectsFrame, particles: &[SpawnBatch]) -> bool {
    particles.iter().all(|b| frame.systems.iter().any(|s| s.id == b.system))
}

/// Room for the most batches a frame fills, so the fill never grows the list.
pub fn reserve(particles: &mut Vec<SpawnBatch>) {
    particles.reserve(MAX_SPAWN_BATCHES.saturating_sub(particles.len()));
}
