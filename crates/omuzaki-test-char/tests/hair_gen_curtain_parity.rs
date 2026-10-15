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

#[test]
fn every_guide_matches_the_prototypes_rest_points_and_u() {
    let Some((js, local)) = private() else { return };
    let dump = js.hair(&local.variant).unwrap();
    let group = guides(&local);
    let GroupGeometry::Strip { u: across, .. } = &group.geometry else { panic!("the curtain is a strip") };
    assert_eq!(group.clumps.len(), curtain::clump_count(&local.curtain), "guide count");
    assert_eq!(across.len(), group.clumps.len(), "one u per guide");
    // The worst point of the whole curtain, with the guide and point it is at, so a bad run says where to look.
    let (mut worst, mut worst_at) = (0.0_f32, (0, 0));
    let mut worst_across = 0.0_f32;
    for (n, (clump, u)) in group.clumps.iter().zip(across).enumerate() {
        let theirs = dump.clumps.get(local.first_curtain_clump + n).expect("the dump has every curtain clump");
        assert!(theirs.shell, "guide {n}: the dump's clump is a curtain guide");
        let their_u = theirs.u.expect("a curtain guide has its u");
        worst_across = worst_across.max((u - their_u).abs());
        assert_eq!(clump.rest.len(), theirs.rest.len(), "guide {n}: point counts");
        for (k, (a, b)) in clump.rest.iter().zip(&theirs.rest).enumerate() {
            let d = a.distance(*b);
            if d > worst {
                (worst, worst_at) = (d, (n, k));
            }
        }
    }
    eprintln!(
        "curtain: {} guides, worst rest-point error {worst:.2e} m (guide {}, point {}), worst u error {worst_across:.2e}",
        group.clumps.len(),
        worst_at.0,
        worst_at.1
    );
    assert!(worst_across <= ACROSS, "worst u error {worst_across:.2e} exceeds {ACROSS:.0e}");
    assert!(worst <= REST, "worst rest-point error {worst:.2e} m exceeds {REST:.0e} m");
}
