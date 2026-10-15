use std::path::PathBuf;

use super::*;
use crate::eval::fixture::Fixture;
use crate::eval::provenance::{EvalSet, Item, Provenance, Source};
use crate::eval::{FixtureFile, Script, TaskFile, Unparsed};

fn task(id: &str, feature: &str, fixture: &str, prompt: &str) -> TaskFile {
    let text = format!(
        "id = '{id}'\nfeature = '{feature}'\ntitle = 't'\nprompt = '{prompt}'\nfixture = '{fixture}'\nscopes = ['read']\n\
         [[check]]\nid = 'c'\nkind = 'doc.exists'\npath = 'a'\n"
    );
    let script = |pass: bool| Script {
        file_name: format!("{id}.x.{}.jsonl", if pass { "pass" } else { "fail" }),
        expect_pass: pass,
        bytes: vec![],
    };
    TaskFile {
        path: PathBuf::from(format!("crates/c/evals/{id}.toml")),
        crate_name: "c".into(),
        task: toml::from_str(&text).unwrap(),
        bytes: text.into_bytes(),
        scripts: vec![script(true), script(false)],
    }
}

fn fixture(id: &str, steps: &str) -> FixtureFile {
    let text = format!("id = '{id}'\nsummary = 's'\nsteps = [{steps}]\n");
    FixtureFile {
        path: PathBuf::from(format!("crates/c/evals/fixtures/{id}.toml")),
        fixture: toml::from_str::<Fixture>(&text).unwrap(),
        bytes: text.into_bytes(),
    }
}

fn features() -> BTreeSet<String> {
    ["QG-03".to_owned()].into()
}

fn private() -> PrivateList {
    PrivateList { prefixes: vec!["characters/example-private/".into()], ..PrivateList::default() }
}

fn run(d: &Discovery, texts: Option<&[ToolText]>) -> Vec<String> {
    let features = features();
    let private = private();
    lint(&Inputs { discovery: d, features: &features, private: &private, tool_texts: texts })
        .into_iter()
        .map(|f| f.message)
        .collect()
}

fn clean() -> Discovery {
    Discovery {
        tasks: vec![task("a.one", "QG-03", "f", "Make her hair shorter")],
        fixtures: vec![fixture("f", "")],
        ..Discovery::default()
    }
}

fn has(found: &[String], needle: &str) -> bool {
    found.iter().any(|m| m.contains(needle))
}

#[test]
fn a_clean_workspace_has_no_findings() {
    assert_eq!(run(&clean(), None), Vec::<String>::new());
}

#[test]
fn duplicate_task_and_fixture_ids_are_found() {
    let mut d = clean();
    d.tasks.push(task("a.one", "QG-03", "f", "Other"));
    d.fixtures.push(fixture("f", ""));
    let found = run(&d, None);
    assert!(has(&found, "duplicate task id `a.one`") && has(&found, "duplicate fixture id `f`"), "{found:?}");
}

#[test]
fn an_unknown_fixture_is_found() {
    let mut d = clean();
    d.tasks[0].task.fixture = "nope".into();
    assert!(has(&run(&d, None), "unknown fixture `nope`"));
}

#[test]
fn a_feature_that_is_not_in_the_spec_is_found() {
    let mut d = clean();
    d.tasks[0].task.feature = "ZZ-99".into();
    assert!(has(&run(&d, None), "feature `ZZ-99`"));
}

#[test]
fn a_task_needs_both_scripts() {
    let mut d = clean();
    d.tasks[0].scripts.retain(|s| s.expect_pass);
    let found = run(&d, None);
    assert!(has(&found, "fail.jsonl") && !has(&found, "pass.jsonl"), "{found:?}");
}

#[test]
fn a_task_needs_a_hard_check_and_unique_check_ids() {
    let mut d = clean();
    d.tasks[0].task.checks[0].hard = false;
    assert!(has(&run(&d, None), "at least one hard check"));
    let mut d = clean();
    let again = d.tasks[0].task.checks[0].clone();
    d.tasks[0].task.checks.push(again);
    assert!(has(&run(&d, None), "duplicate check id `c`"));
}

#[test]
fn a_bad_task_id_is_found() {
    let mut d = clean();
    d.tasks[0].task.id = "Bad_Id".into();
    assert!(has(&run(&d, None), "must be lowercase"));
}

#[test]
fn unknown_check_kinds_and_parse_errors_are_reported_by_file() {
    let mut d = clean();
    d.unparsed.push(Unparsed {
        path: "crates/c/evals/x.toml".into(),
        reason: "boom".into(),
        unknown_kinds: vec!["doc.sparkle".into()],
    });
    d.unparsed.push(Unparsed {
        path: "crates/c/evals/y.toml".into(),
        reason: "bad toml".into(),
        unknown_kinds: vec![],
    });
    let found = run(&d, None);
    assert!(has(&found, "unknown check kind `doc.sparkle`") && has(&found, "does not parse: bad toml"), "{found:?}");
    assert!(!has(&found, "boom"), "an unknown kind is explained by its own finding: {found:?}");
}

