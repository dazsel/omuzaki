//! The caching character builder: [`RegistryBuilder`](omuzaki_char_api::RegistryBuilder)'s result, without
//! rebuilding what an edit did not touch.
//!
//! [`CachingBuilder`] runs the public pipeline of `omuzaki-char-api` ([`plan`], [`build_part`], [`Assembler`]) and differs
//! from the reference builder in one place only: where a part's [`PartOutput`] comes from (built now, or kept from an
//! earlier build). Everything done to an output afterwards (validation, weight brushes, the rig, errors and their order)
//! is the shared [`Assembler`], so a cached build equals the reference one byte for byte.
//!
//! # What a cache entry is keyed on
//!
//! A part's output is a pure function of its inputs ([`build_part`]), so an entry is keyed on all of them, hashed with
//! 128-bit FNV-1a:
//! - the part's id, kind and normalised parameters (stored defaults and key order don't matter);
//! - the character seed, the quality, `rig_export` and the whole face profile (every part reads them);
//! - the value in the palette of every slot the parameters name (`@slot`);
//! - what the parts before it published, as a chain over the keys of the earlier parts that published anything.
//!   Parts see each other only through [`Published`], so this is exact: a part rebuilds
//!   when, and only when, a publisher before it changed. Editing a part that publishes nothing (hair, most accessories)
//!   rebuilds that part alone; editing the body rebuilds every part after it that follows a publisher.
//!
//! Three contracts make the key sound. The first two hold for the mock kinds, and the differential tests would catch a
//! kind that broke them:
//! 1. A part reads palette colours only through the colour references in its own parameters, as
//!    [`BuildCtx::colour`](omuzaki_char_api::BuildCtx::colour) is documented to be used. A kind that looked a fixed slot
//!    up behind the parameters' back would keep a stale colour. [`conformance::check_colours_through_parameters`]
//!    rebuilds each part with every slot its parameters don't name changed and names a part whose output moved;
//!    `tests/palette_independence.rs` runs it over the kit characters.
//! 2. What a part publishes (surfaces, fits, anchors, colliders) is geometry and doesn't depend on palette colours
//!    (decision Q6). The chain therefore uses the key *without* the colours: a skin colour change rebuilds the body and
//!    head, which name the skin slots, but not the garments after them, which only see the body's shape.
//!
//!    This is **enforced, not only stated**: facts are type-erased, so the builder can't see a colour in one, and a
//!    palette-dependent fact would be served stale with no error. The conformance check in [`conformance`] builds a
//!    part of every linked kind under several palettes that differ from the character's in every slot, and fails,
//!    naming the kind, the part and the fact, if what it publishes differs. `tests/palette_independence.rs` runs it over
//!    every kind the kit characters use, and `tests/builder.rs` shows it refusing a palette-dependent publisher. It is
//!    a public function, so other crates can run it over their own kinds. A crate that adds a fact adds a probe for it
//!    there; a published fact nothing compares fails the sweep.
//! 3. A kind that overrides [`PartKind::recolour`](omuzaki_char_api::PartKind::recolour) keeps its promise: recolouring
//!    the output of a build gives exactly what a build with the new palette gives. Kinds that don't override it are
//!    rebuilt, so they are correct whatever they bake into their output.
//!
//! # A palette change never rebuilds geometry (decision Q6)
//!
//! The key has two halves: the *geometry key* (everything but the palette's colours) and the *full key* (the geometry
//! key and the colours the parameters name). When a part's full key misses but the cache holds the same part under
//! another palette (same geometry key), the builder copies that entry and asks the kind to recolour it
//! ([`recolour_part`]): materials re-resolved, baked tints rewritten, textures repainted through the build's
//! rasteriser, and no geometry touched. A kind that declines ([`Recoloured::NeedsRebuild`], the default) or fails is
//! built as before, and the error a build reports is the build's own. So a colourway swap costs a copy and a re-resolve
//! per part that names a changed slot (plus the repaint of its textures), not its build time, and
//! [`BuildTimings::built`] is empty when every kind recolours. A recoloured part is kept under its new full key like
//! any other, so swapping back and forth between colourways is served from the cache.
//!
//! The build services are not part of a key (ADR 0012: assets are content-addressed, a rasteriser is deterministic),
//! but an entry remembers whether it used the rasteriser and which assets it read, and is rebuilt when a later build
//! lacks them, so it fails exactly as the reference builder does.
//!
//! A part with a simulation is kept like any other. Its [`PartRuntime`] is stateful (a frame loop steps it), so the
//! cache keeps the output the generator returned as a template that never steps, and every hit gets a
//! [`PartOutput::fork`]: a fresh runtime in the generator's initial state, bit-identical to a rebuild, for the price
//! of copying its buffers. Hair is therefore rebuilt only when an edit touches it. The template's size
//! ([`PartRuntime::heap_bytes`]) counts toward the byte limit. A runtime that cannot be forked (the trait's default)
//! makes its part rebuild on every build and never be stored. Errors are never cached.
//!
//! # Parallelism
//!
//! A part's view is whatever the parts before it published, so parts are independent exactly while the ones before
//! them publish nothing. The builder guesses from earlier builds which kinds publish nothing, builds such a run (and
//! the first part after it) on scoped threads against one view, then merges in build order. If a guess was wrong, the
//! parts built against a stale view are dropped and redone, so the thread count never changes the result.

