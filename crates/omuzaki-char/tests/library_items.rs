//! The crate's library items (`library/items/`): every file parses, passes the manifest checks, names only registered
//! ops with arguments they accept, and, expanded the way the library applies it, runs on a fresh kit character (each
//! step keeping the editing contract) to a character that builds with what the item adds.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use omuzaki_char_api::{BodyTemplateId, CharId, PartId};
use omuzaki_doc_mock::MemDoc;
use omuzaki_library_api::{Bindings, ItemManifest, Params, StepRequest, check_ops, expand, sources};
use serde_json::json;

fn manifests() -> Vec<(&'static str, ItemManifest)> {
    omuzaki_char::link();
    omuzaki_char::library_items::files()
        .iter()
        .map(|(stem, text)| (*stem, ItemManifest::parse(text).unwrap_or_else(|e| panic!("{stem}: {e}"))))
        .collect()
}

#[test]
fn every_item_reads_and_passes_the_manifest_checks() {
    let all = manifests();
    assert!(all.len() >= 13, "the seeded items are all embedded");
    for (stem, manifest) in &all {
        assert!(manifest.check().is_empty(), "{stem}: {:?}", manifest.check());
        let problems = check_ops(manifest, |op| omuzaki_ops_api::find(op).map(|r| (r.meta)()));
        assert!(problems.is_empty(), "{stem}: {problems:?}");
    }
    let seed = sources().into_iter().find(|s| s.id() == "omuzaki-char").expect("the source is registered");
    assert_eq!(seed.items().len(), all.len(), "the source offers every file");
}

#[test]
fn every_item_applies_to_a_kit_character_and_builds() {
    for template in ["standard", "tall"] {
        for (stem, manifest) in manifests() {
            if !manifest.compat.templates.iter().any(|t| t == "*" || t == template) {
                continue;
            }
            let mut doc = MemDoc::default();
            common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": template}));
            let bindings = Bindings {
                character: Some(CharId::new("kit").unwrap()),
                parts: manifest
                    .creates
                    .iter()
                    .map(|(key, creates)| (key.clone(), PartId::new(&creates.id).unwrap()))
                    .collect::<BTreeMap<_, _>>(),
                target: manifest.target_part.first().map(|kind| PartId::new(kind).unwrap()),
                name: None,
                template: Some(BodyTemplateId::new(template).unwrap()),
            };
            let steps = expand(&manifest, &bindings, &Params::new()).unwrap_or_else(|e| panic!("{stem}: {e}"));
            assert!(!steps.is_empty(), "{stem}: a recipe with no steps");
            for step in steps {
                let StepRequest::Op { op, args, .. } = step else { panic!("{stem}: only op steps are seeded") };
                common::round_trip(&mut doc, &op, args);
            }
            let def = common::definition(&doc, "kit");
            for creates in manifest.creates.values() {
                let part = &def.parts[&PartId::new(&creates.id).unwrap()];
                assert_eq!(part.kind.as_str(), creates.kind, "{stem}: adds a {} part", creates.kind);
            }
            let built = common::build(&doc, "kit");
            let warnings: Vec<_> = built.report.parts.iter().flat_map(|p| p.warnings.iter()).collect();
            assert!(warnings.is_empty(), "{stem} on {template}: {warnings:?}");
        }
    }
}
