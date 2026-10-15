//! Parity of the hair `curtain` generator's guides (`omuzaki_char::hair::gen::curtain`) with the prototype's
//! `parts/hair.js` 291 to 326: the rest pose of every guide, point by point, and each guide's `u` across the back.
//!
//! The oracle is the sample character's dump, which is private test material kept in the local assets folder: without it
//! (always, in CI and in the cloud) every test here skips with a message. The character's numbers are not public either,
//! so a local file next to the dumps supplies them, `geometry/char_hair/curtain.local.json`:
//!
//! ```json
//! { "variant": "game", "seed": 1, "framing_locks": 0, "first_curtain_clump": 0,
//!   "head": { "centre": [x, y, z], "semi_axes": [a, b, c] }, "curtain": { ... CurtainGen ... } }
//! ```
//!
//! The head is an ellipsoid fitted to the character's head by the dump script. `curtain` carries the envelope and the
//! wave, which the prototype keeps private (its `ENVELOPE`). The guides draw from the prototype's legacy stream, which
//! has to stand where the prototype's does when the curtain starts: each framing lock draws two numbers before it, so the
//! stream is advanced past `2 * framing_locks` draws first. Rest points are held to 1e-4 m and `u` to 1e-6. The numbers
//! measured are printed so the first run with the dumps can propose the final tolerances. The strip mesh is not compared
//! here yet: that waits for #1679's treatment.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason

use std::fs;

use omuzaki_char::hair::compile::{CompiledGroup, GroupGeometry, HairGenCtx};
use omuzaki_char::hair::r#gen::curtain;
use omuzaki_char_api::{CurtainGen, Dynamics, GroupId, HeadSurface, LegacyLcg, Quality};
use omuzaki_core::glam::Vec3;
use omuzaki_test::jsref::JsRef;
use serde::Deserialize;

/// Rest-point tolerance, metres.
const REST: f32 = 1e-4;
/// `u` tolerance, across the back (0 to 1).
const ACROSS: f32 = 1e-6;

#[derive(Deserialize)]
struct Local {
    variant: String,
    /// The character's legacy seed, as its `HairRng::LegacyLcg` gives it.
    seed: u32,
    /// Framing locks drawn before the curtain: two numbers each.
    framing_locks: usize,
    /// Where the curtain's guides start in the dump's clump list.
    first_curtain_clump: usize,
    head: Ellipsoid,
    curtain: CurtainGen,
}

#[derive(Debug, Deserialize)]
struct Ellipsoid {
    centre: Vec3,
    semi_axes: Vec3,
}

impl HeadSurface for Ellipsoid {
    fn centre(&self) -> Vec3 {
        self.centre
    }
    fn radius(&self, dir: Vec3) -> f32 {
        1.0 / (dir / self.semi_axes).length()
    }
}

/// The dump and the character's local numbers, or `None` (after saying why) so the caller skips.
fn private() -> Option<(JsRef, Local)> {
    let js = JsRef::open()?;
    let path = js.root().join("geometry/char_hair/curtain.local.json");
    let Ok(text) = fs::read_to_string(&path) else {
        eprintln!("skipped: no local curtain numbers at {}", path.display());
        return None;
    };
    Some((js, serde_json::from_str(&text).expect("the local curtain file parses")))
}

/// The curtain's guides as the prototype's stream makes them for the character.
fn guides(local: &Local) -> CompiledGroup {
    let mut rng = LegacyLcg::new(local.seed).expect("a valid legacy seed");
    for _ in 0..2 * local.framing_locks {
        rng.next_unit();
    }
    // The curtain never reads the palette, so its default stands in for the character's. The type is not a dependency of
    // this crate, so inference takes it from `HairGenCtx::new`.
    #[allow(clippy::default_trait_access)]
    let palette = Default::default();
    let (id, dynamics) = (GroupId::new("c-curtain").unwrap(), Dynamics::default());
    let mut ctx = HairGenCtx::new(&local.head, &palette, Quality::Live, &id, &dynamics, &mut rng);
    curtain::compile(&local.curtain, &mut ctx)
}