pub mod conformance;

use std::collections::{BTreeSet, HashMap};
use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use std::{mem, thread};

use omuzaki_char_api::keying::{CommonKeyInputs, PartKeyInputs, common_key_inputs, part_key_inputs};
use omuzaki_char_api::{
    Assembler, AssetError, AssetSource, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId,
    CharacterBuilder, CharacterDef, ContentHash, PartId, PartKindId, PartMesh, PartOutput, PartRuntime, Published,
    Publishes, Quality, RasterizerFactory, Recoloured, build_part, find_kind, plan, recolour_part,
};
use omuzaki_core::colour::Colour;
use omuzaki_doc_api::Value;
use omuzaki_face_api::{ColourRef, Palette};

/// Threads a default builder uses at most: parts are small, and past this the merge dominates.
const MAX_DEFAULT_THREADS: usize = 8;
/// Parts a default builder keeps (an edit session produces one entry per slider step of the part being dragged).
const DEFAULT_MAX_PARTS: usize = 512;
/// Bytes of geometry, textures and simulation state a default builder keeps.
const DEFAULT_MAX_BYTES: usize = 256 << 20;

// ------------------------------------------------------------------------------------------------ fingerprints

/// A 128-bit FNV-1a hash, wide enough that two different keys colliding is not something to reason about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Fingerprint(u128);

/// Hashes bytes into a [`Fingerprint`]; `io::Write` lets `serde_json` stream a value into it without a buffer.
struct Fnv(u128);

impl Fnv {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        for &b in bytes {
            self.0 = (self.0 ^ u128::from(b)).wrapping_mul(Self::PRIME);
        }
        self
    }

    fn word(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    /// Length-prefixed, so neighbouring strings can't run together into the same bytes.
    fn text(&mut self, s: &str) -> &mut Self {
        self.word(u64::try_from(s.len()).unwrap_or(u64::MAX)).bytes(s.as_bytes())
    }

    fn print(&mut self, f: Fingerprint) -> &mut Self {
        self.bytes(&f.0.to_le_bytes())
    }

    fn finish(&self) -> Fingerprint {
        Fingerprint(self.0)
    }
}

impl Write for Fnv {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Collects the palette slots a parameter tree names (`"@hair_root"`). A string that merely starts with `@` is
/// collected too: naming an extra slot only makes a key more specific, never wrong.
fn named_slots<'v>(value: &'v Value, out: &mut BTreeSet<&'v str>) {
    match value {
        Value::String(s) if s.starts_with('@') => {
            out.insert(s);
        }
        Value::Array(items) => items.iter().for_each(|v| named_slots(v, out)),
        Value::Object(fields) => fields.values().for_each(|v| named_slots(v, out)),
        _ => {}
    }
}

/// What one build shares across its parts, and the means to key a part.
struct Run<'a> {
    def: &'a CharacterDef,
    options: &'a BuildOptions,
    services: &'a BuildServices,
    /// Template, seed, quality, export flag and face profile: parts read all five, so all five are in every key. Hashed once.
    common: Fingerprint,
}

impl<'a> Run<'a> {
    fn new(def: &'a CharacterDef, options: &'a BuildOptions, services: &'a BuildServices) -> Self {
        // `common_key_inputs` destructures the definition and the options exhaustively: a field added to either fails
        // to compile there until it is keyed or excluded with a reason.
        let CommonKeyInputs { template, seed, quality, rig_export, face } = common_key_inputs(def, options);
        let quality = match quality {
            Quality::Live => 0,
            Quality::Offline => 1,
        };
        let mut h = Fnv::new();
        h.text(template.as_str()).word(u64::from(seed)).word(quality).word(u64::from(rig_export));
        // Can't fail: `Fnv::write` is infallible, and a profile that couldn't serialise couldn't be stored either.
        let _ = serde_json::to_writer(&mut h, face);
        Self { def, options, services, common: h.finish() }
    }

