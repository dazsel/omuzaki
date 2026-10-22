//! The mock character through the public build pipeline, driven by hand the way the caching builder in `omuzaki-char` drives
//! it: `plan`, `build_part` and `Assembler`. It must equal `RegistryBuilder` (which is those pieces in a loop), give the
//! same errors, and merge a part served from a cache exactly like a freshly built one, the hair included: its simulation
//! is served from the cache by forking (`PartRuntime::fork`), so no part is rebuilt that an edit did not touch.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{
    Assembler, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, PartId,
    PartOutput, Published, Quality, RegistryBuilder, StepCtx, VariantId, build_part, plan, resolve_variant,
};
use omuzaki_char_mock::{RestPose, mock_character, mock_variants};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;
use serde_json::json;

fn char_id() -> CharId {
    CharId::new("mock").unwrap()
}

fn registry(def: &CharacterDef, options: &BuildOptions) -> Result<BuiltCharacter, BuildError> {
    RegistryBuilder.build(&char_id(), def, options, &BuildServices::default())
}

/// Builds the character the way a caching builder does: a part with an entry in `cache` is served from it, any other is
/// built. `cache` holds the outputs as a stage never saw them, simulations included; a hit hands out a fork of its
/// entry. Returns the character and the parts whose generator ran.
fn by_hand_with(
    def: &CharacterDef,
    options: &BuildOptions,
    cache: &mut Vec<(PartId, PartOutput)>,
) -> Result<(BuiltCharacter, Vec<PartId>), BuildError> {
    let (id, services) = (char_id(), BuildServices::default());
    let mut assembler = Assembler::new(&id, def, options);
    let (mut kept, mut ran) = (Vec::new(), Vec::new());
    for part in plan(def)? {
        let hit = cache.iter().position(|(cached, _)| *cached == part).map(|i| cache.swap_remove(i).1);
        let (output, entry) = if let Some(entry) = hit {
            (fork_output(&entry), entry)
        } else {
            ran.push(part.clone());
            let output = build_part(def, &part, options, &services, assembler.published())?;
            let entry = fork_output(&output);
            (output, entry)
        };
        kept.push((part.clone(), entry));
        assembler.add(&part, output)?;
    }
    *cache = kept;
    Ok((assembler.finish()?, ran))
}

fn by_hand(def: &CharacterDef, options: &BuildOptions) -> Result<BuiltCharacter, BuildError> {
    by_hand_with(def, options, &mut Vec::new()).map(|(built, _)| built)
}

/// A copy of an output, its simulation forked. What a cache keeps (a copy that never steps, because the original goes on
/// to the stage) and what it serves on a hit.
fn fork_output(out: &PartOutput) -> PartOutput {
    out.fork().expect("every mock simulation can be forked")
}

/// Everything a stage reads from a built character must be equal; the runtimes by what they declare.
fn assert_same(a: &BuiltCharacter, b: &BuiltCharacter) {
    assert_eq!(a.id, b.id);
    assert_eq!(a.variant, b.variant);
    assert_eq!(a.def, b.def);
    assert_eq!(a.palette, b.palette);
    assert_eq!(a.meshes, b.meshes);
    assert_eq!(a.materials, b.materials);
    assert_eq!(a.textures, b.textures);
    assert_eq!(a.face, b.face);
    assert_eq!(a.anchors, b.anchors);
    assert_eq!(a.rig, b.rig);
    assert_eq!(a.report, b.report);
    // The simulations by everything a stage reads: exact `Debug` text of the floats distinguishes every bit pattern a
    // simulation produces (its shortest round-trip form, `-0.0` included).
    let describe = |c: &BuiltCharacter| -> Vec<_> {
        c.runtimes
            .iter()
            .map(|r| {
                let meshes: Vec<_> = (0..r.runtime.dynamic_count()).map(|i| r.runtime.dynamic_mesh(i)).collect();
                (r.part.clone(), r.runtime.cost(), format!("{meshes:?}"))
            })
            .collect()
    };
    assert_eq!(describe(a), describe(b));
}

/// Steps every simulation of `built` through `frames` frames of a fixed, windy scene.
fn run(built: &mut BuiltCharacter, frames: u32) {
    for frame in 0..frames {
        let ctx = StepCtx::new(1.0 / 60.0, Seconds(f64::from(frame) / 60.0), &RestPose, Vec3::new(1.0, 0.0, 0.25));
        for slot in &mut built.runtimes {
            slot.runtime.step(&ctx);
        }
    }
}

#[test]
fn the_mock_character_by_hand_equals_the_registry_builder() {
    let (base, variants) = (mock_character().unwrap(), mock_variants().unwrap());
    let close_up = VariantId::new("close-up").unwrap();
    let cases = [
        (base.clone(), BuildOptions::default()),
        (base.clone(), BuildOptions { quality: Quality::Offline, ..BuildOptions::default() }),
        (base.clone(), BuildOptions { rig_export: true, ..BuildOptions::default() }),
        (
            resolve_variant(&base, &variants, &close_up).unwrap(),
            BuildOptions { variant: Some(close_up), ..BuildOptions::default() },
        ),
    ];
    for (def, options) in &cases {
        let (a, b) = (registry(def, options).unwrap(), by_hand(def, options).unwrap());
        assert!(!a.meshes.is_empty() && !a.runtimes.is_empty(), "a real character, not an empty one");
        assert_same(&a, &b);
    }
}

