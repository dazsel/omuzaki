//! The limit-table engine (`omuzaki_io_ops::limits`): the `VRChat` numbers survived the move, a malformed table is a
//! clear error, and evaluation, findings and staleness behave as the tables say.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_io_api::Severity;
use omuzaki_io_ops::limits::{
    AvatarStats, LimitTable, STATS, StatTier, builtin_checked, builtin_ids, evaluate, findings, stale,
};

/// Every limit of the old `data/vrchat-performance.toml`, written out by hand per platform and stat (bounds split per
/// axis), so a table edit that changes a number fails here. Each row is a limit per tier.
type Golden = &'static [(&'static str, &'static [(&'static str, &'static [f64])])];
const OLD_NUMBERS: Golden = &[
    (
        "vrchat-pc",
        &[
            ("triangles", &[32000.0, 70000.0, 70000.0, 70000.0]),
            ("bounds_x", &[2.5, 4.0, 5.0, 5.0]),
            ("bounds_y", &[2.5, 4.0, 6.0, 6.0]),
            ("bounds_z", &[2.5, 4.0, 5.0, 5.0]),
            ("texture_mib", &[40.0, 75.0, 110.0, 150.0]),
            ("skinned_meshes", &[1.0, 2.0, 8.0, 16.0]),
            ("basic_meshes", &[4.0, 8.0, 16.0, 24.0]),
            ("material_slots", &[4.0, 8.0, 16.0, 32.0]),
            ("physbones_components", &[4.0, 8.0, 16.0, 32.0]),
            ("physbones_transforms", &[16.0, 64.0, 128.0, 256.0]),
            ("physbones_colliders", &[4.0, 8.0, 16.0, 32.0]),
            ("physbones_collision_checks", &[32.0, 128.0, 256.0, 512.0]),
            ("contacts", &[8.0, 16.0, 24.0, 32.0]),
            ("constraints", &[100.0, 250.0, 300.0, 350.0]),
            ("constraint_depth", &[20.0, 50.0, 80.0, 100.0]),
            ("animators", &[1.0, 4.0, 16.0, 32.0]),
            ("bones", &[75.0, 150.0, 256.0, 400.0]),
            ("lights", &[0.0, 0.0, 0.0, 1.0]),
            ("particle_systems", &[0.0, 4.0, 8.0, 16.0]),
            ("total_particles", &[0.0, 300.0, 1000.0, 2500.0]),
            ("mesh_particle_polys", &[0.0, 1000.0, 2000.0, 5000.0]),
            ("particle_trails", &[0.0, 0.0, 1.0, 1.0]),
            ("particle_collisions", &[0.0, 0.0, 1.0, 1.0]),
            ("trail_renderers", &[1.0, 2.0, 4.0, 8.0]),
            ("line_renderers", &[1.0, 2.0, 4.0, 8.0]),
            ("raycasts", &[1.0, 4.0, 8.0, 15.0]),
            ("cloths", &[0.0, 1.0, 1.0, 1.0]),
            ("cloth_vertices", &[0.0, 50.0, 100.0, 200.0]),
            ("physics_colliders", &[0.0, 1.0, 8.0, 8.0]),
            ("physics_rigidbodies", &[0.0, 1.0, 8.0, 8.0]),
            ("audio_sources", &[1.0, 4.0, 8.0, 8.0]),
        ],
    ),
    (
        "vrchat-quest",
        &[
            ("triangles", &[7500.0, 10000.0, 15000.0, 20000.0]),
            ("bounds_x", &[2.5, 4.0, 5.0, 5.0]),
            ("bounds_y", &[2.5, 4.0, 6.0, 6.0]),
            ("bounds_z", &[2.5, 4.0, 5.0, 5.0]),
            ("texture_mib", &[10.0, 18.0, 25.0, 40.0]),
            ("skinned_meshes", &[1.0, 1.0, 2.0, 2.0]),
            ("basic_meshes", &[1.0, 1.0, 2.0, 2.0]),
            ("material_slots", &[1.0, 1.0, 2.0, 4.0]),
            ("physbones_components", &[0.0, 4.0, 6.0, 8.0]),
            ("physbones_transforms", &[0.0, 16.0, 32.0, 64.0]),
            ("physbones_colliders", &[0.0, 4.0, 8.0, 16.0]),
            ("physbones_collision_checks", &[0.0, 16.0, 32.0, 64.0]),
            ("contacts", &[2.0, 4.0, 8.0, 16.0]),
            ("constraints", &[30.0, 60.0, 120.0, 150.0]),
            ("constraint_depth", &[5.0, 15.0, 35.0, 50.0]),
            ("animators", &[1.0, 1.0, 1.0, 2.0]),
            ("bones", &[75.0, 90.0, 150.0, 150.0]),
            ("particle_systems", &[0.0, 0.0, 0.0, 2.0]),
            ("total_particles", &[0.0, 0.0, 0.0, 200.0]),
            ("mesh_particle_polys", &[0.0, 0.0, 0.0, 400.0]),
            ("particle_trails", &[0.0, 0.0, 0.0, 1.0]),
            ("particle_collisions", &[0.0, 0.0, 0.0, 1.0]),
            ("trail_renderers", &[0.0, 0.0, 0.0, 1.0]),
            ("line_renderers", &[0.0, 0.0, 0.0, 1.0]),
            ("raycasts", &[1.0, 2.0, 4.0, 8.0]),
        ],
    ),
];