    /// The keys of `part` when the parts before it published what `chain` stands for.
    fn keys(&self, part: &PartId, chain: Fingerprint) -> Keys {
        let mut h = Fnv::new();
        h.print(self.common).print(chain).text(part.as_str());
        let params = self.def.parts.get(part).map(|part_def| {
            // Exhaustive for the same reason as in `new`: see `part_key_inputs`.
            let PartKeyInputs { kind, params, attach } = part_key_inputs(part_def);
            h.text(kind.as_str());
            let _ = serde_json::to_writer(&mut h, attach); // an `Option<Attach>` always serialises
            // Invalid parameters hash as stored; they fail to build, and failures are never cached.
            let normalised = find_kind(kind.as_str()).and_then(|registration| (registration.check)(params).ok());
            h.word(u64::from(normalised.is_some()));
            normalised.unwrap_or_else(|| params.clone())
        });
        let mut slots = BTreeSet::new();
        if let Some(params) = &params {
            let _ = serde_json::to_writer(&mut h, params); // a `Value` always serialises, and `Fnv::write` never fails
            named_slots(params, &mut slots);
        }
        let geometry = h.finish();
        let mut h = Fnv::new();
        h.print(geometry);
        for slot in slots {
            h.text(slot);
            match resolve(&self.def.palette, slot) {
                Some(c) => h.bytes(&[1, c.r, c.g, c.b, c.a]),
                None => h.bytes(&[0]),
            };
        }
        Keys { geometry, full: h.finish() }
    }
}

/// What identifies a part's output.
#[derive(Clone, Copy)]
struct Keys {
    /// Everything but the palette's colours. What the part publishes is a function of this alone (parts publish
    /// geometry, and geometry doesn't depend on colour: decision Q6), so the parts after it chain on this key.
    geometry: Fingerprint,
    /// The geometry key and the colours the parameters name: the key of the part's whole output, materials included.
    full: Fingerprint,
}