fn set(item: Item) -> EvalSet {
    EvalSet { id: "s1".into(), kind: "synthetic".into(), split: "dev".into(), summary: "s".into(), items: vec![item] }
}

fn record(source: Source) -> Provenance {
    Provenance {
        source,
        commercial: "yes".into(),
        consent: None,
        person: false,
        licence: None,
        generator: Some("0123abc".into()),
    }
}

fn with_set(item: Item) -> Discovery {
    let mut d = clean();
    d.fixtures[0] = fixture("f", "{ op = 'a.b', args = { sheet = { set = 's1', item = 'b3:01' } } }");
    let set = set(item);
    d.sets.insert(set.id.clone(), set.clone());
    d.set_files.push((PathBuf::from("crates/c/evalsets/s1.toml"), set));
    d
}

#[test]
fn a_fixture_item_with_a_clean_record_passes() {
    let d = with_set(Item { hash: "b3:01".into(), path: None, provenance: Some(record(Source::Synthetic)) });
    assert_eq!(run(&d, None), Vec::<String>::new());
}

#[test]
fn a_fixture_item_under_the_private_list_is_refused_even_with_a_yes_record() {
    let d = with_set(Item {
        hash: "b3:01".into(),
        path: Some("characters/example-private/concept/sheet.png".into()),
        provenance: Some(record(Source::Own)),
    });
    let found = run(&d, None);
    assert!(found.len() >= 2, "once for the set, once for the fixture that uses it: {found:?}");
    assert!(found.iter().all(|m| m.contains("private list")), "{found:?}");
}

#[test]
fn a_fixture_item_of_an_unknown_set_or_missing_from_it_is_found() {
    let mut d = clean();
    d.fixtures[0] = fixture("f", "{ op = 'a.b', args = { sheet = { set = 'ghost', item = 'b3:01' } } }");
    assert!(has(&run(&d, None), "unknown eval set `ghost`"));
    let mut d = with_set(Item { hash: "b3:02".into(), path: None, provenance: Some(record(Source::Cc0)) });
    d.fixtures[0] = fixture("f", "{ op = 'a.b', args = { sheet = { set = 's1', item = 'b3:01' } } }");
    assert!(has(&run(&d, None), "is not in eval set `s1`"));
}

fn texts(description: &str) -> Vec<ToolText> {
    vec![ToolText { source: "tool face_set_profile".into(), text: description.into() }]
}

#[test]
fn a_prompt_quoting_a_tool_description_is_found() {
    let mut d = clean();
    d.tasks[0].task.prompt = "Please make her face a bit more oval, keep everything else".into();
    let found = run(&d, Some(&texts("Use this to make her face a bit more oval, keep everything else as it is.")));
    assert!(has(&found, "prompt shares 8+ words with tool face_set_profile"), "{found:?}");
}

#[test]
fn a_prompt_sharing_only_a_short_run_passes() {
    let mut d = clean();
    d.tasks[0].task.prompt = "Make her face a bit more oval".into();
    assert_eq!(run(&d, Some(&texts("make her face a bit more round and then more oval"))), Vec::<String>::new());
}

#[test]
fn leak_comparison_ignores_case_and_punctuation() {
    let mut d = clean();
    d.tasks[0].task.prompt = "MAKE her face, a bit: more OVAL please!".into();
    assert!(has(&run(&d, Some(&texts("make her face a bit more oval please"))), "prompt shares"));
}

#[test]
fn tool_dumps_parse_in_both_shapes() {
    let array = tool_texts_from_json(r#"[{"name":"a","description":"d"}]"#).unwrap();
    assert_eq!(array, vec![ToolText { source: "tool a".into(), text: "d".into() }]);
    let object = tool_texts_from_json(r#"{"instructions":"i","tools":[{"name":"a","description":"d"}]}"#).unwrap();
    assert_eq!(object.len(), 2);
    assert!(tool_texts_from_json("3").is_err());
}

#[test]
fn feature_ids_come_from_full_table_rows_only() {
    let spec = "| QG-03 | Name | P0 | notes |\n| CB-10 | x | P1 | y |\nQG-04 in prose\n| QG-05 | no closing pipe\n| xx-01 | lower | P0 | n |\n";
    let ids = spec_feature_ids(spec);
    assert_eq!(ids.into_iter().collect::<Vec<_>>(), vec!["CB-10", "QG-03"]);
}