/// The Android rows the page disables: Unsupported, never counted.
const QUEST_UNSUPPORTED: [&str; 6] =
    ["lights", "cloths", "cloth_vertices", "physics_colliders", "physics_rigidbodies", "audio_sources"];

/// The platform-general stats the registry has for other platforms; `VRChat`'s page does not cap them, so its tables
/// have no row for them.
const NOT_PUBLISHED_BY_VRCHAT: [&str; 6] =
    ["vertices", "morph_targets", "textures", "max_texture_side", "meshes", "attachments"];

fn builtin(id: &str) -> &'static LimitTable {
    builtin_checked(id).unwrap()
}

/// A table with the given header lines and stat rows, for the parser's error cases.
fn table_text(header: &str, rows: &str) -> String {
    format!(
        "[limits]\n{header}\n[limits.texture]\nopaque_bits = 4.0\nalpha_bits = 8.0\nmips = true\n[limits.stats]\n{rows}\n"
    )
}

const HEADER: &str = "platform = \"test\"\nversion = \"v1\"\nsource = \"https://example.invalid/limits\"\nretrieved = \"2026-10-12\"\ntiers = [\"Good\", \"Poor\"]\nbeyond = \"Over\"";

#[test]
fn every_number_of_the_old_file_is_in_the_new_tables() {
    for (id, rows) in OLD_NUMBERS {
        let table = builtin(id);
        for (key, limits) in *rows {
            let stat = table.stat(key).unwrap_or_else(|| panic!("{id} lost the row {key}"));
            let got: Vec<f64> = stat.per_tier.iter().map(|l| l.unwrap()).collect();
            assert_eq!(&got, limits, "{id} {key}");
            assert!(!stat.unsupported, "{id} {key}");
        }
        let unsupported: Vec<&str> = table.stats.iter().filter(|s| s.unsupported).map(|s| s.key.as_str()).collect();
        let expected: &[&str] = if *id == "vrchat-quest" { &QUEST_UNSUPPORTED } else { &[] };
        assert_eq!(unsupported, expected, "{id}");
        assert_eq!(table.stats.len(), rows.len() + unsupported.len(), "{id} has a row the old file did not");
    }
}