/// The largest of a run of errors, with the guide and point it is at. Every error must be finite, and the run must not be
/// empty: a NaN drops out of `>` and of `f32::max` without a trace, and an empty run compares nothing and passes.
#[derive(Debug, Default)]
struct Worst {
    value: f32,
    at: (usize, usize),
    seen: usize,
}

impl Worst {
    fn fold(&mut self, error: f32, at: (usize, usize)) {
        assert!(error.is_finite(), "non-finite error at guide {}, point {}: {error}", at.0, at.1);
        self.seen += 1;
        if error > self.value {
            (self.value, self.at) = (error, at);
        }
    }

    /// The finished run, refusing one that compared nothing.
    fn finish(self, what: &str) -> Self {
        assert!(self.seen > 0, "{what}: nothing compared, so nothing was checked");
        self
    }
}

#[test]
fn every_guide_matches_the_prototypes_rest_points_and_u() {
    let Some((js, local)) = private() else { return };
    let dump = js.hair(&local.variant).unwrap();
    let group = guides(&local);
    let GroupGeometry::Strip { u: across, .. } = &group.geometry else { panic!("the curtain is a strip") };
    assert_eq!(group.clumps.len(), curtain::clump_count(&local.curtain), "guide count");
    assert_eq!(across.len(), group.clumps.len(), "one u per guide");
    let after = local.first_curtain_clump + group.clumps.len();
    assert!(
        dump.clumps.get(after).is_none_or(|c| !c.shell),
        "the dump has more curtain guides than the generator makes ({})",
        group.clumps.len()
    );
    let (mut points, mut across_worst) = (Worst::default(), Worst::default());
    for (n, (clump, u)) in group.clumps.iter().zip(across).enumerate() {
        let theirs = dump.clumps.get(local.first_curtain_clump + n).expect("the dump has every curtain clump");
        assert!(theirs.shell, "guide {n}: the dump's clump is a curtain guide");
        let their_u = theirs.u.expect("a curtain guide has its u");
        across_worst.fold((u - their_u).abs(), (n, 0));
        assert_eq!(clump.rest.len(), theirs.rest.len(), "guide {n}: point counts");
        for (k, (a, b)) in clump.rest.iter().zip(&theirs.rest).enumerate() {
            assert!(a.is_finite() && b.is_finite(), "guide {n} point {k}: non-finite rest point");
            points.fold(a.distance(*b), (n, k));
        }
    }
    let (points, across_worst) = (points.finish("rest points"), across_worst.finish("u"));
    eprintln!(
        "curtain: {} guides, worst rest-point error {:.2e} m (guide {}, point {}), worst u error {:.2e}",
        group.clumps.len(),
        points.value,
        points.at.0,
        points.at.1,
        across_worst.value
    );
    assert!(across_worst.value <= ACROSS, "worst u error {:.2e} exceeds {ACROSS:.0e}", across_worst.value);
    assert!(points.value <= REST, "worst rest-point error {:.2e} m exceeds {REST:.0e} m", points.value);
}

#[cfg(test)]
mod tests {
    use super::Worst;

    #[test]
    fn the_worst_error_keeps_its_place() {
        let mut worst = Worst::default();
        for (k, e) in [0.5, 2.0, 1.0].into_iter().enumerate() {
            worst.fold(e, (3, k));
        }
        let worst = worst.finish("test");
        assert_eq!((worst.value, worst.at, worst.seen), (2.0, (3, 1), 3));
    }

    #[test]
    #[should_panic(expected = "nothing compared")]
    fn an_empty_comparison_fails() {
        let _ = Worst::default().finish("rest points");
    }

    #[test]
    #[should_panic(expected = "non-finite error at guide 2, point 5")]
    fn a_nan_error_fails() {
        Worst::default().fold(f32::NAN, (2, 5));
    }
}
