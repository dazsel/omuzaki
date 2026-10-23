//! Where the default head sits on each template's neck (#825, #960): a head built with no `cranium.centre` of its own is
//! seated on the template it is built on, so it meets the neck on all four templates as it does on soft-sd, and rides
//! the neck when the body's sliders move it. A centre written by hand is taken as written, on every template, even when
//! it equals the old default.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, Cranium, HeadParams, PartDef,
    PartId, PartKindId, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use serde_json::{Value, json};

// Naming the crate links its part kinds (body, head) into the test binary.
use omuzaki_char::template::registry;

const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

/// How far the default head's lowest point stands above the neck's top ring, in head units: the chin overlaps the neck
/// by this much on every template.
const SEAT: f64 = -0.046;

fn part(kind: &str, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The real body with `body` parameters and a head with `head` parameters, on `template`.
fn build_with(template: &str, body: Value, head: Value) -> BuiltCharacter {
    let mut def: CharacterDef = mock_character().unwrap();
    def.template = template.parse().unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", body));
    def.parts.insert(PartId::new("head").unwrap(), part("head", head));
    RegistryBuilder
        .build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap_or_else(|e| panic!("{template}: {e}"))
}

fn build(template: &str, head: Value) -> BuiltCharacter {
    build_with(template, json!({}), head)
}

/// The lowest (`f32::min`) or highest (`f32::max`) y of the mesh called `name`.
fn extreme(built: &BuiltCharacter, name: &str, pick: fn(f32, f32) -> f32, from: f32) -> f32 {
    let mesh = built.meshes.iter().find(|m| m.mesh.name == name).unwrap_or_else(|| panic!("no {name} mesh"));
    mesh.mesh.geometry.positions.iter().map(|p| p.y).fold(from, pick)
}

/// How far the head's lowest point stands above the neck's top ring, in the template's authored units (the figure is
/// built in metres, so the stature scale is divided out). Negative: the chin overlaps the neck.
fn gap(built: &BuiltCharacter) -> f64 {
    let head_bottom = extreme(built, "head-head", f32::min, f32::INFINITY);
    let neck_top = extreme(built, "head-neck", f32::max, f32::NEG_INFINITY);
    let scale = built.stature.map_or(1.0, |s| f64::from(s.scale));
    f64::from(head_bottom - neck_top) / scale
}

#[test]
fn the_default_head_meets_the_neck_the_same_way_on_every_template() {
    assert_eq!(registry::templates().len(), TEMPLATES.len(), "a new template joins this list");
    for template in TEMPLATES {
        let g = gap(&build(template, json!({})));
        eprintln!("{template}: gap {g:.5}");
        assert!((g - SEAT).abs() < 1e-3, "{template}: the head's gap to the neck is {g}, not {SEAT}");
        // The template's own default head (what `character.create` writes) sits the same way, lowered by as much more as
        // its neck drops than the default head's.
        let defaults = registry::find(template).unwrap().defaults().head;
        let g = gap(&build(template, serde_json::to_value(&defaults).unwrap()));
        let expected = SEAT - (defaults.neck.drop - HeadParams::default().neck.drop);
        assert!((g - expected).abs() < 1e-3, "{template}: the default head's gap is {g}, not {expected}");
    }
}

#[test]
fn the_default_head_rides_the_neck_when_the_neck_is_lengthened() {
    for template in TEMPLATES {
        let long = build_with(template, json!({ "proportions": { "neck_length": 1.3 } }), json!({}));
        let g = gap(&long);
        assert!((g - SEAT).abs() < 1e-3, "{template}: with a longer neck the gap is {g}, not {SEAT}");
    }
}

#[test]
fn a_written_centre_is_taken_as_written_even_when_it_is_the_old_default() {
    let soft_sd = build("soft-sd", json!({}));
    for template in TEMPLATES {
        // The old default, written by hand: no longer a stand-in for "unset", so the head stays exactly there.
        let written = build(template, json!({ "cranium": { "centre": Cranium::DEFAULT_CENTRE } }));
        let (lowest, soft_lowest) = (
            extreme(&written, "head-head", f32::min, f32::INFINITY) / written.stature.map_or(1.0, |s| s.scale),
            extreme(&soft_sd, "head-head", f32::min, f32::INFINITY) / soft_sd.stature.map_or(1.0, |s| s.scale),
        );
        assert!((lowest - soft_lowest).abs() < 1e-4, "{template}: the chin is at {lowest}, soft-sd's at {soft_lowest}");
        // Raised by 0.1 from its own seat, the gap grows by 0.1.
        let [x, y, z] = Cranium::DEFAULT_CENTRE;
        let seat_y = y + (registry_head_base(template) - registry_head_base("soft-sd"));
        let raised = build(template, json!({ "cranium": { "centre": [x, seat_y + 0.1, z] } }));
        let g = gap(&raised);
        assert!((g - (SEAT + 0.1)).abs() < 1e-3, "{template}: raised by 0.1 the gap is {g}");
    }
}

/// The y of `template`'s head base at neutral sliders.
fn registry_head_base(template: &str) -> f64 {
    let body = omuzaki_char_api::BodyParams::default();
    let layout = registry::find(template).unwrap().layout(&body.proportions, &body.shape, &body.hand);
    layout.head_base().unwrap().y
}

/// Writes each template's body, neck and default head as an OBJ for `tools/head-placement-sheet.py`: `<template>-after`
/// as the head part seats it, `<template>-before` with the head written at soft-sd's centre, where every template put
/// it before #825.
/// `OMUZAKI_SHEET_DIR=<dir> cargo test -p omuzaki-char --test head_placement -- --ignored dump`.
#[test]
#[ignore = "writes the sheets' meshes to $OMUZAKI_SHEET_DIR"]
#[allow(clippy::disallowed_methods, reason = "a developer tool writing to a directory the person named, not an op")]
fn dump() {
    use std::fmt::Write as _;

    let dir =
        std::path::PathBuf::from(std::env::var("OMUZAKI_SHEET_DIR").expect("set OMUZAKI_SHEET_DIR to a directory"));
    std::fs::create_dir_all(&dir).unwrap();
    let pinned = json!({ "cranium": { "centre": Cranium::DEFAULT_CENTRE } });
    for template in TEMPLATES {
        for (name, head) in [("before", pinned.clone()), ("after", json!({}))] {
            let built = build(template, head);
            let neck_top = extreme(&built, "head-neck", f32::max, f32::NEG_INFINITY);
            let mut obj = format!("# joint head 0 {neck_top} 0\n");
            let mut base = 1_usize;
            for m in &built.meshes {
                let g = &m.mesh.geometry;
                writeln!(obj, "g {}", m.mesh.name).unwrap();
                for p in &g.positions {
                    writeln!(obj, "v {} {} {}", p.x, p.y, p.z).unwrap();
                }
                for n in &g.normals {
                    writeln!(obj, "vn {} {} {}", n.x, n.y, n.z).unwrap();
                }
                for t in g.indices.chunks(3) {
                    let [a, b, c] = [t[0], t[1], t[2]].map(|i| i as usize + base);
                    writeln!(obj, "f {a}//{a} {b}//{b} {c}//{c}").unwrap();
                }
                base += g.positions.len();
            }
            std::fs::write(dir.join(format!("{template}-{name}.obj")), obj).unwrap();
        }
    }
}
