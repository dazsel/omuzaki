//! The palette-independence conformance check: the rule that makes the caching builder's key sound.
//!
//! The caching builder keys a part that reads facts on the *geometry* keys of the parts that published them, never on
//! their colours (decision Q6: a palette change never rebuilds geometry). That is exact only while no fact depends on the
//! palette. `Fact` values are type-erased and have no equality, so the rule is enforced here instead: [`check`] builds a
//! part of each kind under the character's own palette and [`PALETTES`] others (each differing from it in *every*
//! entry, so a kind that only reacts past a threshold is still caught) and compares what it published, fact by fact.
//! Each fact says how its values are compared, beside the fact (`register_fact_probe!`): with `==`, or with a function
//! (`HeadFact` and `FitFact`, which hold trait objects with no equality, compare their `Debug` text). A kind that
//! publishes something different under another palette is a [`Report::violations`] entry naming the kind, the part and the fact.
//!
//! The palettes also differ in their slot set (one with an extra slot, one without the slots no part names, and one
//! without each slot a part names), and the whole comparison runs under each of the [`OPTIONS`] (both qualities, with
//! and without `rig_export`), because a fact may depend on either; a violation names the options and the seed it showed
//! under. The slot-set palettes are compared under the first option set only, and the seeds ([`SEEDS`]) are swept with
//! the base's palette against the first colour alternative only, so a fact that depends on a slot set and on the
//! options, or on the seed and on a slot set (or on a colour that is not the first alternative), is not caught: the
//! known gaps, accepted for the build cost.
//!
//! Facts are compared by [`Probes::registered`], read from the registry: a crate that adds a fact adds
//! `register_fact_probe!` beside it, and [`Probes::unprobed_facts`] and [`Probes::probe_problems`] are what a gate test
//! keeps empty. A fact a kind
//! publishes that no probe covers is listed in [`Report::unprobed`] instead of passing unseen. Any crate can run the check over its own kinds and characters.

use std::any::TypeId;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::thread;

use omuzaki_char_api::Fact;
use omuzaki_char_api::fact::{Differs, FactProbe};
use omuzaki_char_api::{
    BuildOptions, BuildServices, CharacterDef, PartDef, PartId, PartKindId, Published, Publishes, Quality, build_part,
    fact, find_kind, plan,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::rng::Rng;
use omuzaki_doc_api::Value;
use omuzaki_face_api::{Palette, PaletteSlot};

use super::named_slots;

/// How many palettes of other colours each kind is built under, besides the character's own: the inverted one and
/// generated ones. [`alternatives`] adds the slot-set ones after them.
pub const PALETTES: usize = 5;
/// How many seeds each base is checked at: its own, then `SEEDS - 1` more. A fact can read the palette only on the
/// draws some seeds give (a kind that chooses through `ctx.rng` whether to), so one seed is not enough.
pub const SEEDS: usize = 5;
/// The step from one swept seed to the next: added, not `XORed`, since XOR with the same constant twice returns to the
/// start, so a chain of two would check one seed twice.
const SEED_STEP: u32 = 0x9e37_79b9;
/// The build options every palette is compared under: both qualities, each with and without `rig_export`. (`variant`
/// stays `None`: variants are resolved into the definition before a build.)
pub const OPTIONS: [(Quality, bool); 4] =
    [(Quality::Live, false), (Quality::Live, true), (Quality::Offline, false), (Quality::Offline, true)];
/// How much of the matrix a check builds: how many seeds and which option sets. [`Sweep::FULL`] is every seed and
/// option set ([`SEEDS`], [`OPTIONS`]), what [`check`] and [`check_all`] run; a test held to a time limit can run fewer
/// with [`check_with`] and [`check_all_with`] and the full sweep elsewhere.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Sweep {
    /// Seeds per base, from its own (at least 1).
    pub seeds: usize,
    /// The option sets compared under; the first gets the slot-set palettes too.
    pub options: &'static [(Quality, bool)],
}

impl Sweep {
    /// Every seed and option set.
    pub const FULL: Self = Self { seeds: SEEDS, options: &OPTIONS };
    /// The base's own seed, under one option set of each quality: Live without `rig_export` (which gets the slot-set
    /// palettes) and Offline with it.
    pub const QUICK: Self = Self { seeds: 1, options: &[(Quality::Live, false), (Quality::Offline, true)] };
}

/// The seed of the generated palettes: fixed, so a failure reproduces.
const SEED: u64 = 0x1598_fac7;

