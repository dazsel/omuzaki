//! The acceptance checks: the hash properties, the named problems, scene refusal, the worked recipes of
//! design 4.1, 4.3 and 4.9, and a snapshot of the public names (a compile-time list: it breaks when a public item
//! is renamed or removed).

#![allow(clippy::unwrap_used, clippy::float_cmp)] // test helpers outside #[test] fns; hash equality is exact by design

use std::collections::BTreeMap;

use omuzaki_char_api::{CharId, PartId};
use omuzaki_identity_api::key::KeyId;
use omuzaki_library_api::rights::{AssetInfo, Attestation};
use omuzaki_library_api::{
    Bindings, ItemManifest, ItemRights, Params, Problem, RecipeError, StepRequest, check_ops, expand,
};
use omuzaki_ops_api::{OpKind, OpMeta, Scope, Undo};
use omuzaki_store_api::ContentHash;
use proptest::prelude::*;
use serde_json::{Value, json};

/// Design 4.1, the body shape (adapter recipe), with the first-party boilerplate the design leaves out.
fn curvy() -> String {
    r#"
schema = 1
id = "omuzaki/body-shapes/curvy"
version = "1.0.0"
section = "body-shapes"
kind = "setting"
target = "character"
slot = "body-shape"
title = "Curvy"
summary = "Full bust and hips with a defined waist, wider below than above."
origin = "body-shape preset curvy in crates/omuzaki-char/library/body-shapes"
compat = { kinds = ["humanoid"], templates = ["*"] }
params.blend = { type = "number", default = 1.0, min = 0.0, max = 1.0, summary = "How far to move from the current shape" }

[rights]
author = "Omuzaki"
licence = "CC0-1.0"
commercial = "ok"
owned = true

[[step]]
op = "body.apply_shape_preset"
args = { character = "$character", preset = "curvy", blend = "$params.blend" }
"#
    .to_owned()
}

fn twintails() -> String {
    r#"
schema = 1
id = "omuzaki/hair/twintails"
version = "1.0.0"
section = "hair"
kind = "part"
target = "character"
slot = "hairstyle"
title = "Twintails"
summary = "Two high tails tied above the ears, with a light fringe."
origin = "generator defaults"
compat = { kinds = ["humanoid"], templates = ["*"], requires = ["head"] }
creates.hair = { kind = "hair", id = "hair", layer = 5 }
params.tail_length = { type = "number", default = 2.2, min = 0.8, max = 3.5, summary = "Tail length in head heights" }

[rights]
author = "Omuzaki"
licence = "CC0-1.0"
commercial = "ok"
owned = true

[[step]]
op = "part.add"
[step.args]
character = "$character"
kind = "hair"
id = "$parts.hair"
layer = 5
[step.args.params.groups.cap.generator.cap]
hairline_front_deg = 18.0
thickness = 0.012
[step.args.params.groups.tails]
mirror = true
generator.tail = { anchor = "crown_side", length = "$params.tail_length" }
"#
    .to_owned()
}

fn uniform() -> String {
    r#"
schema = 1
id = "omuzaki/outfits/school-uniform"
version = "1.0.0"
section = "outfits"
kind = "bundle"
target = "character"
slot = "outfit"
title = "School uniform"
summary = "A shirt, a pleated skirt and piping."
origin = "composition of first-party garments"

[rights]
author = "Omuzaki"
licence = "CC0-1.0"
commercial = "ok"
owned = true

[[step]]
item = "omuzaki/garments/shirt"
version = "^1"
[[step]]
item = "omuzaki/garments/pleated-skirt"
version = "^1"
params = { length = 0.8 }
[[step]]
item = "omuzaki/trims/piping"
version = "^1"
bind = { target = "$parts.skirt" }
"#
    .to_owned()
}

fn bindings() -> Bindings {
    Bindings { character: Some(CharId::new("mia").unwrap()), ..Bindings::default() }
}

fn problems_of(manifest: &str) -> Vec<Problem> {
    ItemManifest::parse(manifest).unwrap().check()
}

fn fields(problems: &[Problem]) -> Vec<&str> {
    problems.iter().map(|p| p.field.as_str()).collect()
}

#[test]
fn the_worked_manifests_check_clean() {
    for text in [curvy(), twintails(), uniform()] {
        let manifest = ItemManifest::parse(&text).unwrap();
        assert_eq!(manifest.check(), [], "{}", manifest.id);
    }
}

