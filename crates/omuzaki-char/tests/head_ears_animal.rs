//! Animal ears (CB-14, #162): cat, fox, rabbit and wolf ears stand flush on top of the head, the pair symmetric, every
//! shell watertight; they publish their tip anchors, take their colours from the palette, and the rabbit's sway on a
//! spring chain that validates against the built skeleton.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss)] // counts in messages

mod common;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use omuzaki_char::head::ears::animal::{self, AROUND, Species};
use omuzaki_char::head::ears::{self, Side};
use omuzaki_char::head::mesh::{Resolution, build_head};
use omuzaki_char_api::{BuiltCharacter, EarKind, EarSpecies, Ears, HeadParams, HeadSurface};
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_doc_mock::MemDoc;
use serde_json::json;

fn default_head() -> &'static Arc<dyn HeadSurface> {
    static HEAD: OnceLock<Arc<dyn HeadSurface>> = OnceLock::new();
    HEAD.get_or_init(|| build_head(&HeadParams::default(), Resolution::LIVE, None).unwrap().surface)
}

fn animal(species: EarSpecies, size: f64) -> Ears {
    let mut ears = Ears { kind: EarKind::Animal, size, ..Ears::default() };
    ears.animal.species = species;
    ears
}

/// Both ears of `ears` on the default head, left then right.
fn pair(ears: &Ears) -> [animal::AnimalEar; 2] {
    let head = default_head().as_ref();
    let root = ears::root(ears, head);
    Side::BOTH.map(|side| animal::build(ears, side, root, head).unwrap())
}

/// How far `p` stands off `head`'s surface (negative: inside it).
fn off(head: &dyn HeadSurface, p: Vec3) -> f32 {
    let d = p - head.centre();
    d.length() - head.radius(d.normalize())
}

#[test]
fn every_species_stands_flush_on_the_head_and_never_sinks() {
    let head = default_head().as_ref();
    for species in EarSpecies::ALL {
        for size in [0.5, 1.0, 2.0] {
            let [left, _] = pair(&animal(species, size));
            let base = &left.shell.positions[1..=AROUND];
            let gap = base.iter().map(|p| off(head, *p).abs()).fold(0.0, f32::max);
            let sink = left.shell.positions.iter().map(|p| -off(head, *p)).fold(0.0, f32::max);
            assert!(gap < 0.001, "{species:?} x{size}: the base ring stands {gap} m off the head");
            assert!(sink < 0.002, "{species:?} x{size}: the shell sinks {sink} m into the head");
            let height = off(head, left.tip);
            assert!(
                height > 0.04 * omuzaki_face_api::narrow(size),
                "{species:?} x{size}: the tip is only {height} m above the head"
            );
        }
    }
}

#[test]
fn the_pair_is_symmetric() {
    let cx = default_head().centre().x;
    for species in EarSpecies::ALL {
        let [left, right] = pair(&animal(species, 1.0));
        let worst = left
            .shell
            .positions
            .iter()
            .zip(&right.shell.positions)
            .chain(left.panel.positions.iter().zip(&right.panel.positions))
            .map(|(l, r)| Vec3::new(2.0 * cx - l.x, l.y, l.z).distance(*r))
            .fold(0.0, f32::max);
        assert!(worst < 1e-6, "{species:?}: {worst}");
        assert!(left.tip.x > cx && right.tip.x < cx, "the left ear is on the left");
    }
}

/// Every edge of `mesh` is shared by exactly two triangles, once each way.
fn watertight(mesh: &MeshData) -> Result<(), String> {
    let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
    for t in mesh.indices.as_chunks::<3>().0 {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *edges.entry((a.min(b), a.max(b))).or_default() += if a < b { 1 } else { -1 };
        }
    }
    let mut uses: HashMap<(u32, u32), usize> = HashMap::new();
    for t in mesh.indices.as_chunks::<3>().0 {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *uses.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let open = uses.values().filter(|&&n| n != 2).count();
    let unpaired = edges.values().filter(|&&n| n != 0).count();
    if open == 0 && unpaired == 0 { Ok(()) } else { Err(format!("{open} open edges, {unpaired} wound one way twice")) }
}

#[test]
fn every_shell_is_watertight_and_faces_out() {
    for species in EarSpecies::ALL {
        for size in [0.3, 1.0, 3.0] {
            for ear in pair(&animal(species, size)) {
                watertight(&ear.shell).unwrap_or_else(|e| panic!("{species:?} x{size}: {e}"));
                let volume: f32 = ear
                    .shell
                    .indices
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|t| {
                        let [a, b, c] = [t[0], t[1], t[2]].map(|i| ear.shell.positions[i as usize]);
                        a.dot(b.cross(c)) / 6.0
                    })
                    .sum();
                assert!(volume > 0.0, "{species:?} x{size}: the shell faces in");
            }
        }
    }
}

#[test]
fn the_species_differ_as_described() {
    let head = default_head().as_ref();
    let measure = |species| {
        let [left, _] = pair(&animal(species, 1.0));
        let s = Species::of(species);
        (off(head, left.tip), s.half_width, left.chain.len())
    };
    let (cat, fox, rabbit, wolf) =
        (measure(EarSpecies::Cat), measure(EarSpecies::Fox), measure(EarSpecies::Rabbit), measure(EarSpecies::Wolf));
    println!("tip above the head (m): cat {:.3}, fox {:.3}, rabbit {:.3}, wolf {:.3}", cat.0, fox.0, rabbit.0, wolf.0);
    assert!(rabbit.0 > fox.0 && fox.0 > cat.0, "the rabbit's is the longest, the fox's taller than the cat's");
    assert!(
        wolf.1 > cat.1 && wolf.1 > fox.1 && rabbit.1 < cat.1,
        "the wolf's is the broadest, the rabbit's the narrowest"
    );
    assert_eq!((cat.2, fox.2, rabbit.2, wolf.2), (0, 0, 3, 0), "only the rabbit's sways");
}