/// The id of the part the check adds for a kind the character has no part of (a suffix is added if `base` has it).
const PROBE: &str = "palette-probe";
/// The slot of the alternative that has one more entry than the palette (a suffix is added if the palette or a part names it).
const EXTRA_SLOT: &str = "unused_probe_slot";

/// How one fact is compared: its name and type, whether a published fact is it, and the comparison.
struct Entry {
    name: &'static str,
    type_id: TypeId,
    holds: fn(&Publishes) -> bool,
    differs: Differs,
}

/// How facts are compared, by fact name and type.
#[derive(Default)]
pub struct Probes {
    /// The comparisons, each made for one fact: a name can have several, one per type that carries it.
    facts: Vec<Entry>,
    /// The probes read from the registry, for [`Probes::probe_problems`].
    registered: Vec<&'static FactProbe>,
}

impl Probes {
    /// The probes every linked crate registered with `register_fact_probe!`, beside its facts: no list is kept here.
    /// Values compare with `==`, or with the function the fact's own registration names (`HeadFact` and `FitFact`, which
    /// hold trait objects, compare their `Debug` text).
    pub fn registered() -> Self {
        Self::from_probes(fact::probes())
    }

    /// The probes `probes`, as [`Probes::registered`] reads them from the registry (a later probe for the same fact
    /// replaces an earlier one, so the last one given is the one [`check`] uses, and [`Probes::probe_problems`] still
    /// reports the doubled registration).
    pub fn from_probes(probes: impl IntoIterator<Item = &'static FactProbe>) -> Self {
        let mut all = Self::default();
        for probe in probes {
            all.put(Entry { name: probe.name, type_id: (probe.type_id)(), holds: probe.holds, differs: probe.differs });
            all.registered.push(probe);
        }
        all
    }

    /// Adds a comparison of fact `F` by value, for a fact registered without one (a test's own): the values a part
    /// publishes must be `==` under both palettes. `Debug` only words the failure.
    #[must_use]
    pub fn fact<F: Fact>(mut self) -> Self
    where
        F::Value: PartialEq + Debug,
    {
        self.put(Entry {
            name: F::NAME,
            type_id: TypeId::of::<F>(),
            holds: fact::holds_fact::<F>,
            differs: fact::differs_by_eq::<F>,
        });
        self
    }

    /// Keeps `entry` as the one comparison of its fact (same name and type): a later one replaces an earlier one, so
    /// [`check`] never reads a stale comparison.
    fn put(&mut self, entry: Entry) {
        match self.facts.iter_mut().find(|e| e.name == entry.name && e.type_id == entry.type_id) {
            Some(existing) => *existing = entry,
            None => self.facts.push(entry),
        }
    }

    /// The registered facts that have no probe here, named: what the gate test keeps empty. A probe counts only when
    /// its name and its fact's type both match, so a probe made for another fact that shares the name covers nothing.
    ///
    /// # Errors
    /// Two registered facts share a name ([`omuzaki_char_api::facts`]).
    pub fn unprobed_facts(&self) -> Result<Vec<&'static str>, String> {
        let probed =
            |f: &fact::FactRegistration| self.facts.iter().any(|e| e.name == f.name && e.type_id == (f.type_id)());
        Ok(fact::facts()?.into_iter().filter(|f| !probed(f)).map(|f| f.name).collect())
    }

    /// What is wrong with the registered probes themselves, one line each, for the gate test to keep empty: a probe
    /// that matches no registered fact (by name and type), and a second probe for one fact (only one would be used).
    ///
    /// # Errors
    /// Two registered facts share a name ([`omuzaki_char_api::facts`]).
    pub fn probe_problems(&self) -> Result<Vec<String>, String> {
        let facts = fact::facts()?;
        // Sorted by fact name and type name where built, so the order never follows the registry's.
        let mut problems: Vec<(&str, &str, String)> = Vec::new();
        let mut seen: BTreeSet<(&str, TypeId)> = BTreeSet::new();
        for probe in &self.registered {
            let id = (probe.type_id)();
            if !facts.iter().any(|f| f.name == probe.name && (f.type_id)() == id) {
                let line = format!("probe '{}' matches no registered fact of that name and type", probe.name);
                problems.push((probe.name, (probe.type_name)(), line));
            } else if !seen.insert((probe.name, id)) {
                problems.push((
                    probe.name,
                    (probe.type_name)(),
                    format!("fact '{}' has more than one probe", probe.name),
                ));
            }
        }
        problems.sort();
        Ok(problems.into_iter().map(|(_, _, line)| line).collect())
    }
}