#[test]
fn check_names_the_field_of_each_mistake() {
    let t = twintails();
    let cases: [(&str, String, &str); 8] = [
        ("a bad id", t.replace("id = \"omuzaki/hair/twintails\"", "id = \"Omuzaki/Hair\""), "id"),
        ("an unknown section", t.replace("section = \"hair\"", "section = \"hairs\""), "section"),
        (
            "a part with no creates",
            t.replace("creates.hair = { kind = \"hair\", id = \"hair\", layer = 5 }", ""),
            "kind",
        ),
        ("a setting that creates", t.replace("kind = \"part\"", "kind = \"setting\""), "kind"),
        (
            "a placeholder inside a longer string",
            t.replace("id = \"$parts.hair\"", "id = \"x-$parts.hair\""),
            "step/0/args/id",
        ),
        (
            "a step naming another character",
            t.replace("character = \"$character\"", "character = \"kai\""),
            "step/0/args/character",
        ),
        (
            "an undeclared param",
            t.replace("length = \"$params.tail_length\"", "length = \"$params.tail_len\""),
            "step/0/args/params/groups/tails/generator/tail/length",
        ),
        ("a $parts key with no creates", t.replace("id = \"$parts.hair\"", "id = \"$parts.fringe\""), "step/0/args/id"),
    ];
    for (what, text, field) in cases {
        let problems = problems_of(&text);
        assert!(fields(&problems).contains(&field), "{what}: wanted {field}, got {problems:?}");
    }
    let undeclared = problems_of(&t.replace("length = \"$params.tail_length\"", "length = \"$params.tail_len\""));
    assert!(undeclared[0].message.contains("$params.tail_len"), "{undeclared:?}");
}

#[test]
fn the_scene_target_is_refused_with_the_reserved_message() {
    let rig = r#"
schema = 1
id = "omuzaki/light-rigs/candle"
version = "1.0.0"
section = "light-rigs"
kind = "setting"
target = "scene"
title = "Candle"
summary = "A warm flickering key."
origin = "design target"
[rights]
author = "Omuzaki"
licence = "CC0-1.0"
commercial = "ok"
owned = true
[[step]]
op = "stage.set_light_rig"
args = { scene = "$scene", rig = "viewer-candle" }
"#;
    let problems = problems_of(rig);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].field, "target");
    assert_eq!(problems[0].message, "the scene target is reserved until the ST-01 scene contract");
    let manifest = ItemManifest::parse(rig).unwrap();
    assert!(matches!(expand(&manifest, &bindings(), &Params::new()), Err(RecipeError::Reserved(_))));
}

#[test]
fn first_party_items_need_the_constant_rights_and_an_origin() {
    let t = twintails();
    let problems = problems_of(
        &t.replace("licence = \"CC0-1.0\"", "licence = \"CC-BY-4.0\"").replace("origin = \"generator defaults\"", ""),
    );
    assert_eq!(fields(&problems), ["origin", "rights"]);
}

fn edit_meta(name: &'static str, kind: OpKind, scope: Scope) -> OpMeta {
    OpMeta { name, summary: "", kind, scope, undo: Undo::Auto }
}

#[test]
fn check_ops_enforces_rules_1_and_3() {
    let manifest = ItemManifest::parse(&twintails()).unwrap();
    let known = |kind, scope| move |op: &str| (op == "part.add").then(|| edit_meta("part.add", kind, scope));
    assert_eq!(check_ops(&manifest, known(OpKind::Edit, Scope::EditCharacter)), []);
    assert_eq!(check_ops(&manifest, known(OpKind::Edit, Scope::EditScene)), []);
    let query = check_ops(&manifest, known(OpKind::Query, Scope::Read));
    assert_eq!((query[0].field.as_str(), query[0].message.contains("edit ops")), ("step/0/op", true));
    let export = check_ops(&manifest, known(OpKind::Edit, Scope::Publish));
    assert!(export[0].message.contains("publish"), "{export:?}");
    let missing = check_ops(&manifest, |_| None);
    assert!(missing[0].message.contains("`part.add` is not a registered op"), "{missing:?}");
}