#[test]
fn a_broken_part_gives_the_registry_builders_error() {
    let options = BuildOptions::default();
    let mut bad_params = mock_character().unwrap();
    bad_params.parts.get_mut(&PartId::new("hair").unwrap()).unwrap().params = json!({"bogus": 1});
    let mut no_head = mock_character().unwrap();
    no_head.parts.remove(&PartId::new("head").unwrap());
    let mut unknown_kind = mock_character().unwrap();
    unknown_kind.parts.get_mut(&PartId::new("vest").unwrap()).unwrap().kind = "no-such-kind".parse().unwrap();

    for def in [bad_params, no_head, unknown_kind] {
        let (a, b) = (registry(&def, &options).unwrap_err(), by_hand(&def, &options).unwrap_err());
        assert_eq!(a, b);
    }
}

#[test]
fn an_edited_character_merges_cached_parts_exactly_like_fresh_ones() {
    let options = BuildOptions::default();
    let before = mock_character().unwrap();
    let mut cache = Vec::new();
    let (first, ran) = by_hand_with(&before, &options, &mut cache).unwrap();
    assert_same(&first, &registry(&before, &options).unwrap());
    assert_eq!(ran.len(), 5, "nothing is cached yet");

    // Edit the vest, the outermost garment: every other part, the simulated hair included, is served from the cache.
    let mut after = before;
    after.parts.get_mut(&PartId::new("vest").unwrap()).unwrap().params = json!({"ease": 0.01, "colour": "@outfit"});
    cache.retain(|(part, _)| part.as_str() != "vest");
    let cached_parts = cache.len();
    let (mut second, ran) = by_hand_with(&after, &options, &mut cache).unwrap();

    assert_eq!(cached_parts, 4, "body, head, shirt and hair are served from the cache");
    assert_eq!(ran, [PartId::new("vest").unwrap()], "only the edited part's generator ran: the hair was not rebuilt");
    let mut reference = registry(&after, &options).unwrap();
    assert_same(&second, &reference);
    assert_ne!(first.meshes, second.meshes, "the edit did change the result");

    // The hair served from the cache simulates exactly like the one the reference builder just built.
    assert_eq!(second.runtimes.len(), 1);
    run(&mut second, 90);
    run(&mut reference, 90);
    assert_same(&second, &reference);
}

#[test]
fn a_cache_serves_the_same_simulation_again_and_again() {
    // A hit forks the entry; the entry itself never steps, so the tenth build gets the same fresh hair as the first,
    // however far the characters built from it have run.
    let (options, def) = (BuildOptions::default(), mock_character().unwrap());
    let mut cache = Vec::new();
    let (mut first, _) = by_hand_with(&def, &options, &mut cache).unwrap();
    run(&mut first, 30);
    for _ in 0..3 {
        let (mut served, ran) = by_hand_with(&def, &options, &mut cache).unwrap();
        assert!(ran.is_empty(), "everything is cached");
        assert_same(&served, &registry(&def, &options).unwrap());
        run(&mut served, 30);
        assert_same(&served, &first);
    }
}

#[test]
fn a_forked_output_merges_like_the_original() {
    // `PartOutput::fork` is what a cache keeps and serves. A fork of every part, simulations included, assembles into the
    // same character as the originals, and the forked simulation starts in the generator's initial state, so it steps
    // exactly like the original does.
    let (def, options, services) = (mock_character().unwrap(), BuildOptions::default(), BuildServices::default());
    let (id, order) = (char_id(), plan(&def).unwrap());
    let (mut original, mut forked) = (Assembler::new(&id, &def, &options), Assembler::new(&id, &def, &options));
    for part in &order {
        let output = build_part(&def, part, &options, &services, original.published()).unwrap();
        let fork = output.fork().expect("every mock simulation can be forked");
        forked.add(part, fork).unwrap();
        original.add(part, output).unwrap();
    }
    let (mut built, mut merged) = (original.finish().unwrap(), forked.finish().unwrap());
    assert_eq!(merged.runtimes.len(), 1, "the hair simulation is forked, not dropped");
    assert_same(&merged, &registry(&def, &options).unwrap());
    run(&mut built, 60);
    run(&mut merged, 60);
    assert_same(&merged, &built);
}

#[test]
fn a_part_is_a_pure_function_of_its_inputs_and_the_published_view() {
    // What a cache and a parallel builder rely on: the same part against a copy of the same published view gives the
    // same output, and against a different view (no head) it fails, so the view is a real input.
    let (def, options, services) = (mock_character().unwrap(), BuildOptions::default(), BuildServices::default());
    let (id, order) = (char_id(), plan(&def).unwrap());
    let mut assembler = Assembler::new(&id, &def, &options);
    let hair = PartId::new("hair").unwrap();
    for part in &order {
        let output = build_part(&def, part, &options, &services, assembler.published()).unwrap();
        if *part == hair {
            let alone_view = assembler.published().clone();
            let again = build_part(&def, part, &options, &services, &alone_view).unwrap();
            assert_eq!(again.meshes, output.meshes);
            assert_eq!(again.materials, output.materials);
            assert_eq!(again.warnings, output.warnings);
            assert_eq!(again.rig, output.rig);
            let empty = build_part(&def, part, &options, &services, &Published::default());
            assert!(matches!(empty, Err(BuildError::Missing { .. })), "without the head the hair cannot build");
        }
        assembler.add(part, output).unwrap();
    }
    assert_same(&assembler.finish().unwrap(), &registry(&def, &options).unwrap());
}