/// What [`check`] found.
#[derive(Debug, Default)]
pub struct Report {
    /// One line per kind and fact that depends on the palette (or whose publishing differs in count).
    pub violations: Vec<String>,
    /// `(kind, fact)` for facts a kind publishes that no probe compares.
    pub unprobed: BTreeSet<(String, String)>,
    /// `(kind, why)` for kinds that don't build with the parameters the check could give them.
    pub unreached: Vec<(String, String)>,
}

/// `palette` with every slot changed: each channel inverted, so no colour equals its old self.
pub fn flipped(palette: &Palette) -> Palette {
    let flip = |c: Colour| Colour::rgba(255 - c.r, 255 - c.g, 255 - c.b, if c.a == 255 { 254 } else { 255 });
    Palette(palette.0.iter().map(|(slot, c)| (slot.clone(), flip(*c))).collect())
}

/// A colour that differs from `c` in every channel.
fn other(c: Colour, rng: &mut Rng) -> Colour {
    let mut pick = |old: u8| {
        let new = rng.next_u64().to_le_bytes()[0];
        if new == old { new.wrapping_add(128) } else { new }
    };
    Colour::rgba(pick(c.r), pick(c.g), pick(c.b), pick(c.a))
}

/// A name that `taken` doesn't hold: `base`, then `base{sep}2`, `base{sep}3`...
fn unused_name(base: &str, sep: char, taken: impl Fn(&str) -> bool) -> String {
    let mut name = base.to_owned();
    let mut n = 2_u32;
    while taken(&name) {
        name = format!("{base}{sep}{n}");
        n += 1;
    }
    name
}

/// The palettes [`check`] compares `palette` against: [`flipped`], then generated ones, each differing from `palette`
/// in every channel of every entry (the [`PALETTES`] colour alternatives). Then the slot-set ones, all with the colours
/// of `palette`, so a fact that depends on the slot set is caught: one with an extra slot no part references; one
/// without the slots no part names (when `palette` has any); and one per slot a part names, without only that slot.
/// Only the palettes that remove slots may fail to build (see `check_seed`). The same for the same input.
pub fn alternatives(palette: &Palette, referenced: &BTreeSet<String>) -> Vec<Palette> {
    let mut rng = Rng::new(SEED);
    let mut all = vec![flipped(palette)];
    while all.len() < PALETTES {
        all.push(Palette(palette.0.iter().map(|(slot, c)| (slot.clone(), other(*c, &mut rng))).collect()));
    }
    // A name a part references is no free name: the slot would enter that part's cache key, and a build error on it
    // would prove nothing about the cache.
    let free = unused_name(EXTRA_SLOT, '_', |n| referenced.contains(n) || palette.0.keys().any(|s| s.as_str() == n));
    if let Ok(slot) = PaletteSlot::new(&free) {
        let mut extra = palette.clone();
        extra.0.insert(slot, Colour::rgb(1, 2, 3));
        all.push(extra);
    }
    let mut fewer = palette.clone();
    fewer.0.retain(|slot, _| referenced.contains(slot.as_str()));
    if fewer != *palette {
        all.push(fewer);
    }
    for slot in palette.0.keys().filter(|slot| referenced.contains(slot.as_str())) {
        let mut without = palette.clone();
        without.0.remove(slot);
        all.push(without);
    }
    all
}

/// The palette slots the parts of `def` name (`@slot`), once their parameters are normalised (so defaults count).
fn referenced_slots(def: &CharacterDef) -> BTreeSet<String> {
    let params: Vec<Value> = def
        .parts
        .values()
        .map(|part| {
            let normalised = find_kind(part.kind.as_str()).and_then(|r| (r.check)(&part.params).ok());
            normalised.unwrap_or_else(|| part.params.clone())
        })
        .collect();
    let mut named = BTreeSet::new();
    for p in &params {
        named_slots(p, &mut named);
    }
    named.into_iter().map(|s| s.trim_start_matches('@').to_owned()).collect()
}

/// The kind of `part` when it is one of `run_kinds`, the kinds a run checks.
fn run_kind<'a>(def: &'a CharacterDef, part: &PartId, run_kinds: &[String]) -> Option<&'a str> {
    def.parts.get(part).map(|p| p.kind.as_str()).filter(|k| run_kinds.iter().any(|w| w == k))
}