#[test]
fn every_registered_table_parses_and_names_only_registry_stats_in_registry_order() {
    // Invariants of any platform's table, so a platform that registers its own needs no edit here.
    let ids: Vec<&str> = builtin_ids().collect();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "ids are sorted and unique: {ids:?}");
    for id in &ids {
        let table = builtin(id);
        assert_eq!(table.id, *id, "the table id is the file stem");
        assert!(!table.platform.trim().is_empty(), "{id}: the platform name is set");
        let registry: Vec<&str> = STATS.iter().map(|d| d.key).collect();
        let keys: Vec<&str> = table.stats.iter().map(|s| s.key.as_str()).collect();
        let mut at = 0;
        for key in &keys {
            let found =
                registry[at..].iter().position(|r| r == key).unwrap_or_else(|| panic!("{id}: {key} is out of order"));
            at += found + 1;
        }
        assert!(!table.tiers.contains(&table.beyond), "{id}: the beyond label is not a tier");
        assert!(table.advisory.iter().all(|a| table.tiers.contains(a)), "{id}: advisory names tiers");
    }
    assert!(LimitTable::builtin("nope").is_none());
    assert!(builtin_checked("nope").is_err());
}

#[test]
fn the_two_vrchat_tables_are_registered_with_their_published_tiers_and_stats() {
    // Assertions about VRChat's own tables, by id: another platform's table is not held to them.
    for id in ["vrchat-pc", "vrchat-quest"] {
        assert!(builtin_ids().any(|known| known == id), "{id} is registered");
        let table = builtin(id);
        let registry: Vec<&str> =
            STATS.iter().map(|d| d.key).filter(|key| !NOT_PUBLISHED_BY_VRCHAT.contains(key)).collect();
        let keys: Vec<&str> = table.stats.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, registry, "{id}: every stat VRChat publishes, in registry order");
        assert_eq!(table.tiers, ["Excellent", "Good", "Medium", "Poor"]);
        assert_eq!(table.beyond, "Very Poor");
        assert_eq!(table.advisory, Vec::<String>::new());
    }
}

#[test]
fn a_table_round_trips_through_toml() {
    for id in builtin_ids() {
        let table = builtin(id);
        let text = table.to_toml().unwrap();
        // The text holds no id: a built-in's comes from its registration.
        let parsed = LimitTable { id: table.id.clone(), ..LimitTable::parse(&text).unwrap() };
        assert_eq!(&parsed, table, "{id}");
        assert!(text.contains("bounds = [["), "{id}: bounds are written as the one documented row");
    }
    let with_unlimited =
        LimitTable::parse(&table_text(HEADER, "triangles = [10, \"-\"]\nlights = \"unsupported\"")).unwrap();
    assert_eq!(LimitTable::parse(&with_unlimited.to_toml().unwrap()).unwrap(), with_unlimited);
}

#[test]
fn a_header_field_that_is_missing_is_refused_by_name() {
    for field in ["source", "retrieved", "version", "platform", "tiers", "beyond"] {
        let header: String = HEADER.lines().filter(|l| !l.starts_with(field)).collect::<Vec<_>>().join("\n");
        let error = LimitTable::parse_named("data/platforms/test.toml", &table_text(&header, "")).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("data/platforms/test.toml") && text.contains(field), "{field}: {text}");
    }
    let empty_source = HEADER.replace("https://example.invalid/limits", "");
    let error = LimitTable::parse(&table_text(&empty_source, "")).unwrap_err();
    assert_eq!(error.key(), Some("source"));
}