#[test]
fn expand_recipe_4_1_equals_the_hand_written_request() {
    let manifest = ItemManifest::parse(&curvy()).unwrap();
    let mut params = Params::new();
    assert_eq!(
        expand(&manifest, &bindings(), &params).unwrap(),
        [StepRequest::Op {
            index: 0,
            op: "body.apply_shape_preset".into(),
            args: json!({"character": "mia", "preset": "curvy", "blend": 1.0}),
        }],
        "the default fills in, with its type"
    );
    params.insert("blend".into(), json!(0.25));
    let StepRequest::Op { args, .. } = &expand(&manifest, &bindings(), &params).unwrap()[0] else { panic!("op step") };
    assert_eq!(args["blend"], json!(0.25));
    params.insert("blend".into(), json!(1.5));
    assert!(matches!(expand(&manifest, &bindings(), &params), Err(RecipeError::Param { key, .. }) if key == "blend"));
    assert!(matches!(
        expand(&manifest, &bindings(), &[("nope".to_owned(), json!(1))].into()),
        Err(RecipeError::Param { .. })
    ));
}

#[test]
fn expand_recipe_4_3_equals_the_hand_written_request() {
    let manifest = ItemManifest::parse(&twintails()).unwrap();
    let bound = Bindings { parts: [("hair".to_owned(), PartId::new("hair-2").unwrap())].into(), ..bindings() };
    let expected = json!({
        "character": "mia", "kind": "hair", "id": "hair-2", "layer": 5,
        "params": {"groups": {
            "cap": {"generator": {"cap": {"hairline_front_deg": 18.0, "thickness": 0.012}}},
            "tails": {"mirror": true, "generator": {"tail": {"anchor": "crown_side", "length": 2.2}}}
        }}
    });
    assert_eq!(
        expand(&manifest, &bound, &Params::new()).unwrap(),
        [StepRequest::Op { index: 0, op: "part.add".into(), args: expected }]
    );
    let unbound = expand(&manifest, &bindings(), &Params::new());
    assert!(matches!(unbound, Err(RecipeError::Unbound { placeholder, .. }) if placeholder == "$parts.hair"));
}

#[test]
fn expand_recipe_4_9_equals_the_hand_written_requests() {
    let manifest = ItemManifest::parse(&uniform()).unwrap();
    let bound = Bindings { parts: [("skirt".to_owned(), PartId::new("skirt").unwrap())].into(), ..bindings() };
    let steps = expand(&manifest, &bound, &Params::new()).unwrap();
    let item = |s: &str| s.parse().unwrap();
    let caret = |s: &str| s.parse().unwrap();
    assert_eq!(
        steps,
        [
            StepRequest::Item {
                index: 0,
                item: item("omuzaki/garments/shirt"),
                version: caret("^1"),
                params: Params::new(),
                bind: BTreeMap::default()
            },
            StepRequest::Item {
                index: 1,
                item: item("omuzaki/garments/pleated-skirt"),
                version: caret("^1"),
                params: [("length".to_owned(), json!(0.8))].into(),
                bind: BTreeMap::default()
            },
            StepRequest::Item {
                index: 2,
                item: item("omuzaki/trims/piping"),
                version: caret("^1"),
                params: Params::new(),
                bind: [("target".to_owned(), "skirt".to_owned())].into()
            },
        ]
    );
}

#[test]
fn expand_enforces_confinement_escapes_and_template_guards() {
    let guarded = r##"
schema = 1
id = "test/guarded"
version = "1.0.0"
section = "colourways"
kind = "setting"
target = "character"
title = "G"
summary = "G."
[rights]
licence = "CC0-1.0"
commercial = "ok"
[[step]]
op = "palette.set"
args = { character = "$character", slot = "$$trim", colour = "#112233" }
[[step]]
op = "palette.set"
when = { templates = ["mini"] }
args = { character = "$character", slot = "eyes", colour = "#445566" }
"##;
    let manifest = ItemManifest::parse(guarded).unwrap();
    assert_eq!(manifest.check(), []);
    let on = |template: &str| Bindings { template: Some(template.parse().unwrap()), ..bindings() };
    let tall = expand(&manifest, &on("tall"), &Params::new()).unwrap();
    assert_eq!(tall.len(), 1, "the guarded step is left out");
    let StepRequest::Op { args, .. } = &tall[0] else { panic!("op step") };
    assert_eq!(args["slot"], json!("$trim"), "$$ is a literal $");
    assert_eq!(expand(&manifest, &on("mini"), &Params::new()).unwrap().len(), 2);
    assert!(
        matches!(expand(&manifest, &bindings(), &Params::new()), Err(RecipeError::Unbound { .. })),
        "a guard needs the template"
    );

    let hijack = ItemManifest::parse(
        &guarded.replace("character = \"$character\", slot = \"$$trim\"", "character = \"kai\", slot = \"x\""),
    )
    .unwrap();
    assert!(
        matches!(expand(&hijack, &on("tall"), &Params::new()), Err(RecipeError::NotTheTarget { field, .. }) if field == "step/0/args/character")
    );
}

