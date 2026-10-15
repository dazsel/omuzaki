//! The palette-independence conformance check: the rule that makes the caching builder's key sound.
//!
//! The caching builder keys a part that reads facts on the *geometry* keys of the parts that published them, never on
//! their colours (decision Q6: a palette change never rebuilds geometry). That is exact only while no fact depends on the
//! palette. `Fact` values are type-erased and have no equality, so the rule is enforced here instead: [`check`] builds a
//! part of each kind under the character's own palette and [`PALETTES`] others (each differing from it in *every*
//! entry, so a kind that only reacts past a threshold is still caught) and compares what it published, fact by fact.
//! Values are compared with `==`; only [`HeadFact`] and [`FitFact`], which hold trait objects with no equality, are
//! compared through their `Debug` text ([`Probes::fact_by_debug`]). A kind that publishes something different under
//! another palette is a [`Report::violations`] entry naming the kind, the part and the fact.
//!
//! The palettes also differ in their slot set (one with an extra slot, one without the slots no part names, and one
//! without each slot a part names), and the whole comparison runs under each of the [`OPTIONS`] (both qualities, with
//! and without `rig_export`), because a fact may depend on either; a violation names the options and the seed it showed
//! under. The seeds ([`SEEDS`]) are swept with the base's palette against the first colour alternative only, so a fact
//! that depends on the seed and on a slot set (or on a colour that is not the first alternative) is not caught at the
//! other seeds: the known gap, accepted for the build cost.
//!
//! Facts are compared by [`Probes`]: the built-in ones are registered, and a crate that adds a fact adds
//! [`Probes::fact`] for it in the test that runs the check. A fact a kind publishes that no probe covers is listed in
//! [`Report::unprobed`] instead of passing unseen. Any crate can run the check over its own kinds and characters.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::thread;

use omuzaki_char_api::{
    AnchorsFact, BuildOptions, BuildServices, CharacterDef, CollidersFact, FaceFact, Fact, FitFact, HeadFact, PartDef,
    PartId, PartKindId, Published, Publishes, Quality, StatureFact, build_part, find_kind, plan,
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
/// The seed of the generated palettes: fixed, so a failure reproduces.
const SEED: u64 = 0x1598_fac7;

/// The id of the part the check adds for a kind the character has no part of (a suffix is added if `base` has it).
const PROBE: &str = "palette-probe";
/// The slot of the alternative that has one more entry than the palette (a suffix is added if the palette has it).
const EXTRA_SLOT: &str = "unused_probe_slot";

/// How one fact of two builds is compared: `None` when equal, else a description of both for the failure message.
type Compare = fn(&Publishes, &Publishes) -> Option<String>;

/// How facts are compared, by fact name.
#[derive(Default)]
pub struct Probes {
    facts: BTreeMap<&'static str, Compare>,
}

impl Probes {
    /// The facts of `omuzaki-char-api`. [`HeadFact`] and [`FitFact`] hold trait objects with no equality, so they are
    /// compared by their `Debug` text ([`Probes::fact_by_debug`]); the others by value.
    pub fn built_in() -> Self {
        Self::default()
            .fact_by_debug::<HeadFact>()
            .fact_by_debug::<FitFact>()
            .fact::<FaceFact>()
            .fact::<AnchorsFact>()
            .fact::<CollidersFact>()
            .fact::<StatureFact>()
    }

    /// Compares fact `F` by value: the values a part publishes must be `==` under both palettes. `Debug` only words the
    /// failure.
    #[must_use]
    pub fn fact<F: Fact>(mut self) -> Self
    where
        F::Value: PartialEq + Debug,
    {
        self.facts.insert(F::NAME, |a, b| {
            let (a, b): (Vec<&F::Value>, Vec<&F::Value>) = (a.values::<F>().collect(), b.values::<F>().collect());
            (a != b).then(|| format!("{a:?} under one palette, {b:?} under another"))
        });
        self
    }

    /// Compares fact `F` by the `Debug` text of its values, for a value with no equality (a trait object). A value
    /// whose `Debug` hides data is under-checked, so prefer [`Probes::fact`].
    #[must_use]
    pub fn fact_by_debug<F: Fact>(mut self) -> Self
    where
        F::Value: Debug,
    {
        self.facts.insert(F::NAME, |a, b| {
            let text = |p: &Publishes| p.values::<F>().map(|v| format!("{v:?}")).collect::<Vec<_>>();
            let (a, b) = (text(a), text(b));
            (a != b).then(|| format!("{a:?} under one palette, {b:?} under another"))
        });
        self
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
/// Only the palettes that remove slots may fail to build (see [`check_seed`]). The same for the same input.
pub fn alternatives(palette: &Palette, referenced: &BTreeSet<String>) -> Vec<Palette> {
    let mut rng = Rng::new(SEED);
    let mut all = vec![flipped(palette)];
    while all.len() < PALETTES {
        all.push(Palette(palette.0.iter().map(|(slot, c)| (slot.clone(), other(*c, &mut rng))).collect()));
    }
    let free = unused_name(EXTRA_SLOT, '_', |n| palette.0.keys().any(|s| s.as_str() == n));
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
fn seeds_of(seed: u32) -> impl Iterator<Item = u32> {
    std::iter::successors(Some(seed), |last| Some(last.wrapping_add(SEED_STEP))).take(SEEDS)
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
            .map(|(def, run_kinds)| scope.spawn(move || check_run(def, run_kinds, probes, services)))
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

/// Builds `def` at each of its [`SEEDS`] under every option set and alternative palette and reports what `run_kinds`
/// published differently. A base that doesn't build stops the run: its kinds are unreached, not clean.
///
/// The base seed gets every alternative. The other seeds get only the first colour alternative: a slot-set dependence
/// does not depend on the seed, so the rest of the slot-set palettes need one seed only (the cost is in the module doc).
fn check_run(def: &CharacterDef, run_kinds: &[String], probes: &Probes, services: &BuildServices) -> Report {
    let mut report = Report::default();
    let alternatives = alternatives(&def.palette, &referenced_slots(def));
    for (i, seed) in seeds_of(def.seed).enumerate() {
        let mut seeded = def.clone();
        seeded.seed = seed;
        let these = if i == 0 { &alternatives[..] } else { &alternatives[..alternatives.len().min(1)] };
        if let Err(why) = check_seed(&seeded, run_kinds, these, probes, services, &mut report) {
            report.unreached.extend(run_kinds.iter().map(|k| (k.clone(), why.clone())));
            break;
        }
    }
    report
}

/// Adds to `report` what `run_kinds` published differently between `def`'s own palette and each of `alternatives`, under
/// every option set, at `def`'s seed. `Err` says why `def` itself doesn't build under some option set.
fn check_seed(
    def: &CharacterDef,
    run_kinds: &[String],
    alternatives: &[Palette],
    probes: &Probes,
    services: &BuildServices,
    report: &mut Report,
) -> Result<(), String> {
    for (quality, rig_export) in OPTIONS {
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
    let mut merged = Report::default();
    if bases.is_empty() {
        merged.unreached =
            wanted_kinds(kinds).into_iter().map(|k| (k, "no base character to build it in".to_owned())).collect();
        return merged;
    }
    let mut unreached: Option<BTreeMap<String, String>> = None;
    for base in bases {
        let report = check(base, kinds, probes, services);
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
            match probes.facts.get(name.as_str()) {
                Some(differs) => {
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