#[test]
fn a_malformed_table_names_the_file_and_the_key() {
    let cases = [
        ("stats.cats", "cats = [1, 2]", "is not a stat"),
        ("stats.triangles", "triangles = [1, 2, 3]", "3 limits for 2 tiers"),
        ("stats.triangles", "triangles = [5, 1]", "limits fall"),
        ("stats.triangles", "triangles = [\"-\", 1]", "limits fall"),
        ("stats.triangles", "triangles = [-1, 1]", "non-negative"),
        ("stats.triangles", "triangles = \"maybe\"", "the only word is 'unsupported'"),
        ("stats.triangles", "triangles = 7", "not a integer"),
        ("stats.triangles", "triangles = [1, true]", "boolean"),
    ];
    for (key, row, reason) in cases {
        let error = LimitTable::parse_named("data/platforms/test.toml", &table_text(HEADER, row)).unwrap_err();
        assert_eq!((error.file(), error.key()), ("data/platforms/test.toml", Some(key)), "{row}");
        assert!(error.to_string().contains(reason), "{row}: {error}");
    }
    let bad_date = HEADER.replace("2026-10-12", "2026-13-01");
    assert_eq!(LimitTable::parse(&table_text(&bad_date, "")).unwrap_err().key(), Some("retrieved"));
    let twice = HEADER.replace("[\"Good\", \"Poor\"]", "[\"Good\", \"Good\"]");
    assert_eq!(LimitTable::parse(&table_text(&twice, "")).unwrap_err().key(), Some("tiers"));
    let no_tiers = HEADER.replace("[\"Good\", \"Poor\"]", "[]");
    assert_eq!(LimitTable::parse(&table_text(&no_tiers, "")).unwrap_err().key(), Some("tiers"));
    // The platform is a name people read, not a slug: several ids share it, and the id is the file stem.
    let named = HEADER.replace("\"test\"", "\"Test Platform (Quest)\"");
    assert_eq!(LimitTable::parse(&table_text(&named, "")).unwrap().id, "test-platform-quest");
    let extra = format!("{HEADER}\nfavourite = 3");
    assert!(LimitTable::parse(&table_text(&extra, "")).unwrap_err().to_string().contains("favourite"));
    assert!(LimitTable::parse("not toml at all =").is_err());
}

#[test]
fn an_unsupported_row_is_reported_and_does_not_lower_the_tier() {
    let table = LimitTable::parse(&table_text(HEADER, "triangles = [100, 200]\nlights = \"unsupported\"")).unwrap();
    let stats = AvatarStats { triangles: 50, lights: 9, ..AvatarStats::default() };
    let eval = evaluate(&stats, &table);
    assert_eq!(eval.tier, Some(0), "9 lights are stripped, so they do not count");
    let lights = eval.stats.iter().find(|v| v.key == "lights").unwrap();
    assert_eq!((lights.tier, lights.value), (StatTier::Unsupported, 9.0));
    let report = findings(&eval, &table, 1);
    assert_eq!(report.findings.len(), 1);
    let finding = &report.findings[0];
    assert_eq!((finding.severity, finding.rule.as_str()), (Severity::Info, "limits.test.lights"));
    assert_eq!(finding.hint.as_deref(), Some("fit.strip"));
    // An unsupported stat with no value says nothing.
    assert_eq!(findings(&evaluate(&AvatarStats::default(), &table), &table, 1).findings.len(), 0);
}

#[test]
fn the_quest_note_is_the_platforms_wording() {
    let table = builtin("vrchat-quest");
    let stats = AvatarStats { audio_sources: 3, ..AvatarStats::default() };
    let report = findings(&evaluate(&stats, table), table, 3);
    let finding = &report.findings[0];
    assert_eq!(finding.rule, "limits.vrchat-quest.audio_sources");
    assert!(finding.message.contains("disabled on mobile"), "{}", finding.message);
}

#[test]
fn a_one_tier_table_accepts_at_the_limit_and_refuses_one_over() {
    let header = HEADER.replace("[\"Good\", \"Poor\"]", "[\"accept\"]");
    let table = LimitTable::parse(&table_text(&header, "triangles = [70000]\nbounds = [[0, 2, 0]]")).unwrap();
    let at = AvatarStats { triangles: 70_000, bounds: [0.0, 2.0, 0.0], ..AvatarStats::default() };
    assert_eq!(evaluate(&at, &table).tier, Some(0));
    let over = AvatarStats { triangles: 70_001, ..at };
    let eval = evaluate(&over, &table);
    assert_eq!(eval.tier, None);
    let report = findings(&eval, &table, 0);
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].message, "triangles 70,001, over accept (70,000)");
    // A target past the last tier is the last tier.
    assert_eq!(findings(&eval, &table, 9), report);
}