/// The manifest with `hairline_front_deg` replaced by `value`, as TOML text.
fn with_angle(value: f64) -> String {
    twintails().replace("hairline_front_deg = 18.0", &format!("hairline_front_deg = {value:?}"))
}

proptest! {
    #[test]
    fn the_hash_ignores_reformatting_and_comments(spaces in 0usize..4, comments in prop::collection::vec("[a-z ]{0,12}", 0..4)) {
        let base = ItemManifest::parse(&twintails()).unwrap();
        let pad = " ".repeat(spaces);
        let mut text = twintails().replace(" = ", &format!("{pad}={pad}"));
        for comment in &comments {
            text = format!("# {comment}\n{text}\n# {comment}\n");
        }
        let reformatted = ItemManifest::parse(&text).unwrap();
        prop_assert_eq!(base.hash(), reformatted.hash());
        prop_assert_eq!(base.canonical_bytes(), reformatted.canonical_bytes());
    }

    #[test]
    fn the_hash_changes_with_any_value(a in -1000.0f64..1000.0, b in -1000.0f64..1000.0) {
        let (one, two) = (ItemManifest::parse(&with_angle(a)).unwrap(), ItemManifest::parse(&with_angle(b)).unwrap());
        prop_assert_eq!(one.hash() == two.hash(), a == b);
    }

    #[test]
    fn every_string_field_is_covered_by_the_hash(extra in "[a-z]{1,8}") {
        let base = ItemManifest::parse(&twintails()).unwrap();
        for edit in [
            |m: &mut ItemManifest, s: &str| m.title.push_str(s),
            |m: &mut ItemManifest, s: &str| m.summary.push_str(s),
            |m: &mut ItemManifest, s: &str| m.tags.push(s.to_owned()),
            |m: &mut ItemManifest, s: &str| m.origin = Some(s.to_owned()),
            |m: &mut ItemManifest, s: &str| m.version = format!("1.0.{}", s.len()),
        ] {
            let mut changed = base.clone();
            edit(&mut changed, &extra);
            prop_assert_ne!(base.hash(), changed.hash());
        }
    }
}

#[test]
fn canonical_json_is_plain_json_with_sorted_keys() {
    let manifest = ItemManifest::parse(&curvy()).unwrap();
    let parsed: Value = serde_json::from_slice(&manifest.canonical_bytes()).unwrap();
    assert_eq!(parsed["step"][0]["args"]["preset"], json!("curvy"));
    let text = String::from_utf8(manifest.canonical_bytes()).unwrap();
    let positions: Vec<usize> = ["\"compat\"", "\"id\"", "\"kind\"", "\"rights\"", "\"step\"", "\"title\""]
        .iter()
        .map(|k| text.find(k).unwrap())
        .collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "{text}");
}

/// The hash of `twintails()` before the marketplace fields existed: a manifest without them must keep it.
/// A real key value for the attesting asserter (never a display name).
const THEM: [u8; 32] = [9; 32];

const TWINTAILS_HASH: &str = "81a9390005eabaf68a5dd699bbcd0c29a2f8aaf93d65955f78daa96bf8a79a70";

#[test]
fn a_manifest_without_the_marketplace_fields_keeps_its_hash_and_bytes() {
    let manifest = ItemManifest::parse(&twintails()).unwrap();
    assert_eq!(manifest.hash().to_string(), TWINTAILS_HASH);
    let text = String::from_utf8(manifest.canonical_bytes()).unwrap();
    for absent in ["asset_info", "attestation", "terms_hash"] {
        assert!(!text.contains(absent), "{absent} is skipped when absent: {text}");
    }
}

/// `twintails()` with a binary asset, declared with the marketplace fields.
fn with_asset(info: &str) -> String {
    let hash = ContentHash::of(b"binary");
    let rights = "[rights]\nauthor = \"Omuzaki\"";
    twintails()
        .replace("origin = \"generator defaults\"\n", "")
        .replace("omuzaki/hair/twintails", "someone/hair/twintails")
        .replace(rights, &format!("assets.cap = \"{hash}\"\n{info}\n{rights}"))
}