/// The names of the facts `publishes` holds (its `Debug` is a map from fact name to value count).
fn fact_names(publishes: &Publishes) -> Vec<String> {
    let text = format!("{publishes:?}");
    text.trim_matches(|c| c == '{' || c == '}')
        .split(", ")
        .filter_map(|entry| entry.split_once("\": ").map(|(name, _)| name.trim_start_matches('"').to_owned()))
        .collect()
}

/// What every part of a character published, in build order.
type PartsPublished = [(PartId, Publishes)];

/// A build that failed: the part it failed on (none when the plan itself fails), and why.
type BuildFailure = (Option<PartId>, String);

/// What every part of `def` published, in build order, built with `quality` and `rig_export`.
fn publishes_of(
    def: &CharacterDef,
    quality: Quality,
    rig_export: bool,
    services: &BuildServices,
) -> Result<Vec<(PartId, Publishes)>, BuildFailure> {
    let options = BuildOptions { quality, rig_export, ..BuildOptions::default() };
    let mut published = Published::default();
    let mut all = Vec::new();
    for part in plan(def).map_err(|e| (None, e.to_string()))? {
        let output = build_part(def, &part, &options, services, &published)
            .map_err(|e| (Some(part.clone()), format!("{part}: {e}")))?;
        published.absorb(&part, output.publishes.clone()).map_err(|e| (Some(part.clone()), e.to_string()))?;
        all.push((part, output.publishes));
    }
    Ok(all)
}

/// The seeds a base at `seed` is checked at: `seed`, then each one [`SEED_STEP`] on from the last.
fn seeds_of(seed: u32, seeds: usize) -> impl Iterator<Item = u32> {
    std::iter::successors(Some(seed), |last| Some(last.wrapping_add(SEED_STEP))).take(seeds.max(1))
}

/// The kinds a check is asked for: `kinds`, or every registered kind.
fn wanted_kinds(kinds: Option<&[&str]>) -> Vec<String> {
    match kinds {
        Some(kinds) => kinds.iter().map(|k| (*k).to_owned()).collect(),
        // Sorted: `inventory` yields kinds in link order, which differs between targets, and the report follows this order.
        None => {
            omuzaki_char_api::kinds().map(|r| (r.meta)().id.to_owned()).collect::<BTreeSet<_>>().into_iter().collect()
        }
    }
}

/// Checks the parts of `kinds` (every registered kind when `None`) against `base`, a character that builds.
///
/// A kind `base` has an active part of (one the build plan reaches) is checked through that part; any other kind through one extra part with its default
/// parameters (`base` supplies what it requires). Every build gets `services` (the assets and rasteriser a kind needs; a kind
/// that needs one that isn't there is `unreached`). A kind that can't be built that way is listed in
/// [`Report::unreached`], not skipped silently.
pub fn check(base: &CharacterDef, kinds: Option<&[&str]>, probes: &Probes, services: &BuildServices) -> Report {
    check_with(base, kinds, probes, services, Sweep::FULL)
}

/// [`check`] over `sweep`.
pub fn check_with(
    base: &CharacterDef,
    kinds: Option<&[&str]>,
    probes: &Probes,
    services: &BuildServices,
    sweep: Sweep,
) -> Report {
    let wanted = wanted_kinds(kinds);
    let mut report = Report::default();
    // The kinds with an active part: the ones the build plan reaches (a disabled part, or one under a disabled ancestor,
    // is never built, so it covers nothing). A base that can't be planned covers nothing.
    let planned = plan(base).unwrap_or_default();
    let in_base: BTreeSet<&str> =
        planned.iter().filter_map(|part| base.parts.get(part)).map(|p| p.kind.as_str()).collect();
    // One run over `base` covers every kind it has a part of; each other kind gets a run of its own.
    let mut runs: Vec<(CharacterDef, Vec<String>)> = Vec::new();
    let base_kinds: Vec<String> = wanted.iter().filter(|k| in_base.contains(k.as_str())).cloned().collect();
    if !base_kinds.is_empty() {
        runs.push((base.clone(), base_kinds));
    }
    for kind in wanted.iter().filter(|k| !in_base.contains(k.as_str())) {
        let Some(registration) = find_kind(kind) else {
            report.unreached.push((kind.clone(), "not registered".to_owned()));
            continue;
        };
        let probe = unused_name(PROBE, '-', |n| base.parts.keys().any(|id| id.as_str() == n));
        let (Ok(kind_id), Ok(part_id)) = (PartKindId::new(kind.as_str()), PartId::new(&probe)) else {
            report.unreached.push((kind.clone(), "not a valid kind id".to_owned()));
            continue;
        };
        let mut def = base.clone();
        let part = PartDef {
            kind: kind_id,
            parent: None,
            enabled: true,
            layer: 1000,
            attach: None,
            params: (registration.defaults)(),
        };
        def.parts.insert(part_id, part);
        runs.push((def, vec![kind.clone()]));
    }
    // The runs are independent builds, so they go on separate threads; the reports are merged in run order, so the
    // result doesn't depend on which finishes first.
    let reports: Vec<Report> = thread::scope(|scope| {
        let handles: Vec<_> = runs
            .iter()
            .map(|(def, run_kinds)| scope.spawn(move || check_run(def, run_kinds, probes, services, sweep)))
            .collect();
        handles
            .into_iter()
            .zip(&runs)
            .map(|(handle, (_, run_kinds))| {
                handle.join().unwrap_or_else(|_| Report {
                    unreached: run_kinds.iter().map(|k| (k.clone(), "the check panicked".to_owned())).collect(),
                    ..Report::default()
                })
            })
            .collect()
    });
    for run in reports {
        report.violations.extend(run.violations);
        report.unprobed.extend(run.unprobed);
        report.unreached.extend(run.unreached);
    }
    report
}

