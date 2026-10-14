//! Keeps the design doc's tables honest (see `omuzaki-face-api/tests/design_doc.rs`): a preset id, hair generator or
//! hair-program name the code has and the doc lacks fails here.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{
    BangsGen, BraidGen, BrushFalloff, BunGen, CapGen, CurtainGen, FramingGen, Generator, GuidesGen, HAIR_INFO,
    HAIR_PROGRAM, HAIR_UV, HairShading, PRESET_IDS, PartSweepGen, TailGen,
};
use omuzaki_face_api::Palette;
use omuzaki_render_api::Shading;

const DOC: &str = include_str!("../../../docs/design/char-face-api.md");

fn assert_listed(what: &str, name: &str) {
    assert!(DOC.contains(name), "the design doc doesn't mention {what} `{name}`: add it to the tables");
}

#[test]
fn preset_ids_generators_and_the_hair_program_are_documented() {
    for id in PRESET_IDS {
        assert_listed("shape preset", id);
    }
    let generators = [
        Generator::Cap(CapGen::default()),
        Generator::PartSweep(PartSweepGen::default()),
        Generator::Framing(FramingGen::default()),
        Generator::Braid(BraidGen::default()),
        Generator::Curtain(CurtainGen::default()),
        Generator::Bangs(BangsGen::default()),
        Generator::Tail(TailGen::default()),
        Generator::Bun(BunGen::default()),
        Generator::Guides(GuidesGen::default()),
    ];
    for g in &generators {
        let tag = serde_json::to_value(g).unwrap()["type"].as_str().unwrap().to_owned();
        assert!(DOC.contains(&format!("`{tag}`")), "the hair generator `{tag}` has no row in the design doc");
    }
    for name in [HAIR_PROGRAM, HAIR_UV, HAIR_INFO] {
        assert_listed("hair contract name", name);
    }
    let Shading::Program { params, .. } = HairShading::default().shading(&Palette::default()).unwrap() else {
        panic!("a program")
    };
    for key in params.keys() {
        assert_listed("hair program parameter", key.as_str());
    }
}

#[test]
fn hair_streaks_inner_darkening_and_the_rng_switch_are_documented() {
    for name in ["Streak", "streaks", "inner_darkening", "tint", "HairRng", "LegacyLcg", "legacy_lcg", "forked"] {
        assert_listed("hair shading or rng name", name);
    }
}

#[test]
fn the_build_services_are_documented() {
    for name in [
        "BuildServices",
        "AssetSource",
        "AssetError",
        "NotFound",
        "Unavailable",
        "RasterizerFactory",
        "BuildCtx::asset",
        "BuildCtx::asset_or_fail",
        "BuildError::Asset",
        "BuildCtx::paint_texture",
        "FaceLayerSpec::for_rect",
        "MemAssets",
        "a 2D rasteriser",
    ] {
        assert_listed("build service item", name);
    }
}

#[test]
fn the_rig_data_contract_is_documented() {
    for name in [
        "RigContribution",
        "ExtraBone",
        "CharacterRig",
        "JointFit",
        "WeightBrush",
        "apply_brushes",
        "weight_brushes",
        "rig_export",
        "humanoid_id",
        "extra_id",
        "BodyFit::envelope",
        "BuildCtx::joint",
        "Published::joint",
        "<kind>_<part>",
    ] {
        assert_listed("rig item", name);
    }
    for falloff in [BrushFalloff::Constant, BrushFalloff::Linear, BrushFalloff::Smooth] {
        let tag = serde_json::to_value(falloff).unwrap();
        assert!(
            DOC.contains(&format!("`{}`", tag.as_str().unwrap())),
            "the brush falloff {tag} has no mention in the design doc"
        );
    }
    for error in [
        "Skeleton",
        "MissingParentBone",
        "DuplicateBone",
        "UnknownMeshBone",
        "UnknownRigBone",
        "DuplicateSpringName",
        "Springs",
        "Brush",
    ] {
        assert!(DOC.contains(&format!("| `{error}")), "BuildError::{error} has no row in the 'Rig data' error table");
    }
}

#[test]
fn the_build_pipeline_and_the_part_stream_are_documented() {
    for name in [
        "plan(",
        "build_part(",
        "part_rng(",
        "Assembler::new",
        "Assembler::published",
        "Assembler::add",
        "Assembler::finish",
        "check_published_joints",
        "check_published_colliders",
        "FNV-1a",
        "SplitMix64",
        "0xcbf29ce484222325",
        "0x100000001b3",
    ] {
        assert_listed("build pipeline item", name);
    }
}
