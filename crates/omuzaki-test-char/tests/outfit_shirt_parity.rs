//! Parity of the `shirt` garment (`omuzaki_char::outfit::shirt`, #190) with the prototype's shirt, as the Director's
//! reference for #190 gives it.
//!
//! The oracle is the sample character's dump (`geometry/game`: the meshes `shirt`, `shirt-lining`, `shirt-lines` and
//! `shirt-holes`), private test material kept in the local assets folder: without it (always, in CI and in the cloud) the
//! tests skip with a message. The prototype's shirt is her body's own envelope (ease 0, a 10 % hip tuck, a sleeve 6 to 8 mm
//! off the arm), so the port is built on the soft-sd body, whose torso and arm are that body, with the ease at its least
//! (1 mm), the tuck at 10 % and the sleeve's width at 6.5 mm.
//!
//! What differs, deliberately, and so what each test compares:
//! - the dump's `shirt` mesh also holds the collar (the collar is its own part, `collar`): its range is left out;
//! - the port's sleeves start behind the arm and end where the body is cut (`outfit/shirt/union.rs`), so the shoulders
//!   are compared as surfaces (nearest vertices), not index by index;
//! - the port's pearl and thread holes are lathes, not three.js solids: their vertex counts differ (68 and 28 against
//!   100 and 40), their shapes agree.
//!
//! The numbers measured are printed so the first run with the dumps can propose the tolerances.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic on small counts

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, PartDef, PartId, PartKindId,
    RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_test::jsref::JsRef;
use serde_json::json;

/// The geometry set the dumps hold the shirt in.
const SET: &str = "game";
/// Vertices of the dump's torso grid (37 rows of 61), a sleeve (30 of 25), a cuff band (16 of 41), the pearl (a three.js
/// cylinder and torus), and the collar (two shells of 15 x 45) on each side.
const TORSO: usize = 37 * 61;
const SLEEVE: usize = 30 * 25;
const CUFF: usize = 16 * 41;
const PEARL: usize = 100 + 119;
const COLLAR: usize = 2 * 15 * 45;
/// How far the nearest-vertex distance may be, metres, on average and at the 95th percentile, between a port surface
/// and the prototype's.
const MEAN: f64 = 0.003;
const P95: f64 = 0.010;

fn dump(name: &str) -> Option<MeshData> {
    let js = JsRef::open()?;
    let geometry = js.geometry(SET).expect("the geometry dump loads");
    Some(geometry.mesh(name).unwrap_or_else(|| panic!("the dump has `{name}`")).mesh.clone())
}

fn built() -> BuiltCharacter {
    // Referring to the character crate links its part kinds.
    assert!(!omuzaki_char::template::registry::templates().is_empty());
    let mut def: CharacterDef = mock_character().unwrap();
    def.parts.clear();
    let part = |kind: &str, layer: i32, params: serde_json::Value| PartDef {
        kind: PartKindId::new(kind).unwrap(),
        parent: None,
        enabled: true,
        layer,
        attach: None,
        params,
    };
    def.template = "soft-sd".parse().unwrap();
    def.parts.insert(PartId::new("body").unwrap(), part("body", 0, json!({})));
    let shirt = json!({
        "common": { "ease": 0.001, "fabric": { "kind": "plain", "colour": "#f5f2ed" } },
        "tuck": 0.10,
        "sleeve": { "width": 0.0065 },
    });
    def.parts.insert(PartId::new("shirt").unwrap(), part("shirt", 20, shirt));
    RegistryBuilder
        .build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

fn mesh<'a>(built: &'a BuiltCharacter, name: &str) -> &'a MeshData {
    &built.meshes.iter().find(|m| m.mesh.name == name).unwrap_or_else(|| panic!("no mesh `{name}`")).mesh.geometry
}

/// The distance from each of `from` to the nearest of `to`, sorted.
fn nearest(from: &[Vec3], to: &[Vec3]) -> Vec<f64> {
    let mut d: Vec<f64> = from
        .iter()
        .map(|p| to.iter().map(|q| f64::from(p.distance_squared(*q))).fold(f64::INFINITY, f64::min).sqrt())
        .collect();
    d.sort_by(f64::total_cmp);
    d
}

/// (mean, 95th percentile, maximum) of sorted distances.
fn summary(d: &[f64]) -> (f64, f64, f64) {
    let mean = d.iter().sum::<f64>() / d.len() as f64;
    (mean, d[(d.len() - 1) * 95 / 100], d[d.len() - 1])
}

/// The two-way nearest-vertex distances between the port's `a` and the prototype's `b`.
fn chamfer(label: &str, a: &[Vec3], b: &[Vec3]) -> [(f64, f64, f64); 2] {
    let (ab, ba) = (summary(&nearest(a, b)), summary(&nearest(b, a)));
    println!(
        "{label}: port to prototype mean {:.2} p95 {:.2} max {:.2} mm; prototype to port mean {:.2} p95 {:.2} max {:.2} mm",
        ab.0 * 1e3,
        ab.1 * 1e3,
        ab.2 * 1e3,
        ba.0 * 1e3,
        ba.1 * 1e3,
        ba.2 * 1e3
    );
    [ab, ba]
}