fn resolve(palette: &Palette, slot: &str) -> Option<Colour> {
    palette.resolve(&ColourRef::parse(slot).ok()?).ok()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while the lock was held can't leave the cache half-updated (every update is a single map operation).
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ------------------------------------------------------------------------------------------------ the cache

/// What a part read through the build services while it built.
#[derive(Default)]
struct Used {
    assets: Mutex<Vec<ContentHash>>,
    rasterizer: AtomicBool,
}

/// An asset source that notes what is read from it.
struct RecordingAssets {
    inner: Arc<dyn AssetSource>,
    used: Arc<Used>,
}

impl AssetSource for RecordingAssets {
    fn get(&self, hash: &ContentHash) -> Result<Arc<[u8]>, AssetError> {
        lock(&self.used.assets).push(*hash);
        self.inner.get(hash)
    }
}

/// The same services, noting what a part uses.
fn observe(services: &BuildServices) -> (BuildServices, Arc<Used>) {
    let used = Arc::new(Used::default());
    let assets = services.assets.as_ref().map(|inner| {
        Arc::new(RecordingAssets { inner: Arc::clone(inner), used: Arc::clone(&used) }) as Arc<dyn AssetSource>
    });
    let rasterizer = services.rasterizer.as_ref().map(|factory| {
        let (factory, used) = (Arc::clone(factory), Arc::clone(&used));
        Arc::new(move || {
            used.rasterizer.store(true, Ordering::Relaxed);
            factory()
        }) as RasterizerFactory
    });
    (BuildServices { assets, rasterizer }, used)
}

/// A finished part, kept for reuse: a template [`PartOutput`] that is only ever forked, and what keeping it costs.
struct Stored {
    /// A fork of the output as the generator returned it. Never stepped: each build gets a fresh fork of it.
    template: PartOutput,
    /// The services the part used, to refuse the entry when a later build lacks them.
    assets: Vec<ContentHash>,
    rasterizer: bool,
    /// What the entry counts toward the byte limit ([`output_bytes`]).
    bytes: usize,
}

/// The bytes a kept output holds: its geometry and textures, and its simulation's own
/// [`heap_bytes`](PartRuntime::heap_bytes), so a state the mesh sizes don't show still counts toward the limit.
fn output_bytes(output: &PartOutput) -> usize {
    let mesh_bytes = |m: &PartMesh| {
        let g = &m.geometry;
        mem::size_of_val(g.positions.as_slice())
            + mem::size_of_val(g.normals.as_slice())
            + mem::size_of_val(g.uvs.as_slice())
            + mem::size_of_val(g.colours.as_slice())
            + mem::size_of_val(g.joints.as_slice())
            + mem::size_of_val(g.weights.as_slice())
            + mem::size_of_val(g.indices.as_slice())
            + g.extra.iter().map(|e| mem::size_of_val(e.data.as_slice())).sum::<usize>()
    };
    output.meshes.iter().map(mesh_bytes).sum::<usize>()
        + output.textures.iter().map(|(_, t)| t.rgba8.len()).sum::<usize>()
        + output.runtime.as_deref().map_or(0, PartRuntime::heap_bytes)
}

impl Stored {
    /// Keeps a fork of `output`. `None` if the part has a simulation that cannot be forked, which makes the part
    /// uncacheable.
    fn of(output: &PartOutput, used: &Used) -> Option<Self> {
        let template = output.fork()?;
        let mut assets = lock(&used.assets).clone();
        assets.sort_unstable();
        assets.dedup();
        Some(Self {
            bytes: output_bytes(&template),
            template,
            assets,
            rasterizer: used.rasterizer.load(Ordering::Relaxed),
        })
    }

    /// The output for one build: a fresh fork of the template. `None` if the template no longer forks, which a runtime
    /// breaking its contract could cause; the caller then builds the part.
    fn output(&self) -> Option<PartOutput> {
        self.template.fork()
    }

    /// This entry for an output recoloured from `base`: it depends on everything `base` read (the recolour only adds a
    /// rasteriser use or an asset read of its own, which `Stored::of` already noted).
    fn recoloured_from(mut self, base: &Self) -> Self {
        self.assets.extend_from_slice(&base.assets);
        self.assets.sort_unstable();
        self.assets.dedup();
        self.rasterizer |= base.rasterizer;
        self
    }

    /// Whether this build's services still provide what the part used, so serving it fails exactly where building would.
    fn usable_with(&self, services: &BuildServices) -> bool {
        (!self.rasterizer || services.rasterizer.is_some())
            && self.assets.iter().all(|hash| services.assets.as_ref().is_some_and(|a| a.get(hash).is_ok()))
    }
}

/// Whether a part's output changes what later parts see. Mirrors [`Published::absorb`], which adds nothing for it.
fn is_silent(p: &Publishes) -> bool {
    p.is_empty()
}

struct Resident {
    part: Arc<Stored>,
    last_used: u64,
    /// The geometry key of the entry, to find the part again under another palette.
    geometry: Fingerprint,
}

#[derive(Default)]
struct Store {
    parts: HashMap<Fingerprint, Resident>,
    /// The most recent entry of each geometry key: the full key to recolour from when a part's palette changed.
    latest: HashMap<Fingerprint, Fingerprint>,
    bytes: usize,
    clock: u64,
    /// Whether the last parts of each kind published nothing: the guess that decides what builds in parallel. Kept
    /// when the entries are cleared.
    silent: HashMap<PartKindId, bool>,
}

impl Store {
    fn get(&mut self, key: Fingerprint) -> Option<Arc<Stored>> {
        self.clock += 1;
        let resident = self.parts.get_mut(&key)?;
        resident.last_used = self.clock;
        Some(Arc::clone(&resident.part))
    }

    /// The entry to recolour for a part whose full key missed: the latest one with the same geometry key (the same part
    /// under another palette).
    fn base(&mut self, geometry: Fingerprint) -> Option<Arc<Stored>> {
        let full = *self.latest.get(&geometry)?;
        self.get(full)
    }

    /// Keeps `part` under `keys`, then drops the least recently used entries until the limits hold.
    fn insert(&mut self, keys: Keys, part: Stored, max_parts: usize, max_bytes: usize) {
        self.clock += 1;
        self.bytes += part.bytes;
        let resident = Resident { part: Arc::new(part), last_used: self.clock, geometry: keys.geometry };
        let old = self.parts.insert(keys.full, resident);
        self.bytes -= old.map_or(0, |r| r.part.bytes);
        self.latest.insert(keys.geometry, keys.full);
        while self.parts.len() > max_parts || self.bytes > max_bytes {
            let Some(oldest) = self.parts.iter().min_by_key(|(_, r)| r.last_used).map(|(k, _)| *k) else { break };
            self.evict(oldest);
        }
    }

    fn evict(&mut self, key: Fingerprint) {
        let Some(gone) = self.parts.remove(&key) else { return };
        self.bytes -= gone.part.bytes;
        if self.latest.get(&gone.geometry) == Some(&key) {
            self.latest.remove(&gone.geometry);
        }
    }

    fn clear(&mut self) {
        self.parts.clear();
        self.latest.clear();
        self.bytes = 0;
    }
}

// ------------------------------------------------------------------------------------------------ timings

/// Where a part's output came from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PartSource {
    /// The part's generator ran.
    Built,
    /// The output was kept from an earlier build.
    Cached,
    /// The output of the same part under another palette was kept, and its kind recoloured it: materials, baked tints
    /// and textures, no geometry.
    Recoloured,
}