/// Builds `def` at each of `sweep`'s seeds under each of its option sets and alternative palette and reports what `run_kinds`
/// published differently. A base that doesn't build stops the run: its kinds are unreached, not clean.
///
/// The base seed gets every alternative. The other seeds get only the first colour alternative: a slot-set dependence
/// does not depend on the seed, so the rest of the slot-set palettes need one seed only (the cost is in the module doc).
fn check_run(
    def: &CharacterDef,
    run_kinds: &[String],
    probes: &Probes,
    services: &BuildServices,
    sweep: Sweep,
) -> Report {
    let mut report = Report::default();
    let alternatives = alternatives(&def.palette, &referenced_slots(def));
    for (i, seed) in seeds_of(def.seed, sweep.seeds).enumerate() {
        let mut seeded = def.clone();
        seeded.seed = seed;
        let these = if i == 0 { &alternatives[..] } else { &alternatives[..alternatives.len().min(1)] };
        if let Err(why) = check_seed(&seeded, run_kinds, these, (probes, services, sweep.options), &mut report) {
            report.unreached.extend(run_kinds.iter().map(|k| (k.clone(), why.clone())));
            break;
        }
    }
    report
}

/// Adds to `report` what `run_kinds` published differently between `def`'s own palette and each of `alternatives`, under
/// each of `options` (the slot-set alternatives under the first only), at `def`'s seed. `Err` says why `def` itself doesn't build under some option set.
fn check_seed(
    def: &CharacterDef,
    run_kinds: &[String],
    alternatives: &[Palette],
    (probes, services, options): (&Probes, &BuildServices, &[(Quality, bool)]),
    report: &mut Report,
) -> Result<(), String> {
    for (k, &(quality, rig_export)) in options.iter().enumerate() {
        // The slot-set alternatives (after the colour ones) under the first option set only: whether a part reads the
        // palette's slot set does not hang on the options any more than on the seed.
        let alternatives = if k == 0 { alternatives } else { &alternatives[..alternatives.len().min(PALETTES)] };
        let options = format!("{quality:?} quality, rig_export {rig_export}, seed {}", def.seed);
        let under_own = publishes_of(def, quality, rig_export, services).map_err(|(_, e)| {
            format!("does not build ({quality:?}, rig_export {rig_export}, seed {}): {e}", def.seed)
        })?;
        for (n, palette) in alternatives.iter().enumerate() {
            let mut changed = def.clone();
            changed.palette = palette.clone();
            match publishes_of(&changed, quality, rig_export, services) {
                Ok(under_other) => {
                    compare(def, run_kinds, (&under_own, &under_other), (n, &options), probes, report);
                }
                // A palette with fewer slots may lack one a part needs, so its build can fail where a fact would be
                // read. That is the known gap of a removal alternative (no fact is compared at it); every colour
                // alternative still compares the rest, since it keeps every slot.
                Err(_) if palette.0.len() < def.palette.0.len() => {}
                // An extra slot is in no part's cache key, so a part that fails when it appears would be served its
                // old output. A part of a checked kind is a violation; a part of another kind fails the run instead,
                // so this run's kinds are listed unreached rather than passing with nothing compared.
                Err((Some(part), e)) if palette.0.len() > def.palette.0.len() => {
                    let why = format!(
                        "part '{part}' does not build under alternative {n}, which adds a slot ({options}): {e}"
                    );
                    let Some(kind) = run_kind(def, &part, run_kinds) else {
                        return Err(why);
                    };
                    let prefix = format!("kind '{kind}': part '{part}' does not build");
                    if !report.violations.iter().any(|v| v.starts_with(&prefix)) {
                        report.violations.push(format!("kind '{kind}': {why}"));
                    }
                }
                Err((_, e)) => return Err(format!("does not build ({options}): {e}")),
            }
        }
    }
    Ok(())
}