#[test]
fn asset_info_round_trips_with_the_items_own_rights_type() {
    let info = "[asset_info.cap]\nsize = 1024\n[asset_info.cap.rights]\nlicence = \"CC-BY-4.0\"\ncommercial = \"ok\"\n\
                terms_hash = \"\"\n";
    // `terms_hash` must be a real hash: an empty one is refused rather than ignored.
    assert!(ItemManifest::parse(&with_asset(info)).is_err());
    let hash = ContentHash::of(b"terms");
    let info = info.replace(
        "terms_hash = \"\"",
        &format!(
            "terms_hash = \"{hash}\"\n[asset_info.cap.rights.attestation]\nby = \"{}\"\nclaim = \"own-work\"",
            KeyId(THEM).to_hex()
        ),
    );
    let manifest = ItemManifest::parse(&with_asset(&info)).unwrap();
    let AssetInfo { size, rights } = &manifest.asset_info["cap"];
    assert_eq!(*size, 1024);
    let expected = ItemRights {
        licence: "CC-BY-4.0".into(),
        author: None,
        owned: false,
        terms_hash: Some(hash),
        attestation: Some(Attestation { by: KeyId(THEM), claim: "own-work".into() }),
        ..ItemRights::first_party()
    };
    assert_eq!(rights, &expected);
    assert_eq!(fields(&problems_of(&with_asset(&info))), Vec::<&str>::new());
    let reparsed: ItemManifest = serde_json::from_slice(&manifest.canonical_bytes()).unwrap();
    assert_eq!(reparsed.hash(), manifest.hash(), "the new fields are part of the hash and survive canonical JSON");
}

#[test]
fn asset_info_has_exactly_the_keys_of_assets() {
    let entry = "[asset_info.{}]\nsize = 1\n[asset_info.{}.rights]\nlicence = \"CC0-1.0\"\ncommercial = \"ok\"\n";
    let both = format!("{}{}", entry.replace("{}", "cap"), entry.replace("{}", "extra"));
    assert_eq!(fields(&problems_of(&with_asset(&both))), ["asset_info/extra"], "an entry for nothing");
    let none_for_cap = entry.replace("{}", "extra");
    let mut fields_found =
        fields(&problems_of(&with_asset(&none_for_cap))).iter().map(|f| (*f).to_owned()).collect::<Vec<_>>();
    fields_found.sort();
    assert_eq!(fields_found, ["asset_info/cap", "asset_info/extra"], "an asset with no entry");
    assert_eq!(
        fields(&problems_of(&with_asset(""))),
        Vec::<&str>::new(),
        "manifests from before asset_info stay valid"
    );
}

#[test]
fn unknown_fields_are_still_refused_in_the_new_types() {
    let info =
        "[asset_info.cap]\nsize = 1\nextra = 2\n[asset_info.cap.rights]\nlicence = \"CC0-1.0\"\ncommercial = \"ok\"\n";
    assert!(ItemManifest::parse(&with_asset(info)).is_err());
    let rights = twintails().replace("owned = true", "owned = true\nterms_hash_typo = 1");
    assert!(ItemManifest::parse(&rights).is_err());
}

/// A compile-time list of the public names other crates build on. Removing or renaming any of them breaks it.
#[test]
fn the_public_api_snapshot() {
    #[allow(unused_imports)]
    use omuzaki_library_api::rights::{AssetInfo, Attestation};
    #[allow(unused_imports)]
    use omuzaki_library_api::rights_join::{
        AXES, Axis, JoinedRights, JointConsent, RightsRow, UNKNOWN_LICENCE, commercial_rank, exceeding, join_rights,
        join_terms,
    };
    #[allow(unused_imports)]
    use omuzaki_library_api::{
        APPLIED_KEY, ApplicationRecord, Bindings, BodyKind, Compat, CompatStatus, CompatTarget, Creates, Dep,
        FIRST_PARTY_LICENCE, FIRST_PARTY_PUBLISHER, FIXTURE_PUBLISHER, Facets, Framing, IdError, ItemDetail, ItemId,
        ItemInfo, ItemKind, ItemManifest, ItemRights, ItemScope, ItemSource, ItemStep, Library, LibraryError, Migrate,
        OMUZAKI_PUBLISHER_KEY, OpStep, Origin, ParamSpec, ParamType, Params, ParseError, PreviewSpec, Problem,
        Publisher, PublisherCheck, PublisherKey, Query, RECORD_SCHEMA, RecipeError, RecordBindings, RecordError,
        Resolved, ResolvedItem, RightsBadge, SearchPage, Section, SourceRegistration, Step, StepRequest, Target, Turn,
        UnknownSection, Usage, Version, VersionError, VersionReq, When, check_ops, expand, migrations, record_path,
        sources,
    };
    assert_eq!(migrations::CURRENT_SCHEMA, 1);
    assert_eq!(Section::ALL.len(), 25);
}
