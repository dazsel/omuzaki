//! The palette-independence conformance check: the conformance rule that makes the caching builder's key sound.
//!
//! The caching builder keys a part that reads facts on the *geometry* keys of the parts that published them, never on
//! their colours (decision Q6: a palette change never rebuilds geometry). That is exact only while no fact depends on the
//! palette. `Fact` values are type-erased and have no equality, so the rule is enforced here instead: [`check`] builds a
//! part of each kind under [`PALETTES`] palettes (the character's own and several others, each differing from it in
//! *every* entry, so a kind that only reacts past a threshold is still caught) and compares what it published, fact by
//! fact, through the `Debug` text of each value (which prints floats exactly). A kind that publishes something
//! different under another palette is a [`Report::violations`] entry naming the kind, the part and the fact.
//!
//! Facts are compared by [`Probes`]: the built-in ones are registered, and a crate that adds a fact adds
//! [`Probes::fact`] for it in the test that runs the check. A fact a kind publishes that no probe covers is listed in
//! [`Report::unprobed`] instead of passing unseen. Any crate can run the check over its own kinds and characters.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use omuzaki_char_api::{
    AnchorsFact, BuildOptions, BuildServices, CharacterDef, CollidersFact, FaceFact, Fact, FitFact, HeadFact, PartDef,
    PartId, PartKindId, Published, Publishes, StatureFact, build_part, find_kind, plan,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::Palette;

/// How many palettes besides the character's own each kind is built under: the inverted one and generated ones.
pub const PALETTES: usize = 5;
/// The seed of the generated palettes: fixed, so a failure reproduces.
const SEED: u64 = 0x1598_fac7;

/// The part the check adds for a kind the character has no part of.
const PROBE: &str = "palette-probe";

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

/// The palettes [`check`] compares `palette` against: [`flipped`], then generated ones, each differing from `palette`
/// in every channel of every entry. The same for the same input.
pub fn alternatives(palette: &Palette) -> Vec<Palette> {
    let mut rng = Rng::new(SEED);
    let mut all = vec![flipped(palette)];
    while all.len() < PALETTES {
        all.push(Palette(palette.0.iter().map(|(slot, c)| (slot.clone(), other(*c, &mut rng))).collect()));
    }
    all
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

/// What every part of `def` published, in build order.
fn publishes_of(def: &CharacterDef) -> Result<Vec<(PartId, Publishes)>, String> {
    let (options, services) = (BuildOptions::default(), BuildServices::default());
    let mut published = Published::default();
    let mut all = Vec::new();
    for part in plan(def).map_err(|e| e.to_string())? {
        let output = build_part(def, &part, &options, &services, &published).map_err(|e| format!("{part}: {e}"))?;
        published.absorb(&part, output.publishes.clone()).map_err(|e| e.to_string())?;
        all.push((part, output.publishes));
    }
    Ok(all)
}

/// Checks the parts of `kinds` (every registered kind when `None`) against `base`, a character that builds.
///
/// A kind `base` has an active part of (one the build plan reaches) is checked through that part; any other kind through one extra part with its default
/// parameters (`base` supplies what it requires). A kind that can't be built that way is listed in
/// [`Report::unreached`], not skipped silently.
pub fn check(base: &CharacterDef, kinds: Option<&[&str]>, probes: &Probes) -> Report {
    let wanted: Vec<String> = match kinds {
        Some(kinds) => kinds.iter().map(|k| (*k).to_owned()).collect(),
        None => omuzaki_char_api::kinds().map(|r| (r.meta)().id.to_owned()).collect(),
    };
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
        let (Ok(kind_id), Ok(part_id)) = (PartKindId::new(kind.as_str()), PartId::new(PROBE)) else {
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
    for (def, run_kinds) in runs {
        let under_own = match publishes_of(&def) {
            Ok(own) => own,
            Err(e) => {
                report.unreached.extend(run_kinds.into_iter().map(|k| (k, format!("does not build: {e}"))));
                continue;
            }
        };
        for (n, palette) in alternatives(&def.palette).into_iter().enumerate() {
            let mut changed = def.clone();
            changed.palette = palette;
            match publishes_of(&changed) {
                Ok(under_other) => compare(&def, &run_kinds, (&under_own, &under_other), n, probes, &mut report),
                Err(e) => {
                    // A palette the kind can't build under is not a fact that depends on it, but it is not checked either.
                    report.unreached.extend(run_kinds.iter().map(|k| (k.clone(), format!("does not build: {e}"))));
                    break;
                }
            }
        }
    }
    report
}

/// [`check`] against several bases (the kit characters that have the parts different kinds need): a kind counts as reached
/// when any base reaches it.
pub fn check_all(bases: &[CharacterDef], kinds: Option<&[&str]>, probes: &Probes) -> Report {
    let mut merged = Report::default();
    let mut unreached: Option<BTreeMap<String, String>> = None;
    for base in bases {
        let report = check(base, kinds, probes);
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
    n: usize,
    probes: &Probes,
    report: &mut Report,
) {
    for ((part, under_a), (_, under_b)) in a.iter().zip(b) {
        let Some(kind) = def.parts.get(part).map(|p| p.kind.as_str()).filter(|k| kinds.iter().any(|w| w == k)) else {
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
                format!("publishes {under_a:?} under the character's palette and {under_b:?} under alternative {n}"),
            );
            continue;
        }
        for name in fact_names(under_a) {
            match probes.facts.get(name.as_str()) {
                Some(differs) => {
                    if let Some(what) = differs(under_a, under_b) {
                        violate(
                            format!(", fact '{name}'"),
                            format!("fact '{name}' depends on the palette (alternative {n}): {what}"),
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