#[test]
fn a_tier_with_no_limit_takes_any_value() {
    let table = LimitTable::parse(&table_text(HEADER, "triangles = [10, \"-\"]")).unwrap();
    let stats = AvatarStats { triangles: u64::from(u32::MAX), ..AvatarStats::default() };
    let eval = evaluate(&stats, &table);
    assert_eq!(eval.tier, Some(1));
    assert_eq!(findings(&eval, &table, 1).findings.len(), 0);
    assert_eq!(findings(&eval, &table, 0).findings.len(), 1);
}

#[test]
fn the_worst_supported_stat_decides_the_tier() {
    let table = builtin("vrchat-quest");
    let stats = AvatarStats { skinned_meshes: 2, bones: 120, animators: 3, ..AvatarStats::default() };
    let eval = evaluate(&stats, table);
    assert_eq!(eval.tier, None, "3 animators is past Poor (2)");
    let stats = AvatarStats { skinned_meshes: 2, bones: 120, ..AvatarStats::default() };
    assert_eq!(evaluate(&stats, table).tier, Some(2));
    assert_eq!(evaluate(&AvatarStats::default(), table).tier, Some(0));
}

#[test]
fn stale_counts_days_since_the_page_was_read() {
    let table = LimitTable::parse(&table_text(HEADER, "")).unwrap();
    assert_eq!(table.retrieved, "2026-10-12");
    assert!(stale(&table, "2027-01-11", 90), "91 days");
    assert!(!stale(&table, "2027-01-10", 90), "90 days");
    assert!(!stale(&table, "2026-10-12", 0));
    assert!(!stale(&table, "2026-10-01", 90), "a clock before the read is not stale");
    // Across a leap day: 2028-02-28 to 2028-03-01 is 2 days.
    let leap = LimitTable::parse(&table_text(&HEADER.replace("2026-10-12", "2028-02-28"), "")).unwrap();
    assert!(!stale(&leap, "2028-03-01", 2) && stale(&leap, "2028-03-01", 1));
    for not_a_date in ["", "2027-1-1", "2027-02-30", "yesterday", "2027-01-111"] {
        assert!(stale(&table, not_a_date, 90), "'{not_a_date}' fails closed");
    }
}

#[test]
fn findings_name_the_rule_the_numbers_and_the_fit_stage() {
    let table = builtin("vrchat-quest");
    let stats = AvatarStats { triangles: 120_000, ..AvatarStats::default() };
    let report = findings(&evaluate(&stats, table), table, 3);
    assert_eq!(report.findings.len(), 1);
    let finding = &report.findings[0];
    assert_eq!(finding.rule, "limits.vrchat-quest.triangles");
    assert_eq!(finding.hint.as_deref(), Some("fit.decimate"));
    assert_eq!(finding.severity, Severity::Error);
    assert_eq!(finding.message, "triangles 120,000, over Poor (20,000)");
    assert_eq!(finding.location.as_deref(), Some("triangles"));
    // Within the target tier there is nothing to report; below it, the stat is over.
    let within = AvatarStats { triangles: 20_000, ..AvatarStats::default() };
    assert_eq!(findings(&evaluate(&within, table), table, 3).findings.len(), 0);
    assert_eq!(findings(&evaluate(&within, table), table, 2).findings.len(), 1);
}

#[test]
fn units_show_in_the_message_and_each_stat_names_its_fix() {
    let table = builtin("vrchat-pc");
    let stats = AvatarStats {
        bounds: [2.0, 7.25, 2.0],
        material_slots: 40,
        bones: 500,
        skinned_meshes: 20,
        texture_px_opaque: 500_000_000,
        ..AvatarStats::default()
    };
    let report = findings(&evaluate(&stats, table), table, 3);
    let hint = |key: &str| report.findings.iter().find(|f| f.rule.ends_with(key)).unwrap().hint.clone().unwrap();
    assert_eq!(hint(".material_slots"), "fit.atlas");
    assert_eq!(hint(".bones"), "fit.bones");
    assert_eq!(hint(".skinned_meshes"), "fit.meshes");
    assert_eq!(hint(".texture_mib"), "fit.textures");
    let bounds = report.findings.iter().find(|f| f.rule.ends_with(".bounds_y")).unwrap();
    assert_eq!(bounds.message, "bounds_y 7.25 m, over Poor (6 m)");
    let texture = report.findings.iter().find(|f| f.rule.ends_with(".texture_mib")).unwrap();
    assert!(texture.message.contains(" MiB, over Poor (150 MiB)"), "{}", texture.message);
}