/// What one part cost in one build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartTiming {
    /// The part.
    pub part: PartId,
    /// Whether it was built, recoloured or served from the cache.
    pub source: PartSource,
    /// Time to build it, or to copy it out of the cache. Parts of a parallel run overlap, so these can add up to more
    /// than [`BuildTimings::total`].
    pub elapsed: Duration,
}

/// What one build cost, for the report card and the frame-budget harness.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildTimings {
    /// One entry per part, in build order.
    pub parts: Vec<PartTiming>,
    /// The whole build: ordering, keys, building, merging and assembling the rig.
    pub total: Duration,
}

impl BuildTimings {
    /// The parts whose generators ran, in build order.
    pub fn built(&self) -> impl Iterator<Item = &PartId> {
        self.of(PartSource::Built)
    }

    /// The parts recoloured from the cache instead of built, in build order.
    pub fn recoloured(&self) -> impl Iterator<Item = &PartId> {
        self.of(PartSource::Recoloured)
    }

    fn of(&self, source: PartSource) -> impl Iterator<Item = &PartId> {
        self.parts.iter().filter(move |t| t.source == source).map(|t| &t.part)
    }
}

// ------------------------------------------------------------------------------------------------ the builder

/// A part's output on its way into the assembler.
struct Ready {
    output: PartOutput,
    /// Present for a part just built that can be kept; `None` for a hit, and for a part whose simulation can't be forked.
    keep: Option<Stored>,
}

struct Fetch {
    source: PartSource,
    took: Duration,
    result: Result<Ready, BuildError>,
}

/// One part of a wave, with the entry that already answers it, if any, or else the entry of the same part under
/// another palette, which its kind may recolour.
struct Job<'a> {
    part: &'a PartId,
    hit: Option<Arc<Stored>>,
    base: Option<Arc<Stored>>,
}

/// Builds characters like [`RegistryBuilder`](omuzaki_char_api::RegistryBuilder), keeping each part's output so an edit
/// rebuilds only the part it changed and the parts that depend on it, in parallel where parts are independent. See the
/// [module docs](self) for the cache key and the parallel schedule.
pub struct CachingBuilder {
    store: Mutex<Store>,
    last: Mutex<Option<BuildTimings>>,
    threads: usize,
    max_parts: usize,
    max_bytes: usize,
}