#[test]
fn the_body_sleeves_and_cuffs_lie_on_the_prototypes_surfaces() {
    let Some(js) = dump("shirt") else {
        eprintln!("skipped: no JS dumps ($OMUZAKI_ASSETS/goldens/js)");
        return;
    };
    assert_eq!(js.positions.len(), TORSO + 2 * (SLEEVE + CUFF + PEARL + COLLAR), "the dump is torso, then two sides");
    let side = SLEEVE + CUFF + PEARL + COLLAR;
    // The prototype without its collar: the torso, then each side's sleeve, cuff and pearl.
    let mut prototype: Vec<Vec3> = js.positions[..TORSO].to_vec();
    for k in 0..2 {
        let start = TORSO + k * side;
        prototype.extend_from_slice(&js.positions[start..start + SLEEVE + CUFF + PEARL]);
    }
    let built = built();
    let port: Vec<Vec3> = mesh(&built, "shirt-shirt").positions.clone();
    let [to_prototype, to_port] = chamfer("shirt", &port, &prototype);
    assert!(
        to_prototype.0 <= MEAN && to_prototype.1 <= P95,
        "the port lies on the prototype's shirt: {to_prototype:?}"
    );
    // The prototype's own shoulders are inside the port's arm; the sleeves are 1.5 mm off in width.
    assert!(to_port.0 <= MEAN && to_port.1 <= P95 * 2.0, "the prototype's shirt is covered by the port's: {to_port:?}");
    // The body grid alone, between the hem and the armpits, where no sleeve is.
    let body_only = |p: &Vec3| p.x.abs() < 0.09 && (0.384..0.44).contains(&p.y);
    let torso: Vec<Vec3> = port.iter().copied().filter(body_only).collect();
    let proto_torso: Vec<Vec3> = js.positions[..TORSO].iter().copied().filter(body_only).collect();
    let [body, _] = chamfer("body", &torso, &proto_torso);
    assert!(body.1 <= 0.0012, "the body is 1 mm off the prototype's, the shirt's ease: {body:?}");
}

#[test]
fn the_lining_and_the_seam_rings_have_the_prototypes_counts_and_lie_on_its_surfaces() {
    let (Some(lining), Some(lines)) = (dump("shirt-lining"), dump("shirt-lines")) else {
        eprintln!("skipped: no JS dumps ($OMUZAKI_ASSETS/goldens/js)");
        return;
    };
    let built = built();
    let port = mesh(&built, "shirt-lining");
    assert_eq!((port.positions.len(), port.triangle_count()), (610, 1080), "10 rings of 61");
    assert_eq!((lining.positions.len(), lining.triangle_count()), (610, 1080));
    let [ab, ba] = chamfer("lining", &port.positions, &lining.positions);
    assert!(ab.2 <= 0.003 && ba.2 <= 0.003, "the lining is 1 mm off the prototype's: {ab:?} {ba:?}");
    // The prototype's lines mesh also holds the collar's two cords on each side (720 vertices, 1160 triangles).
    let port = mesh(&built, "shirt-lines");
    assert_eq!((port.positions.len(), port.triangle_count()), (2 * 41 * 7, 2 * 40 * 6 * 2));
    assert_eq!((lines.positions.len(), lines.triangle_count()), (1294, 2120));
    let cuffs: Vec<Vec3> = {
        let seam = 41 * 7;
        let side = seam + 360; // each side: the seam ring, then the collar's two cords (45 and 15 points of 6)
        let mut v = lines.positions[..seam].to_vec();
        v.extend_from_slice(&lines.positions[side..side + seam]);
        v
    };
    let [ab, ba] = chamfer("seam rings", &port.positions, &cuffs);
    assert!(ab.2 <= 0.004 && ba.2 <= 0.004, "the rings are on the cuffs: {ab:?} {ba:?}");
}

#[test]
fn the_thread_holes_are_eight_small_discs_on_the_pearls() {
    let Some(holes) = dump("shirt-holes") else {
        eprintln!("skipped: no JS dumps ($OMUZAKI_ASSETS/goldens/js)");
        return;
    };
    let built = built();
    let port = mesh(&built, "shirt-holes");
    assert_eq!((holes.positions.len(), holes.triangle_count()), (320, 192), "eight three.js cylinders of 40 vertices");
    assert_eq!(port.positions.len(), 8 * 28, "eight lathes of 4 rings of 7");
    let centre = |v: &[Vec3]| v.iter().copied().sum::<Vec3>() / v.len() as f32;
    let (a, b) = (centre(&port.positions), centre(&holes.positions));
    println!("holes: centres {:.2} mm apart", f64::from(a.distance(b)) * 1e3);
    let [ab, ba] = chamfer("holes", &port.positions, &holes.positions);
    assert!(ab.2 <= 0.008 && ba.2 <= 0.008, "the holes are on the pearls: {ab:?} {ba:?}");
}