#[test]
fn texture_memory_follows_the_tables_own_estimate() {
    let stats = AvatarStats { texture_px_opaque: 1_000_000, texture_px_alpha: 1_000_000, ..AvatarStats::default() };
    let pc = stats.texture_mib(&builtin("vrchat-pc").texture);
    let quest = stats.texture_mib(&builtin("vrchat-quest").texture);
    assert!((pc - (1_000_000.0 * 0.5 + 1_000_000.0) * 4.0 / 3.0 / (1024.0 * 1024.0)).abs() < 1e-12);
    assert!((quest - 2_000_000.0 * (3.56 / 8.0) * 4.0 / 3.0 / (1024.0 * 1024.0)).abs() < 1e-12);
    assert_eq!(stats.get("texture_mib", &builtin("vrchat-pc").texture), Some(pc));
    assert_eq!(stats.get("no_such_stat", &builtin("vrchat-pc").texture), None);
}

#[test]
fn a_hand_built_table_with_an_unknown_stat_never_passes() {
    let mut table = LimitTable::parse(&table_text(HEADER, "triangles = [10, 20]")).unwrap();
    table.stats[0].key = "mystery".into();
    let eval = evaluate(&AvatarStats::default(), &table);
    assert_eq!(eval.tier, None);
    assert_eq!(eval.stats[0].tier, StatTier::Beyond);
}

#[test]
fn every_fix_names_a_known_stage_or_says_what_to_do() {
    use omuzaki_io_ops::limits::Fix;
    let stages = ["fit.decimate", "fit.atlas", "fit.meshes", "fit.bones", "fit.textures", "fit.strip"];
    for def in STATS {
        match def.fix {
            Fix::Stage(stage) => assert!(stages.contains(&stage), "{}: {stage}", def.key),
            Fix::Hand(text) => assert!(text.contains(' '), "{}: a hand fix is a sentence", def.key),
        }
    }
}

/// A two-tier table: `recommended` is advisory and `accept` is the hard limit, as Resonite's published sizes are.
fn advisory_table() -> LimitTable {
    let header =
        HEADER.replace("[\"Good\", \"Poor\"]", "[\"recommended\", \"accept\"]") + "\nadvisory = [\"recommended\"]";
    LimitTable::parse(&table_text(&header, "triangles = [100, 200]")).unwrap()
}

#[test]
fn the_label_names_the_tier_or_what_is_past_the_last() {
    let table = builtin("vrchat-quest");
    let at = |triangles| {
        let stats = AvatarStats { triangles, ..AvatarStats::default() };
        evaluate(&stats, table).label(table).to_string()
    };
    assert_eq!(at(7_500), "Excellent");
    assert_eq!(at(20_000), "Poor");
    assert_eq!(at(20_001), "Very Poor", "past the last tier is the table's beyond label");
    let one_tier =
        LimitTable::parse(&table_text(&HEADER.replace("[\"Good\", \"Poor\"]", "[\"accept\"]"), "triangles = [10]"))
            .unwrap();
    let stats = AvatarStats { triangles: 11, ..AvatarStats::default() };
    assert_eq!(evaluate(&stats, &one_tier).label(&one_tier), "Over");
}

#[test]
fn the_beyond_label_must_not_be_a_tier_and_must_be_there() {
    let same = HEADER.replace("beyond = \"Over\"", "beyond = \"Poor\"");
    assert_eq!(LimitTable::parse(&table_text(&same, "")).unwrap_err().key(), Some("beyond"));
    let blank = HEADER.replace("beyond = \"Over\"", "beyond = \"\"");
    assert_eq!(LimitTable::parse(&table_text(&blank, "")).unwrap_err().key(), Some("beyond"));
}

