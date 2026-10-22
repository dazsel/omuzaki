//! Parity of the hair `framing` and `braid` generators (`omuzaki_char::hair::gen`) with the prototype's `parts/hair.js`
//! 199 to 287: the rest points of the ten framing locks, point by point, with the legacy stream consumed in the
//! prototype's order, and the braid's six strands as the tube vertices at the tail of mesh `hair-static`.
//!
//! The oracle is the sample character's dump, which is private test material kept in the local assets folder: without it
//! (always, in CI and in the cloud) every test here skips with a message. The head surface and the style's numbers are
//! the character's too. The head is the prototype's radius function sampled on a fine grid and kept in a local binary
//! file. The style comes from a local file next to the dumps, `geometry/char_hair/framing_braid.local.json`:
//!
//! ```json
//! { "variant": "game", "seed": 7, "first_framing_clump": 52, "braid_tail_vertices": 3840,
//!   "head_table": "geometry/char_hair/head_radius.local.bin", "centre": [x, y, z],
//!   "framing": { ... FramingGen, head-local ... }, "braid": { ... BraidGen ... } }
//! ```
//!
//! Rest points are held to 1e-4 m. The braid tubes are compared vertex by vertex (the port keeps the prototype's vertex
//! order) against `T-GEOM`: 2e-4 m and one degree. The framing tubes are not compared: the dump draws the simulated
//! locks after the settle, not at rest. The numbers measured are printed so a local run can propose the final
//! tolerances (the noise floor).

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // table indices are small and non-negative

use std::fs;
use std::path::Path;

use omuzaki_char::hair::compile::{CompiledGroup, HairGenCtx};
use omuzaki_char::hair::r#gen::{braid, framing};
use omuzaki_char::hair::tube::{OutwardFrame, RING, tube_vertices};
use omuzaki_char_api::{BraidGen, Dynamics, FramingGen, GroupId, HeadSurface, LegacyLcg, Quality};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_test::jsref::{HairDump, JsRef};
use serde::Deserialize;

/// Rest-point tolerance, metres.
const REST: f32 = 1e-4;
/// Tube vertex tolerance, metres.
const POSITION: f32 = 2e-4;
/// Tube normal tolerance, degrees.
const NORMAL_DEG: f32 = 1.0;
/// Framing locks drawn before anything else: two numbers each.
const FRAMING_DRAWS: usize = 20;
/// The prototype's body axis `z` for the outward reference.
const BODY_AXIS_Z: f32 = -0.03;

#[derive(Deserialize)]
struct Local {
    variant: String,
    seed: u32,
    first_framing_clump: usize,
    braid_tail_vertices: usize,
    head_table: String,
    centre: Vec3,
    framing: FramingGen,
    braid: BraidGen,
}

/// The head as the prototype's radius function sampled on a fine grid, read from a local file and interpolated
/// bilinearly: `nt + 1` rows at polar angle `i / nt * pi` from the top, `np + 1` columns at azimuth `j / np * 2 pi` from
/// the front toward +x. (The dump's head mesh is not that function below the cheeks, so it cannot stand in for it.)
#[derive(Debug)]
struct TableHead {
    centre: Vec3,
    radii: Vec<f32>,
    nt: usize,
    np: usize,
}

impl TableHead {
    fn load(path: &Path, centre: Vec3) -> Self {
        let bytes = fs::read(path).expect("the local head radius table");
        let word = |k: usize| u32::from_le_bytes(bytes[4 * k..4 * k + 4].try_into().expect("four bytes"));
        let (nt, np) = (word(0) as usize, word(1) as usize);
        let radii: Vec<f32> = bytes[8..].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        assert_eq!(radii.len(), (nt + 1) * (np + 1), "the table is nt+1 by np+1");
        Self { centre, radii, nt, np }
    }

    fn at(&self, i: usize, j: usize) -> f32 {
        self.radii[i * (self.np + 1) + j]
    }
}

