//! Seeded mutations of well-formed files: none panics, and every refusal names an `inochi2d.<rule>`.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use common::Rng;
use omuzaki_inochi2d::{ImportError, Limits, import, read_container};
use serde_json::json;

/// Tight limits, so a mutated size field costs milliseconds, not gigabytes.
fn limits() -> Limits {
    Limits {
        max_payload_bytes: 1 << 20,
        max_nodes: 4096,
        max_vertices: 1 << 16,
        max_offsets: 1 << 18,
        max_textures: 64,
        max_texture_bytes: 1 << 20,
        max_texture_dim: 256,
        max_decoded_bytes: 1 << 22,
        ..Limits::default()
    }
}

/// A richer seed than the minimal file: masks, a composite, a mesh group, physics, a two-axis parameter.
fn rich() -> Vec<u8> {
    let mut puppet = common::minimal();
    let mut masked = common::part(3, "Masked", 0);
    masked["masks"] = json!([{ "source": 2, "mode": "DodgeMask" }]);
    puppet["nodes"]["children"].as_array_mut().unwrap().extend([
        json!({ "uuid": 4, "name": "Group", "type": "Composite", "transform": common::identity(), "children": [masked],
                "blend_mode": "Multiply", "opacity": 0.5, "propagate_meshgroup": true }),
        json!({ "uuid": 5, "name": "Warp", "type": "MeshGroup", "transform": common::identity(),
                "mesh": common::quad_mesh(), "children": [common::part(6, "Under", 0)] }),
        json!({ "uuid": 7, "name": "Hair", "type": "SimplePhysics", "transform": common::identity(), "param": 101,
                "model_type": "SpringPendulum", "map_mode": "XY", "length": 50.0 }),
        json!({ "uuid": 8, "name": "Mystery", "type": "Lattice2000", "transform": common::identity() }),
    ]);
    puppet["param"].as_array_mut().unwrap().push(json!({
        "uuid": 101, "name": "Sway", "is_vec2": true, "min": [-1.0, -1.0], "max": [1.0, 1.0],
        "axis_points": [[0.0, 1.0], [0.0, 0.5, 1.0]],
        "bindings": [{ "node": 4, "param_name": "transform.r.z", "values": [[0.0, 0.1, 0.2], [0.3, 0.4, 0.5]],
                       "isSet": [[true, false, true], [true, true, true]], "interpolate_mode": "Cubic" }],
    }));
    common::inp1(&puppet, Some(&[(0, common::png(4, 4, &common::pixels(4, 4)))]))
}

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut b = seed.to_vec();
    for _ in 0..=rng.below(4) {
        if b.is_empty() {
            break;
        }
        let at = rng.below(b.len());
        match rng.below(6) {
            0 => b[at] ^= 1 << rng.below(8),
            1 => b[at] = u8::try_from(rng.below(256)).unwrap(),
            2 => {
                b.remove(at);
            }
            3 => b.insert(at, u8::try_from(rng.below(256)).unwrap()),
            4 => {
                // A whole word: lengths and counts are where hostile files aim.
                let word = [0xFF, 0xFF, 0xFF, 0xFF];
                let end = (at + 4).min(b.len());
                b[at..end].copy_from_slice(&word[..end - at]);
            }
            _ => {
                // A digit in the JSON: numbers that turn into uuids, indices, sizes.
                if let Some(i) = (at..b.len()).find(|&i| b[i].is_ascii_digit()) {
                    b[i] = b'0' + u8::try_from(rng.below(10)).unwrap();
                }
            }
        }
    }
    b
}

#[test]
fn ten_thousand_mutations_never_panic() {
    let seeds = [common::minimal_file(), rich(), common::inp2_file()];
    let limits = limits();
    let mut rng = Rng(0x0BAD_5EED_1234_5678);
    let (mut ok, mut refused) = (0usize, 0usize);
    for _ in 0..10_000 {
        let seed = &seeds[rng.below(seeds.len())];
        let bytes = mutate(&mut rng, seed);
        let _ = read_container(&bytes, &limits);
        match import(&bytes, &limits) {
            Ok(imported) => {
                assert!(imported.def.check().is_ok());
                ok += 1;
            }
            Err(e) => {
                assert!(e.rule().starts_with("inochi2d."), "{e:?}");
                assert!(matches!(
                    e,
                    ImportError::Malformed { .. } | ImportError::Limit { .. } | ImportError::Unsupported { .. }
                ));
                refused += 1;
            }
        }
    }
    // Both outcomes are exercised: the mutations are neither all harmless nor all fatal.
    assert!(ok > 100 && refused > 100, "{ok} imported, {refused} refused");
}

#[test]
fn the_rich_seed_imports() {
    let bytes = rich();
    let imported = import(&bytes, &limits()).unwrap();
    let m = &imported.report.mapped;
    assert_eq!((m.parts, m.composites, m.mesh_deformers, m.unknown, m.pendulums, m.params), (3, 1, 1, 1, 1, 2));
}