impl Default for CachingBuilder {
    fn default() -> Self {
        let cores = thread::available_parallelism().map_or(1, NonZeroUsize::get);
        Self {
            store: Mutex::default(),
            last: Mutex::default(),
            threads: cores.min(MAX_DEFAULT_THREADS),
            max_parts: DEFAULT_MAX_PARTS,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl CachingBuilder {
    /// A builder with an empty cache, using up to one thread per core (at most 8).
    pub fn new() -> Self {
        Self::default()
    }

    /// The same builder building on at most `threads` threads (`1` builds strictly in order). The result never depends
    /// on it.
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    /// The same builder keeping at most `max_parts` parts and about `max_bytes` bytes of geometry, textures and
    /// simulation state ([`PartRuntime::heap_bytes`]), least recently used first.
    #[must_use]
    pub fn with_limits(mut self, max_parts: usize, max_bytes: usize) -> Self {
        self.max_parts = max_parts;
        self.max_bytes = max_bytes;
        self
    }

    /// How many parts are cached.
    pub fn cached_parts(&self) -> usize {
        lock(&self.store).parts.len()
    }

    /// Forgets every cached part. (What the builder learnt about which kinds publish nothing is kept: it only decides
    /// what runs in parallel.)
    pub fn clear(&self) {
        lock(&self.store).clear();
    }

    /// What the most recent finished build cost; `None` before the first. With builds on several threads, whichever
    /// finished last.
    pub fn last_timings(&self) -> Option<BuildTimings> {
        lock(&self.last).clone()
    }

    /// Builds `def` and says what each part cost and whether it was built or cached.
    ///
    /// # Errors
    /// The error [`RegistryBuilder`](omuzaki_char_api::RegistryBuilder) gives for `def`: the same variant, naming the same
    /// part.
    pub fn build_timed(
        &self,
        id: &CharId,
        def: &CharacterDef,
        options: &BuildOptions,
        services: &BuildServices,
    ) -> Result<(BuiltCharacter, BuildTimings), BuildError> {
        let started = Instant::now();
        let order = plan(def)?;
        let run = Run::new(def, options, services);
        let mut assembler = Assembler::new(id, def, options);
        let (mut chain, mut at) = (Fingerprint(0), 0);
        let mut parts = Vec::with_capacity(order.len());
        while at < order.len() {
            at = self.wave(&run, &order, at, &mut chain, &mut assembler, &mut parts)?;
        }
        let built = assembler.finish()?;
        let timings = BuildTimings { parts, total: started.elapsed() };
        *lock(&self.last) = Some(timings.clone());
        Ok((built, timings))
    }

    /// Applies a new palette to `built` in place without rebuilding any geometry the kinds can spare (decision Q6).
    ///
    /// Only parts whose parameters name a slot that changed are touched; every other part is served from the cache
    /// untouched. A part that does name one is recoloured from its cached build under the old palette: its kind
    /// re-resolves the material colours, rewrites the tints it baked into vertex colours and repaints the textures it
    /// painted ([`PartKind::recolour`](omuzaki_char_api::PartKind::recolour)). Only a part whose kind declines (or that
    /// isn't cached any more) is built in full. The result equals a fresh build of `built.def` with `palette`.
    /// Simulations restart, as in any build. The character keeps its look (LK-06): the rebuilt materials are drawn in
    /// it again. On error `built` is left as it was.
    ///
    /// `options` and `services` are the ones `built` was built with; a built character doesn't record them.
    ///
    /// # Errors
    /// As [`CachingBuilder::build_timed`]; for example a part names a slot the new palette lacks.
    pub fn rebuild_materials(
        &self,
        built: &mut BuiltCharacter,
        palette: &Palette,
        options: &BuildOptions,
        services: &BuildServices,
    ) -> Result<BuildTimings, BuildError> {
        let mut def = built.def.clone();
        def.palette = palette.clone();
        let (mut rebuilt, timings) = self.build_timed(&built.id, &def, options, services)?;
        rebuilt.inherit_look(built);
        *built = rebuilt;
        Ok(timings)
    }

    /// Adds the parts of one wave to `assembler`; returns the index of the next part to do.
    ///
    /// A wave is a run of parts that are expected to publish nothing and the part after it: all of them see the same
    /// published view, so they are looked up and built together. They are then merged in build order. A part that
    /// publishes before the last of the wave ends it early: the rest saw a stale view and are dropped (and redone by
    /// the next wave).
    fn wave(
        &self,
        run: &Run<'_>,
        order: &[PartId],
        at: usize,
        chain: &mut Fingerprint,
        assembler: &mut Assembler<'_>,
        timings: &mut Vec<PartTiming>,
    ) -> Result<usize, BuildError> {
        let end = self.wave_end(run.def, order, at);
        let members = &order[at..end];
        let keys: Vec<_> = members.iter().map(|part| run.keys(part, *chain)).collect();
        let resident: Vec<_> = {
            let mut store = lock(&self.store);
            keys.iter()
                .map(|keys| {
                    let hit = store.get(keys.full);
                    // Only a miss looks for the part under another palette.
                    let base = if hit.is_none() { store.base(keys.geometry) } else { None };
                    (hit, base)
                })
                .collect()
        };
        // Checked outside the lock: confirming an asset is still readable may touch the store.
        let usable = |s: Arc<Stored>| s.usable_with(run.services).then_some(s);
        let jobs: Vec<_> = members
            .iter()
            .zip(resident)
            .map(|(part, (hit, base))| Job { part, hit: hit.and_then(usable), base: base.and_then(usable) })
            .collect();
        let misses = jobs.iter().filter(|j| j.hit.is_none()).count();
        let helpers = self.threads.min(misses).saturating_sub(1);
        let view = assembler.published();
        let fetched = parallel(helpers, &jobs, |job| fetch(run, view, job)).ok_or_else(|| BuildError::Part {
            part: order[at].clone(),
            message: "internal error: a build worker returned no result: please report this".to_owned(),
        })?;

        for (i, ((part, keys), fetch)) in members.iter().zip(&keys).zip(fetched).enumerate() {
            let Fetch { source, took, result } = fetch;
            let Ready { output, keep } = result?;
            let silent = is_silent(&output.publishes);
            assembler.add(part, output)?;
            if let Some(stored) = keep {
                lock(&self.store).insert(*keys, stored, self.max_parts, self.max_bytes);
            }
            timings.push(PartTiming { part: part.clone(), source, elapsed: took });
            self.learn(run.def, part, silent);
            if !silent {
                *chain = Fnv::new().print(*chain).print(keys.geometry).finish();
                if i + 1 < members.len() {
                    return Ok(at + i + 1);
                }
            }
        }
        Ok(end)
    }

    /// One past the last part of the wave starting at `at`: the first part not expected to be silent ends it. With one
    /// thread a wave is a single part, so nothing is ever built against a guessed view.
    fn wave_end(&self, def: &CharacterDef, order: &[PartId], at: usize) -> usize {
        if self.threads <= 1 {
            return at + 1;
        }
        let store = lock(&self.store);
        let silent =
            |part: &PartId| def.parts.get(part).and_then(|p| store.silent.get(&p.kind)).copied().unwrap_or(false);
        order[at..].iter().position(|part| !silent(part)).map_or(order.len(), |p| at + p + 1)
    }

    /// Records whether a part of this kind published nothing. A kind that has ever published is never guessed silent
    /// again: a wrong guess costs a rebuild, a missed one only parallelism.
    fn learn(&self, def: &CharacterDef, part: &PartId, silent: bool) {
        if let Some(part_def) = def.parts.get(part) {
            lock(&self.store).silent.entry(part_def.kind.clone()).and_modify(|s| *s &= silent).or_insert(silent);
        }
    }
}

impl CharacterBuilder for CachingBuilder {
    fn build(
        &self,
        id: &CharId,
        def: &CharacterDef,
        options: &BuildOptions,
        services: &BuildServices,
    ) -> Result<BuiltCharacter, BuildError> {
        self.build_timed(id, def, options, services).map(|(built, _)| built)
    }
}

/// Answers one job: the cached output, the output of the same part under another palette recoloured, or a fresh build
/// against `view`.
fn fetch(run: &Run<'_>, view: &Published, job: &Job<'_>) -> Fetch {
    let started = Instant::now();
    if let Some(output) = job.hit.as_ref().and_then(|stored| stored.output()) {
        return Fetch { source: PartSource::Cached, took: started.elapsed(), result: Ok(Ready { output, keep: None }) };
    }
    if let Some(ready) = job.base.as_deref().and_then(|base| recoloured(run, job.part, base)) {
        return Fetch { source: PartSource::Recoloured, took: started.elapsed(), result: Ok(ready) };
    }
    let (services, used) = observe(run.services);
    let result = build_part(run.def, job.part, run.options, &services, view).map(|output| {
        let keep = Stored::of(&output, &used);
        Ready { output, keep }
    });
    Fetch { source: PartSource::Built, took: started.elapsed(), result }
}

/// `base`, which was built (or recoloured) under another palette, brought up to the palette of the run by the part's
/// kind. `None` when the kind declines or fails: the caller builds the part, so the error of a failed recolour is
/// never reported, the build's is.
fn recoloured(run: &Run<'_>, part: &PartId, base: &Stored) -> Option<Ready> {
    let (services, used) = observe(run.services);
    let mut output = base.output()?;
    match recolour_part(run.def, part, &services, &mut output) {
        Ok(Recoloured::Done) => {
            let keep = Stored::of(&output, &used).map(|stored| stored.recoloured_from(base));
            Some(Ready { output, keep })
        }
        Ok(Recoloured::NeedsRebuild) | Err(_) => None,
    }
}

/// Runs `work` on every job, the calling thread and `helpers` more taking jobs in order, and returns the results in job
/// order (`None` if one is missing). Results are independent of `helpers`.
fn parallel<T: Sync, R: Send>(helpers: usize, jobs: &[T], work: impl Fn(&T) -> R + Sync) -> Option<Vec<R>> {
    let slots: Vec<Mutex<Option<R>>> = jobs.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let drain = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let (Some(job), Some(slot)) = (jobs.get(i), slots.get(i)) else { break };
            *lock(slot) = Some(work(job));
        }
    };
    thread::scope(|scope| {
        for _ in 0..helpers {
            scope.spawn(drain);
        }
        drain();
    });
    // `None` if a job has no result, which can't happen: a job's result is stored before the worker takes the next.
    slots.into_iter().map(|slot| slot.into_inner().unwrap_or_else(PoisonError::into_inner)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv(bytes: &[u8]) -> u128 {
        Fnv::new().bytes(bytes).finish().0
    }

    #[test]
    fn the_hash_is_128_bit_fnv_1a() {
        assert_eq!(fnv(b""), 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d);
        assert_eq!(fnv(b"a"), 0xd228_cb69_6f1a_8caf_7891_2b70_4e4a_8964);
        assert_eq!(fnv(b"foobar"), 0x343e_1662_793c_64bf_6f0d_3597_ba44_6f18);
    }

    #[test]
    fn colour_slots_are_found_anywhere_in_the_parameters() {
        let params = serde_json::json!({ "a": "@skin", "b": [{ "c": "#fff000" }, "@outfit"], "d": "@skin" });
        let mut slots = BTreeSet::new();
        named_slots(&params, &mut slots);
        assert_eq!(slots.into_iter().collect::<Vec<_>>(), ["@outfit", "@skin"]);
    }

    #[test]
    fn jobs_finish_in_order_whatever_the_helpers() {
        let jobs: Vec<u64> = (0..40).collect();
        for helpers in [0, 1, 7] {
            let out = parallel(helpers, &jobs, |j| j * j).unwrap();
            assert_eq!(out, jobs.iter().map(|j| j * j).collect::<Vec<_>>());
        }
    }

    /// A simulation of no visible size that says it holds `hidden` bytes.
    struct Hidden(usize);

    impl PartRuntime for Hidden {
        fn step(&mut self, _: &omuzaki_char_api::StepCtx<'_>) {}
        fn reset(&mut self, _: &dyn omuzaki_char_api::PoseSource) {}
        fn dynamic_count(&self) -> usize {
            0
        }
        fn dynamic_mesh(&self, _: usize) -> Option<omuzaki_char_api::DynamicMesh<'_>> {
            None
        }
        fn cost(&self) -> omuzaki_char_api::RuntimeCost {
            omuzaki_char_api::RuntimeCost::default()
        }
        fn fork(&self) -> Option<Box<dyn PartRuntime>> {
            Some(Box::new(Self(self.0)))
        }
        fn heap_bytes(&self) -> usize {
            self.0
        }
    }

    #[test]
    fn a_simulations_hidden_state_counts_toward_the_byte_budget() {
        let output = PartOutput::new().with_runtime(Box::new(Hidden(4096)));
        let stored = Stored::of(&output, &Used::default()).expect("forks");
        assert_eq!(stored.bytes, 4096, "no mesh and no texture: the runtime's own size is all there is");
        let mut store = Store::default();
        store.insert(Keys { geometry: Fingerprint(1), full: Fingerprint(1) }, stored, usize::MAX, 4095);
        assert_eq!(store.parts.len(), 0, "a part whose simulation alone exceeds the budget is not kept");
    }

    fn stored(bytes: usize) -> Stored {
        Stored { template: PartOutput::default(), assets: Vec::new(), rasterizer: false, bytes }
    }

    #[test]
    fn the_cache_drops_the_least_recently_used_part_first() {
        let key = |n| Keys { geometry: Fingerprint(n), full: Fingerprint(n) };
        let (a, b, c) = (Fingerprint(1), Fingerprint(2), Fingerprint(3));
        let mut store = Store::default();
        store.insert(key(1), stored(10), 2, 1000);
        store.insert(key(2), stored(10), 2, 1000);
        assert!(store.get(a).is_some(), "a is now newer than b");
        store.insert(key(3), stored(10), 2, 1000);
        assert!(store.get(b).is_none() && store.get(a).is_some() && store.get(c).is_some());
        store.insert(key(2), stored(995), 2, 1000);
        assert_eq!((store.parts.len(), store.bytes), (1, 995), "the count limit drops a, the byte limit then drops c");
    }

    #[test]
    fn the_latest_entry_of_a_geometry_key_is_the_one_to_recolour_and_goes_when_it_is_evicted() {
        let geometry = Fingerprint(7);
        let (old, new) = (Fingerprint(70), Fingerprint(71));
        let mut store = Store::default();
        assert!(store.base(geometry).is_none());
        store.insert(Keys { geometry, full: old }, stored(1), 8, 1000);
        store.insert(Keys { geometry, full: new }, stored(1), 8, 1000);
        assert!(Arc::ptr_eq(&store.base(geometry).unwrap(), &store.get(new).unwrap()), "the newer palette's entry");
        store.evict(old);
        assert!(store.base(geometry).is_some(), "evicting the older entry keeps the pointer to the newer");
        store.evict(new);
        assert!(store.base(geometry).is_none() && store.latest.is_empty(), "no pointer outlives its entry");
    }
}