/// [`check`] against several bases (the kit characters that have the parts different kinds need): a kind counts as reached
/// when any base reaches it. With no bases nothing can be built, so every wanted kind (`kinds`, or every registered kind)
/// is listed in [`Report::unreached`] rather than the report coming back clean.
pub fn check_all(bases: &[CharacterDef], kinds: Option<&[&str]>, probes: &Probes, services: &BuildServices) -> Report {
    check_all_with(bases, kinds, probes, services, Sweep::FULL)
}

/// [`check_all`] over `sweep`.
pub fn check_all_with(
    bases: &[CharacterDef],
    kinds: Option<&[&str]>,
    probes: &Probes,
    services: &BuildServices,
    sweep: Sweep,
) -> Report {
    let mut merged = Report::default();
    if bases.is_empty() {
        merged.unreached =
            wanted_kinds(kinds).into_iter().map(|k| (k, "no base character to build it in".to_owned())).collect();
        return merged;
    }
    let mut unreached: Option<BTreeMap<String, String>> = None;
    for base in bases {
        let report = check_with(base, kinds, probes, services, sweep);
        merged.violations.extend(report.violations);
        merged.unprobed.extend(report.unprobed);
        let these: BTreeMap<_, _> = report.unreached.into_iter().collect();
        unreached = Some(match unreached {
            None => these,
            Some(before) => before.into_iter().filter(|(kind, _)| these.contains_key(kind)).collect(),
        });
    }
    merged.unreached = unreached.unwrap_or_default().into_iter().collect();
    merged.violations.sort();
    merged.violations.dedup();
    merged
}