impl HeadSurface for TableHead {
    fn centre(&self) -> Vec3 {
        self.centre
    }

    fn radius(&self, dir: Vec3) -> f32 {
        let dir = dir.normalize_or(Vec3::Y);
        let th = dir.y.clamp(-1.0, 1.0).acos();
        let ph = dir.x.atan2(dir.z).rem_euclid(std::f32::consts::TAU);
        let (fi, fj) = (th / std::f32::consts::PI * self.nt as f32, ph / std::f32::consts::TAU * self.np as f32);
        let (i, j) = ((fi.floor() as usize).min(self.nt - 1), (fj.floor() as usize).min(self.np - 1));
        let (a, b) = (fi - i as f32, fj - j as f32);
        let top = self.at(i, j) * (1.0 - b) + self.at(i, j + 1) * b;
        let bottom = self.at(i + 1, j) * (1.0 - b) + self.at(i + 1, j + 1) * b;
        top * (1.0 - a) + bottom * a
    }
}

/// The dump, the local numbers and the head, or `None` (after saying why) so the caller skips.
fn private() -> Option<(JsRef, Local, TableHead)> {
    let js = JsRef::open()?;
    let path = js.root().join("geometry/char_hair/framing_braid.local.json");
    let Ok(text) = fs::read_to_string(&path) else {
        eprintln!("skipped: no local framing and braid numbers at {}", path.display());
        return None;
    };
    let local: Local = serde_json::from_str(&text).expect("the local framing and braid file parses");
    let head = TableHead::load(&js.root().join(&local.head_table), local.centre);
    Some((js, local, head))
}