#[test]
fn both_ears_build_within_the_budget() {
    let head = default_head().as_ref();
    let ears = animal(EarSpecies::Rabbit, 1.0);
    let root = ears::root(&ears, head);
    let runs = 20;
    let start = Instant::now();
    for _ in 0..runs {
        for side in Side::BOTH {
            animal::build(&ears, side, root, head).unwrap();
        }
    }
    let each = start.elapsed().as_secs_f64() * 1e3 / f64::from(runs);
    println!("both ears build in {each:.3} ms");
    // The spec's 5 ms is for a release build; a debug build is several times slower, so it is held to 25.
    assert!(each < if cfg!(debug_assertions) { 25.0 } else { 5.0 }, "{each} ms");
}

fn kit_with(species: &str) -> (MemDoc, BuiltCharacter) {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    common::apply(
        &mut doc,
        "part.set_param",
        json!({"character": "kit", "part": "head", "path": "ears/kind", "value": "animal"}),
    );
    common::apply(
        &mut doc,
        "part.set_param",
        json!({"character": "kit", "part": "head", "path": "ears/animal/species", "value": species}),
    );
    let built = common::build(&doc, "kit");
    (doc, built)
}

#[test]
fn on_the_kit_the_ears_publish_their_tips_and_take_the_palettes_colours() {
    for species in ["cat", "fox", "rabbit", "wolf"] {
        let (_, built) = kit_with(species);
        let names: Vec<&str> = built.meshes.iter().map(|m| m.mesh.name.as_str()).collect();
        for mesh in ["head-ear-l", "head-ear-r", "head-ear-l-inner", "head-ear-r-inner"] {
            assert!(names.contains(&mesh), "{species}: no {mesh} in {names:?}");
        }
        let anchors: Vec<&str> = built.anchors.keys().map(|(_, a)| a.as_str()).collect();
        assert!(anchors.contains(&"ear_top_l") && anchors.contains(&"ear_top_r"), "{species}: {anchors:?}");
        assert!(!anchors.contains(&"ear_l"), "{species}: animal ears are not on the side of the head");
        let top = built.anchors.iter().find(|((_, a), _)| a.as_str() == "ear_top_l").unwrap().1;
        let head_top = built.anchors.iter().find(|((_, a), _)| a.as_str() == "top").unwrap().1;
        assert!(top.position.y > head_top.position.y - 0.02, "{species}: the tip stands about the top of the head");
    }
}

#[test]
fn the_rabbits_chain_validates_against_the_built_skeleton() {
    let (_, built) = kit_with("rabbit");
    built.rig.springs.validate(&built.rig.skeleton).unwrap();
    let chains: Vec<_> = built.rig.springs.chains.iter().filter(|c| c.name.starts_with("ear_head_")).collect();
    assert_eq!(chains.len(), 2, "one chain per ear");
    assert!(chains.iter().all(|c| c.joints.len() == 3));
    let bones = built.rig.skeleton.bones().iter().filter(|b| b.name.starts_with("ear_head_")).count();
    assert_eq!(bones, 6);
    let (_, cat) = kit_with("cat");
    assert!(cat.rig.springs.chains.iter().all(|c| !c.name.starts_with("ear_")), "a cat's ears are rigid");
}

#[test]
fn a_recoloured_hair_slot_recolours_the_fur() {
    let (mut doc, built) = kit_with("fox");
    let fur = |b: &BuiltCharacter| {
        b.materials.iter().find(|(n, _)| n.as_str() == "head-fur-ear").map(|(_, m)| m.base).unwrap()
    };
    let before = fur(&built);
    common::apply(
        &mut doc,
        "hair.recolour",
        json!({"character": "kit", "part": "hair", "root": "#3a2a4a", "mid": "#6a5a8a", "tip": "#a898c8"}),
    );
    let after = fur(&common::build(&doc, "kit"));
    println!("fur {before:?} -> {after:?}");
    assert_ne!(before, after);
    // The lit fur is the hair's mid colour (#6a5a8a) lifted an eighth toward white, as the hair is drawn.
    assert_eq!(after, omuzaki_core::colour::Colour::rgb(124, 110, 152), "the fur follows the hair's mid colour");
}

#[test]
fn part_describe_marks_the_animal_section_advanced_so_a_form_keeps_it_folded() {
    let (doc, _) = kit_with("cat");
    let answer = common::plan(&doc, "part.describe", json!({"character": "kit", "part": "head"})).unwrap();
    let parameters = answer.output.unwrap()["parameters"].as_array().unwrap().clone();
    let level = |path: &str| {
        parameters.iter().find(|p| p["path"] == path).unwrap_or_else(|| panic!("no {path}")).get("level").cloned()
    };
    assert_eq!(level("ears/animal"), Some(json!("advanced")));
    assert_eq!(level("ears"), None, "the ears' own settings stay in view");
}
