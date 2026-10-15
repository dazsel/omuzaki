//! The palette-independence check: the conformance rule that makes the caching builder's key sound.
//!
//! The caching builder keys a part that reads facts on the *geometry* keys of the parts that published them, never on
//! their colours (decision Q6: a palette change never rebuilds geometry). That is exact only while no fact depends on the
//! palette. `Fact` values are type-erased and have no equality, so the rule is enforced here instead: [`check`] builds a
//! part of each kind under two palettes that differ in every slot and compares what it published, fact by fact, through
//! the `Debug` text of each value (which prints floats exactly). A kind that publishes something different under the
//! other palette is a [`Report::violations`] entry naming the kind, the part and the fact.
//!
//! Facts are compared by [`Probes`]: the built-in ones are registered, and a crate that adds a fact adds
//! [`Probes::fact`] for it in the test that runs the check. A fact a kind publishes that no probe covers is listed in
//! [`Report::unprobed`] (and the sweep test refuses it) instead of passing unseen.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
// Each test binary uses the part of the helper it needs.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use omuzaki_char_api::{
    AnchorsFact, BuildOptions, BuildServices, CharacterDef, CollidersFact, FaceFact, Fact, FitFact, HeadFact, PartDef,
    PartId, PartKindId, Published, Publishes, StatureFact, build_part, find_kind, plan,
};
use omuzaki_core::colour::Colour;
use omuzaki_face_api::Palette;

/// The part the check adds for a kind the character has no part of.
const PROBE: &str = "palette-probe";

/// How facts are compared: the `Debug` text of every value of a fact, by fact name.
#[derive(Default)]
pub struct Probes {
    /// By fact name: the values of the fact in a part's `Publishes`, as text.
    facts: BTreeMap<&'static str, fn(&Publishes) -> Vec<String>>,
}

impl Probes {
    /// The facts of `omuzaki-char-api`.
    pub fn built_in() -> Self {
        Self::default()
            .fact::<HeadFact>()
            .fact::<FaceFact>()
            .fact::<FitFact>()
            .fact::<AnchorsFact>()
            .fact::<CollidersFact>()
            .fact::<StatureFact>()
    }

    /// Compares fact `F` by the `Debug` text of its values.
    #[must_use]
    pub fn fact<F: Fact>(mut self) -> Self
    where
        F::Value: Debug,
    {
        self.facts.insert(F::NAME, |p| p.values::<F>().map(|v| format!("{v:?}")).collect());
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

impl Report {
    /// Panics with every violation, if there are any.
    pub fn assert_independent(&self) {
        assert!(self.violations.is_empty(), "facts depend on the palette:\n{}", self.violations.join("\n"));
    }
}

/// `palette` with every slot changed: each channel inverted, so no colour equals its old self.
pub fn flipped(palette: &Palette) -> Palette {
    let flip = |c: Colour| Colour::rgba(255 - c.r, 255 - c.g, 255 - c.b, if c.a == 255 { 254 } else { 255 });
    let other = Palette(palette.0.iter().map(|(slot, c)| (slot.clone(), flip(*c))).collect());
    assert!(palette.0.iter().all(|(slot, c)| other.0[slot] != *c), "every slot differs");
    other
}

/// The names of the facts `publishes` holds (its `Debug` is a map from fact name to value count).
fn fact_names(publishes: &Publishes) -> Vec<String> {
    let text = format!("{publishes:?}");
    text.trim_matches(|c| c == '{' || c == '}')
        .split(", ")
        .filter_map(|entry| entry.split_once("\": ").map(|(name, _)| name.trim_start_matches('"').to_owned()))
        .collect()
}

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
/// A kind `base` has a part of is checked through that part; any other kind through one extra part with its default
/// parameters (`base` supplies what it requires). A kind that can't be built that way is listed in
/// [`Report::unreached`], not skipped silently.
pub fn check(base: &CharacterDef, kinds: Option<&[&str]>, probes: &Probes) -> Report {
    let wanted: Vec<String> = match kinds {
        Some(kinds) => kinds.iter().map(|k| (*k).to_owned()).collect(),
        None => omuzaki_char_api::kinds().map(|r| (r.meta)().id.to_owned()).collect(),
    };
    let mut report = Report::default();
    let in_base: BTreeSet<&str> = base.parts.values().map(|p| p.kind.as_str()).collect();
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
        let mut def = base.clone();
        let part = PartDef {
            kind: PartKindId::new(kind.as_str()).unwrap(),
            parent: None,
            enabled: true,
            layer: 1000,
            attach: None,
            params: (registration.defaults)(),
        };
        def.parts.insert(PartId::new(PROBE).unwrap(), part);
        runs.push((def, vec![kind.clone()]));
    }
    for (def, run_kinds) in runs {
        let mut other = def.clone();
        other.palette = flipped(&def.palette);
        match (publishes_of(&def), publishes_of(&other)) {
            (Ok(a), Ok(b)) => compare(&def, &run_kinds, &a, &b, probes, &mut report),
            (Err(e), _) | (_, Err(e)) => {
                report.unreached.extend(run_kinds.into_iter().map(|k| (k, format!("does not build: {e}"))));
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
    merged.violations.dedup();
    merged
}

fn compare(
    def: &CharacterDef,
    kinds: &[String],
    a: &[(PartId, Publishes)],
    b: &[(PartId, Publishes)],
    probes: &Probes,
    report: &mut Report,
) {
    for ((part, under_a), (_, under_b)) in a.iter().zip(b) {
        let kind = def.parts[part].kind.as_str();
        if !kinds.iter().any(|k| k == kind) {
            continue;
        }
        if format!("{under_a:?}") != format!("{under_b:?}") {
            report.violations.push(format!(
                "kind '{kind}' (part '{part}') publishes {under_a:?} under one palette and {under_b:?} under another"
            ));
            continue;
        }
        for name in fact_names(under_a) {
            match probes.facts.get(name.as_str()) {
                Some(values) if values(under_a) != values(under_b) => report.violations.push(format!(
                    "kind '{kind}' (part '{part}'): fact '{name}' depends on the palette: {:?} under one palette, {:?} \
                     under another",
                    values(under_a),
                    values(under_b)
                )),
                Some(_) => {}
                None => {
                    report.unprobed.insert((kind.to_owned(), name));
                }
            }
        }
    }
}