/// Adds to `report` what differs between the parts of `kinds` built under the character's palette and alternative
/// number `n`. Only the first palette that shows a difference is reported.
fn compare(
    def: &CharacterDef,
    kinds: &[String],
    (a, b): (&PartsPublished, &PartsPublished),
    (n, options): (usize, &str),
    probes: &Probes,
    report: &mut Report,
) {
    for ((part, under_a), (_, under_b)) in a.iter().zip(b) {
        // A part of another kind is another run's to check.
        let Some(kind) = run_kind(def, part, kinds) else {
            continue;
        };
        // Reported once per part and fact, however many palettes show it.
        let mut violate = |scope: String, message: String| {
            let line = format!("kind '{kind}' (part '{part}'){scope}: {message}");
            if !report.violations.iter().any(|v| v.starts_with(&format!("kind '{kind}' (part '{part}'){scope}:"))) {
                report.violations.push(line);
            }
        };
        if format!("{under_a:?}") != format!("{under_b:?}") {
            violate(
                String::new(),
                format!(
                    "publishes {under_a:?} under the character's palette and {under_b:?} under alternative {n} ({options})"
                ),
            );
            // The violation names the part; its facts are not compared on top of it.
            continue;
        }
        for name in fact_names(under_a) {
            // The probe for this name whose fact is the one published: a probe made for another fact that shares the name
            // would see no values and pass, so it is not used.
            let probe = probes.facts.iter().find(|e| e.name == name && (e.holds)(under_a) && (e.holds)(under_b));
            match probe {
                Some(Entry { differs, .. }) => {
                    if let Some(what) = differs(under_a, under_b) {
                        violate(
                            format!(", fact '{name}'"),
                            format!("fact '{name}' depends on the palette (alternative {n}, {options}): {what}"),
                        );
                    }
                }
                None => {
                    report.unprobed.insert((kind.to_owned(), name));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------- colours only through the parameters

/// The first palette contract (`PartKind::build`, contract 1): a part reads palette colours only through the slots its
/// own parameters name. For each part of `base` under each of the [`OPTIONS`], the part is built again against what
/// the parts before it published, with every slot its normalised parameters don't name [`flipped`] and one extra slot
/// added; its meshes, materials, material classes, textures and warnings must come out the same. A kind that looked a
/// fixed slot up behind its parameters' back changes there, and the caching builder, which keys a part on the slots
/// its parameters name, would serve it stale.
///
/// Returns one line per part and option set whose output changed, naming the part, its kind and what differed.
///
/// # Errors
/// The first build of `base` that fails, with the part and the reason: a base that doesn't build checks nothing.
pub fn check_colours_through_parameters(base: &CharacterDef, services: &BuildServices) -> Result<Vec<String>, String> {
    let order = plan(base).map_err(|e| e.to_string())?;
    let mut violations = Vec::new();
    for (quality, rig_export) in OPTIONS {
        let options = BuildOptions { quality, rig_export, ..BuildOptions::default() };
        let mut published = Published::default();
        for part in &order {
            let output = build_part(base, part, &options, services, &published).map_err(|e| format!("{part}: {e}"))?;
            let other = unnamed_flipped(base, part);
            let again = build_part(&other, part, &options, services, &published)
                .map_err(|e| format!("{part} with its unnamed slots changed: {e}"))?;
            let differs: Vec<&str> = [
                ("meshes", output.meshes != again.meshes),
                ("materials", output.materials != again.materials),
                ("material classes", output.material_classes != again.material_classes),
                ("textures", output.textures != again.textures),
                ("warnings", output.warnings != again.warnings),
            ]
            .into_iter()
            .filter_map(|(what, differs)| differs.then_some(what))
            .collect();
            if !differs.is_empty() {
                let kind = base.parts.get(part).map_or("?", |p| p.kind.as_str());
                violations.push(format!(
                    "part `{part}` (kind `{kind}`, {quality:?}, rig_export {rig_export}): its {} changed when only \
                     palette slots its parameters don't name changed",
                    differs.join(", ")
                ));
            }
            published.absorb(part, output.publishes).map_err(|e| format!("{part}: {e}"))?;
        }
    }
    Ok(violations)
}

/// `def` with every palette slot that `part`'s normalised parameters don't name changed in every channel, and one more
/// slot no part names.
fn unnamed_flipped(def: &CharacterDef, part: &PartId) -> CharacterDef {
    let mut named = BTreeSet::new();
    let params = def.parts.get(part).map(|p| {
        find_kind(p.kind.as_str()).and_then(|r| (r.check)(&p.params).ok()).unwrap_or_else(|| p.params.clone())
    });
    if let Some(params) = &params {
        named_slots(params, &mut named);
    }
    let named: BTreeSet<&str> = named.into_iter().map(|s| s.trim_start_matches('@')).collect();
    let flip = flipped(&def.palette);
    let mut other = def.clone();
    for (slot, colour) in &mut other.palette.0 {
        if !named.contains(slot.as_str())
            && let Some(changed) = flip.0.get(slot)
        {
            *colour = *changed;
        }
    }
    let free = unused_name(EXTRA_SLOT, '_', |n| named.contains(n) || other.palette.0.keys().any(|s| s.as_str() == n));
    if let Ok(slot) = PaletteSlot::new(&free) {
        other.palette.0.insert(slot, Colour::rgb(1, 2, 3));
    }
    other
}

// ------------------------------------------------------------------------------------------------------- recolouring

/// What [`check_recolour`] found.
#[derive(Debug, Default)]
pub struct RecolourReport {
    /// One line per part and option set whose recoloured output differs from a fresh build under the new palette.
    pub violations: Vec<String>,
    /// The kinds that recoloured (answered [`Recoloured::Done`](omuzaki_char_api::Recoloured::Done)) at least once.
    pub recoloured: BTreeSet<String>,
    /// The kinds that declined, and so are rebuilt on a palette change.
    pub declined: BTreeSet<String>,
}

/// The third contract (`PartKind::recolour`): recolouring a part's output to a new palette gives exactly what a build
/// under that palette gives. For each part of `base` under each of the [`OPTIONS`], the part is built under the
/// character's palette and recoloured to the [`flipped`] one, then built under the flipped one; a kind that answers
/// `Done` must match the fresh build in meshes, materials, material classes, textures and warnings.
///
/// # Errors
/// The first build of `base` (under either palette) that fails, with the part and the reason.
pub fn check_recolour(base: &CharacterDef, services: &BuildServices) -> Result<RecolourReport, String> {
    let order = plan(base).map_err(|e| e.to_string())?;
    let mut other = base.clone();
    other.palette = flipped(&base.palette);
    let mut report = RecolourReport::default();
    for (quality, rig_export) in OPTIONS {
        let options = BuildOptions { quality, rig_export, ..BuildOptions::default() };
        let mut published = Published::default();
        for part in &order {
            let kind = base.parts.get(part).map_or_else(String::new, |p| p.kind.to_string());
            let mut output =
                build_part(base, part, &options, services, &published).map_err(|e| format!("{part}: {e}"))?;
            let fresh = build_part(&other, part, &options, services, &published)
                .map_err(|e| format!("{part} under the flipped palette: {e}"))?;
            let publishes = output.publishes.clone();
            match omuzaki_char_api::recolour_part(&other, part, services, &mut output) {
                Ok(omuzaki_char_api::Recoloured::Done) => {
                    report.recoloured.insert(kind.clone());
                    let differs: Vec<&str> = [
                        ("meshes", output.meshes != fresh.meshes),
                        ("materials", output.materials != fresh.materials),
                        ("material classes", output.material_classes != fresh.material_classes),
                        ("textures", output.textures != fresh.textures),
                        ("warnings", output.warnings != fresh.warnings),
                        ("runtime", output.runtime.is_some() != fresh.runtime.is_some()),
                    ]
                    .into_iter()
                    .filter_map(|(what, differs)| differs.then_some(what))
                    .collect();
                    if !differs.is_empty() {
                        report.violations.push(format!(
                            "part `{part}` (kind `{kind}`, {quality:?}, rig_export {rig_export}): recoloured, its {} \
                             differ from a build under the new palette",
                            differs.join(", ")
                        ));
                    }
                }
                Ok(_) => {
                    report.declined.insert(kind.clone());
                }
                Err(e) => report.violations.push(format!(
                    "part `{part}` (kind `{kind}`, {quality:?}, rig_export {rig_export}): recolour failed: {e}"
                )),
            }
            published.absorb(part, publishes).map_err(|e| format!("{part}: {e}"))?;
        }
    }
    report.declined.retain(|kind| !report.recoloured.contains(kind));
    Ok(report)
}

// ------------------------------------------------------------------------------------------------- an App's gate test

/// The characters [`check_linked`] builds every kind in: the kit's default character (body, head, hair, shirt, skirt,
/// legwear and boots, on the `soft-sd` template), and the same with a collar and a tie on the shirt's collar ring, for
/// the kinds that hang on a shirt. Real kinds only, so an App that links no mock checks what it ships.
///
/// # Errors
/// The default character doesn't read (a broken built-in starter or template).
pub fn kit_bases() -> Result<Vec<CharacterDef>, String> {
    let template = omuzaki_char_api::BodyTemplateId::new("soft-sd").map_err(|e| e.to_string())?;
    let default = crate::ops::character::create::default_character(&template).map_err(|e| e.to_string())?;
    let mut dressed = default.clone();
    let on_collar: omuzaki_char_api::Attach =
        serde_json::from_value(serde_json::json!({ "to": "anchor", "part": "shirt", "anchor": "collar_ring" }))
            .map_err(|e| e.to_string())?;
    for (id, layer) in
        [("collar", crate::outfit::common::LAYER_SHIRT + 1), ("tie", crate::outfit::common::LAYER_SHIRT + 2)]
    {
        let part = PartDef {
            kind: PartKindId::new(id).map_err(|e| e.to_string())?,
            parent: None,
            enabled: true,
            layer,
            attach: Some(on_collar.clone()),
            params: Value::Object(serde_json::Map::new()),
        };
        dressed.parts.insert(PartId::new(id).map_err(|e| e.to_string())?, part);
    }
    Ok(vec![default, dressed])
}

/// The palette-independence check over every part kind linked into the running binary (#1635), against
/// [`kit_bases`], with the registered probes: what each App's gate test runs beside its link gate (ADR 0015), so a kind
/// from any crate whose published facts depend on the palette fails that App's test, named with its fact. The two
/// palette contracts of the build ([`check_colours_through_parameters`]) and the recolour contract
/// ([`check_recolour`]) are checked over the same characters; their findings join [`Report::violations`].
///
/// # Errors
/// The bases don't read or don't build.
pub fn check_linked(services: &BuildServices, sweep: Sweep) -> Result<Report, String> {
    let bases = kit_bases()?;
    let mut report = check_all_with(&bases, None, &Probes::registered(), services, sweep);
    for base in &bases {
        report.violations.extend(check_colours_through_parameters(base, services)?);
        report.violations.extend(check_recolour(base, services)?.violations);
    }
    Ok(report)
}