#[test]
fn a_stat_past_an_advisory_tier_but_within_the_target_is_a_warning() {
    let table = advisory_table();
    let target = 1; // accept
    let level = |triangles| {
        let stats = AvatarStats { triangles, ..AvatarStats::default() };
        findings(&evaluate(&stats, &table), &table, target)
    };
    assert!(level(100).findings.is_empty(), "within the recommendation: nothing to say");
    let report = level(150);
    let finding = report.findings.first().expect("a warning");
    assert_eq!((finding.severity, finding.rule.as_str()), (Severity::Warning, "limits.test.triangles"));
    assert!(finding.message.contains("past the recommended size (100)"), "{}", finding.message);
    assert_eq!(finding.hint.as_deref(), Some("fit.decimate"));
    assert_eq!(report.errors().count(), 0, "a recommendation is never an error");
    let over = level(201);
    assert_eq!(over.errors().count(), 1, "past the hard limit is an error as before");
}

#[test]
fn advisory_must_name_tiers_and_each_once() {
    for (advisory, reason) in [("[\"nope\"]", "not a tier"), ("[\"Good\", \"Good\"]", "twice")] {
        let header = format!("{HEADER}\nadvisory = {advisory}");
        let error = LimitTable::parse(&table_text(&header, "")).unwrap_err();
        assert_eq!(error.key(), Some("advisory"), "{advisory}");
        assert!(error.to_string().contains(reason), "{error}");
    }
}

#[test]
fn the_profile_section_is_left_for_the_registry_and_the_table_still_round_trips() {
    let text = format!("{}\n[profile]\nformat = \"vrm0\"\nanything = 3\n", table_text(HEADER, "triangles = [10, 20]"));
    let table = LimitTable::parse(&text).unwrap();
    assert_eq!(table.stat("triangles").unwrap().per_tier, [Some(10.0), Some(20.0)]);
    assert!(table.to_toml().unwrap().contains("[limits]"), "the table is written as the [limits] section");
    assert!(LimitTable::parse(&format!("{text}\n[typo]\nx = 1\n")).unwrap_err().to_string().contains("typo"));
}

#[test]
fn a_platform_registers_its_own_table_and_the_data_file_names_are_the_ids() {
    // The two VRChat tables register themselves (`vrchat_rank.rs`); nothing in `limits` names them. Each is read from
    // `data/platforms/<id>.toml`, and a lookup of anything else says so.
    let error = builtin_checked("nope").unwrap_err();
    assert_eq!(error.file(), "data/platforms");
    assert!(builtin_ids().all(|id| builtin(id).id == id));
    // Both VRChat ids share one platform name, so a listing groups them.
    assert_eq!(builtin("vrchat-pc").platform, builtin("vrchat-quest").platform);
}

#[test]
fn bounds_are_one_row_of_x_y_z_per_tier() {
    let parse = |rows: &str| LimitTable::parse(&table_text(HEADER, rows));
    let table = parse("bounds = [[1, 2, 3], [4, 5, \"-\"]]").unwrap();
    assert_eq!(table.stat("bounds_y").unwrap().per_tier, [Some(2.0), Some(5.0)]);
    assert_eq!(table.stat("bounds_z").unwrap().per_tier, [Some(3.0), None]);
    assert!(table.to_toml().unwrap().contains("bounds = [[1.0, 2.0, 3.0], [4.0, 5.0, \"-\"]]"));
    assert!(parse("bounds = \"unsupported\"").unwrap().stat("bounds_x").unwrap().unsupported);
    for (rows, why) in [
        ("bounds_x = [1, 2]", "an axis on its own"),
        ("bounds = [[1, 2, 3]]", "one triple for two tiers"),
        ("bounds = [[1, 2], [3, 4]]", "a pair"),
        ("bounds = [1, 2]", "numbers instead of triples"),
        ("bounds = [[1, 2, 3], [-4, 5, 6]]", "a negative limit"),
        ("bounds = 4", "not an array"),
    ] {
        assert_eq!(parse(rows).unwrap_err().key(), Some("stats.bounds"), "{why}");
    }
}