/// Compiles `compile` with the prototype's stream at the start, and returns the group and the stream after it.
fn compile_with(
    head: &TableHead,
    seed: u32,
    compile: impl FnOnce(&mut HairGenCtx<'_>) -> CompiledGroup,
) -> (CompiledGroup, LegacyLcg) {
    let mut rng = LegacyLcg::new(seed).expect("a valid legacy seed");
    // Neither generator reads the palette; its default stands in for the character's.
    #[allow(clippy::default_trait_access)]
    let palette = Default::default();
    let (id, dynamics) = (GroupId::new("c-parity").unwrap(), Dynamics::default());
    let group = {
        let mut ctx = HairGenCtx::new(head, &palette, Quality::Live, &id, &dynamics, &mut rng);
        compile(&mut ctx)
    };
    (group, rng)
}

/// The largest of a run of errors, with where it is. Every error must be finite, and the run must not be empty: a NaN
/// drops out of `>` and of `f32::max` without a trace, and an empty run compares nothing and passes.
#[derive(Debug, Default)]
struct Worst {
    value: f32,
    at: (usize, usize),
    seen: usize,
}

impl Worst {
    fn fold(&mut self, error: f32, at: (usize, usize)) {
        assert!(error.is_finite(), "non-finite error at clump {}, point {}: {error}", at.0, at.1);
        self.seen += 1;
        if error > self.value {
            (self.value, self.at) = (error, at);
        }
    }

    fn finish(self, what: &str) -> Self {
        assert!(self.seen > 0, "{what}: nothing compared, so nothing was checked");
        self
    }
}

fn dump_clump(dump: &HairDump, index: usize) -> &omuzaki_test::jsref::HairClump {
    dump.clumps.get(index).expect("the dump has the clump")
}

#[test]
fn every_framing_lock_matches_the_prototypes_rest_points() {
    let Some((js, local, head)) = private() else { return };
    let dump = js.hair(&local.variant).unwrap();
    let (group, mut rng_after) = compile_with(&head, local.seed, |ctx| framing::compile(&local.framing, ctx));
    assert_eq!(group.clumps.len(), framing::clump_count(&local.framing), "lock count");
    let after = local.first_framing_clump + group.clumps.len();
    assert!(
        dump.clumps.get(after).is_none_or(|c| c.shell),
        "the dump has more framing locks than the generator makes ({})",
        group.clumps.len()
    );
    let mut points = Worst::default();
    for (n, clump) in group.clumps.iter().enumerate() {
        let theirs = dump_clump(&dump, local.first_framing_clump + n);
        assert!(theirs.dynamic && !theirs.shell, "lock {n}: the dump's clump is a framing lock");
        assert_eq!(clump.rest.len(), theirs.rest.len(), "lock {n}: point counts");
        for (k, (a, b)) in clump.rest.iter().zip(&theirs.rest).enumerate() {
            assert!(a.is_finite() && b.is_finite(), "lock {n} point {k}: non-finite rest point");
            points.fold(a.distance(*b), (n, k));
        }
    }
    // The stream stands where the prototype's does when the curtain starts: after exactly 20 draws.
    let mut fresh = LegacyLcg::new(local.seed).unwrap();
    for _ in 0..FRAMING_DRAWS {
        fresh.next_unit();
    }
    assert_eq!(rng_after.next_unit(), fresh.next_unit(), "the framing locks drew {FRAMING_DRAWS} numbers");
    let points = points.finish("rest points");
    eprintln!(
        "framing: {} locks, worst rest-point error {:.2e} m (lock {}, point {})",
        group.clumps.len(),
        points.value,
        points.at.0,
        points.at.1
    );
    assert!(points.value <= REST, "worst rest-point error {:.2e} m exceeds {REST:.0e} m", points.value);
}

#[test]
fn the_braid_tubes_match_the_prototypes_vertices() {
    let Some((js, local, head)) = private() else { return };
    let (group, _) = compile_with(&head, local.seed, |ctx| braid::compile(&local.braid, ctx));
    assert_eq!(group.clumps.len(), braid::clump_count(&local.braid), "strand count");
    let geometry = js.geometry(&local.variant).unwrap();
    let theirs = &geometry.mesh("hair-static").expect("the dump has hair-static").mesh;
    let first = theirs.positions.len() - local.braid_tail_vertices;
    let frame = OutwardFrame { head: head.centre, body_axis: Vec2::new(0.0, BODY_AXIS_Z) };
    let (mut positions, mut normals) = (Worst::default(), Worst::default());
    let mut vertex = first;
    for (n, clump) in group.clumps.iter().enumerate() {
        let count = clump.rest.len() * RING;
        let (mut p, mut nrm) = (vec![Vec3::ZERO; count], vec![Vec3::ZERO; count]);
        tube_vertices(&clump.rest, &clump.width, &clump.thickness, |q| frame.rest(q), &mut p, &mut nrm).unwrap();
        for k in 0..count {
            let (their_p, their_n) = (theirs.positions[vertex + k], theirs.normals[vertex + k]);
            positions.fold(p[k].distance(their_p), (n, k));
            normals.fold(nrm[k].angle_between(their_n).to_degrees(), (n, k));
        }
        vertex += count;
    }
    assert_eq!(vertex, theirs.positions.len(), "the strands fill the tail of hair-static exactly");
    let (positions, normals) = (positions.finish("braid positions"), normals.finish("braid normals"));
    eprintln!(
        "braid: {} strands, {} vertices, worst position error {:.2e} m (strand {}, vertex {}), worst normal error {:.3} degrees",
        group.clumps.len(),
        local.braid_tail_vertices,
        positions.value,
        positions.at.0,
        positions.at.1,
        normals.value
    );
    assert!(positions.value <= POSITION, "worst position error {:.2e} m exceeds {POSITION:.0e} m", positions.value);
    assert!(normals.value <= NORMAL_DEG, "worst normal error {:.3} degrees exceeds {NORMAL_DEG}", normals.value);
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
    #[should_panic(expected = "non-finite error at clump 2, point 5")]
    fn a_nan_error_fails() {
        Worst::default().fold(f32::NAN, (2, 5));
    }
}
