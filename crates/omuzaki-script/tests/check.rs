//! The static check: what it finds, where, and that it agrees with what a run sends.

// A test file: a failed unwrap is the failure, and building a script as text is clearer with `format!`.
#![allow(clippy::unwrap_used, clippy::format_push_string)]

mod common;

use common::{FakeRunner, Flag, request, run, run_with};
use omuzaki_ops_api::{ErrorCode, Grants, Runner, Scope};
use omuzaki_script::{CheckReport, FieldErrorKind, Limits, ScriptErrorKind, ScriptHost, effective_grants};
use proptest::prelude::*;

fn host() -> ScriptHost {
    ScriptHost::new(&FakeRunner::default().ops(), Limits::console())
}

fn at(report: &CheckReport) -> Vec<(String, u32, u32)> {
    report.ops.iter().map(|o| (o.name.clone(), o.line, o.column)).collect()
}

#[test]
fn it_lists_calls_with_their_positions_including_fns_and_closures() {
    let source = "\
ops::face::set_profile(#{ character: \"a\" });
fn helper(c) {
    ops::measure::likeness(#{ character: c })
}
[0].for_each(|x| ops::face::set_profile(#{ character: \"b\" }));
try_op(|| ops::face::set_profile(#{ character: \"c\" }));
";
    let report = host().check(source);
    assert_eq!(
        at(&report),
        [
            ("face.set_profile".to_owned(), 1, 12),
            ("measure.likeness".to_owned(), 3, 19),
            ("face.set_profile".to_owned(), 5, 29),
            ("face.set_profile".to_owned(), 6, 22),
        ],
        "{report:?}"
    );
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn a_directive_that_would_be_ignored_is_refused_where_it_stands() {
    for (source, position, place) in [
        ("1;\n// omuzaki: grants = [\"read\"]\n2", (2, 1), "after the first line"),
        ("// a note\n  // omuzaki: grants = []\n", (2, 3), "after the first line"),
        ("\u{feff}// omuzaki: grants = [\"read\"]\n1;", (1, 2), "after a byte order mark"),
    ] {
        let report = host().check(source);
        // Ours comes first; Rhai may add its own complaint about the same line.
        let found = report.parse_errors.first().expect(source);
        assert_eq!((found.line, found.column), position, "{source:?}: {report:?}");
        assert!(found.message.contains(place), "{source:?}: {found:?}");
        let (runner, result) = run(source);
        assert_eq!(result.expect_err("refused").kind, ScriptErrorKind::Parse, "{source:?}");
        assert_eq!(runner.requests(), vec![], "{source:?}");
    }
    // After code on the first line is not the first thing on it either.
    let report = host().check("1; // omuzaki: grants = [\"read\"]");
    assert!(report.parse_errors[0].message.contains("after code on the line"), "{report:?}");
    // On the first line, with no mark, it is still read.
    assert_eq!(host().check("// omuzaki: grants = [\"read\"]\n1").ceiling, Some(vec![Scope::Read]));
}

#[test]
fn directive_text_that_is_not_a_comment_is_just_text() {
    for source in [
        "let s = `\n// omuzaki: grants = []\n`; s",
        "let s = `a ${ `\n// omuzaki: nope\n` } b`; s",
        "/*\n// omuzaki: grants = []\n*/ 1",
        "/* /* nested */\n// omuzaki: grants = [] */ 1",
        "let c = '\\'';\nlet s = \"it's\";\n1",
        "let s = \"say \\\" // omuzaki: grants = []\"; s",
    ] {
        let report = host().check(source);
        assert!(report.parse_errors.is_empty(), "{source:?}: {report:?}");
        assert_eq!(report.ceiling, None, "{source:?}");
    }
    // A real comment after such a string is still found.
    let report = host().check("let s = \"x\";\n// omuzaki: grants = []\n");
    assert_eq!(report.parse_errors[0].line, 2, "{report:?}");
    let (runner, result) = run("let s = `\n// omuzaki: grants = [\\\"read\\\"]\n`; s.len()");
    assert!(result.is_ok(), "text in a string narrows nothing and refuses nothing");
    assert_eq!(runner.requests(), vec![]);
}

#[test]
fn a_position_in_the_check_is_the_position_a_run_reports() {
    let source = "let x = 1;\nops::face::set_profil(#{ character: \"a\" });";
    let report = host().check(source);
    assert_eq!(report.unknown_ops.len(), 1);
    let unknown = &report.unknown_ops[0];
    assert_eq!(unknown.name, "face.set_profil");
    let (_, result) = run(source);
    let error = result.expect_err("an unknown op fails the run");
    assert_eq!((error.line, error.column), (unknown.line, unknown.column), "{error}");
    assert!(error.message.contains("Function not found: ops::face::set_profil"), "{error}");
}

#[test]
fn computed_and_pointer_based_calls_are_refused_at_parse() {
    for source in [
        "Fn(\"ops::face::set_profile\").call(#{})",
        "call(f, 1)",
        "let f = |x| x; f.curry(1)",
        "import \"x\" as y;",
        "eval(\"1\")",
    ] {
        let report = host().check(source);
        assert!(!report.parse_errors.is_empty(), "{source}: {report:?}");
        assert!(!report.is_clean(), "{source}");
        assert!(report.ops.is_empty(), "{source}");
    }
}

#[test]
fn every_problem_is_reported_not_just_the_first() {
    let source = "\
ops::face::set_profile(#{ character: \"a\", blnd: 0.5 });
ops::face::set_profil(#{});
ops::face::set_profile(#{ profile: \"egg\" });
ops::render::video(#{});
ops::script::run(#{});
";
    let report = host().check(source);
    assert_eq!(
        report.unknown_ops.iter().map(|u| (u.name.as_str(), u.line)).collect::<Vec<_>>(),
        [("face.set_profil", 2)]
    );
    let fields: Vec<_> = report.field_errors.iter().map(|f| (f.field.as_str(), f.kind, f.line)).collect();
    assert_eq!(fields, [("blnd", FieldErrorKind::Unknown, 1), ("character", FieldErrorKind::Missing, 3)]);
    let key = source.lines().next().unwrap().find("blnd").unwrap() + 1;
    assert_eq!(report.field_errors[0].column as usize, key, "an unknown field is placed at its key");
    assert_eq!(report.refused.iter().map(|r| r.op.as_str()).collect::<Vec<_>>(), ["render.video", "script.run"]);
    assert!(report.refused.iter().all(|r| r.hint.is_some()));
    assert_eq!(report.problems().len(), 5);
    assert_eq!(report.first_problem().map(|p| p.line), Some(1));
}

#[test]
fn a_map_that_is_not_a_literal_is_not_checked_against_the_schema() {
    let report = host().check("let a = #{ nope: 1 };\nops::face::set_profile(a);");
    assert!(report.field_errors.is_empty(), "{report:?}");
    assert_eq!(report.ops.len(), 1);
}

#[test]
fn an_op_whose_schema_lists_no_fields_gets_no_field_errors() {
    let report = host().check("ops::measure::likeness(#{ anything: 1 });");
    assert_eq!(report.field_errors, vec![]);
}

#[test]
fn scopes_are_the_scopes_of_the_named_ops() {
    let report = host().check("ops::face::set_profile(#{ character: \"a\" });\nops::export_::publish_clip(#{});");
    assert_eq!(report.scopes, [Scope::EditCharacter, Scope::Publish]);
    assert_eq!(report.ceiling, None);
}

#[test]
fn parse_errors_have_a_line_and_a_column() {
    let report = host().check("let a = 1;\nlet b = ;");
    assert_eq!(report.parse_errors.len(), 1);
    assert_eq!((report.parse_errors[0].line, report.parse_errors[0].column), (2, 9));
}

#[test]
fn a_source_over_the_limit_is_a_parse_error_and_is_not_parsed() {
    let report = host().check(&" ".repeat(64 * 1024 + 1));
    assert_eq!(report.parse_errors.len(), 1);
    assert!(report.parse_errors[0].message.contains("source limit"), "{report:?}");
}

#[test]
fn the_report_serialises_with_the_documented_keys() {
    let json = serde_json::to_value(host().check("ops::face::set_profile(#{ character: \"a\" });")).unwrap();
    let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["ceiling", "field_errors", "ops", "parse_errors", "refused", "scopes", "unknown_ops"]);
    assert_eq!(json["ops"][0], serde_json::json!({ "name": "face.set_profile", "line": 1, "column": 12 }));
    assert_eq!(json["scopes"], serde_json::json!(["edit:character"]));
}

// -- the ceiling and the effective grants ---------------------------------------------------------------------------

#[test]
fn the_ceiling_is_read_from_the_first_line() {
    let report = host()
        .check("// omuzaki: grants = [\"edit:character\", \"render\"]\nops::face::set_profile(#{ character: \"a\" });");
    assert_eq!(report.ceiling, Some(vec![Scope::EditCharacter, Scope::Render]));
    assert!(report.is_clean());
}

#[test]
fn an_unreadable_ceiling_is_an_error_not_no_ceiling() {
    for source in
        ["// omuzaki: grants = [\"edit:everything\"]", "// omuzaki: grants = edit:character", "// omuzaki: grant = []"]
    {
        let report = host().check(source);
        assert_eq!(report.parse_errors.len(), 1, "{source}: {report:?}");
        assert_eq!(report.ceiling, None);
        // A run refuses it too, rather than running with more than the author meant.
        let (runner, result) = run(source);
        assert_eq!(result.expect_err("refused").kind, ScriptErrorKind::Parse, "{source}");
        assert_eq!(runner.requests(), vec![]);
    }
}

#[test]
fn a_script_naming_only_face_ops_gets_read_and_edit_character_even_for_a_caller_holding_everything() {
    let report = host().check("ops::face::set_profile(#{ character: \"a\" });");
    let grants = effective_grants(&Grants::person(), &report);
    assert_eq!(grants.scopes().collect::<Vec<_>>(), [Scope::Read, Scope::EditCharacter]);
    assert!(!grants.allows(Scope::Publish));
}

#[test]
fn a_ceiling_naming_a_scope_the_caller_lacks_does_not_widen() {
    let report = host().check(
        "// omuzaki: grants = [\"edit:character\", \"publish\"]\nops::face::set_profile(#{ character: \"a\" });",
    );
    let caller = Grants::of([Scope::Read, Scope::EditCharacter]);
    let grants = effective_grants(&caller, &report);
    assert_eq!(grants.scopes().collect::<Vec<_>>(), [Scope::Read, Scope::EditCharacter]);
}

#[test]
fn a_ceiling_narrows_below_the_named_ops() {
    let report = host().check("// omuzaki: grants = [\"edit:scene\"]\nops::face::set_profile(#{ character: \"a\" });");
    let grants = effective_grants(&Grants::person(), &report);
    assert!(!grants.allows(Scope::EditCharacter), "the ceiling forbids what the op needs");
    assert!(grants.allows(Scope::Read), "read stays: every request needs it");
}

#[test]
fn no_ceiling_means_no_narrowing_beyond_the_named_ops() {
    let report = host().check("ops::face::set_profile(#{ character: \"a\" });\nops::export_::publish_clip(#{});");
    let grants = effective_grants(&Grants::person(), &report);
    assert_eq!(grants.scopes().collect::<Vec<_>>(), [Scope::Read, Scope::EditCharacter, Scope::Publish]);
}

#[test]
fn a_caller_without_a_scope_never_gains_it_from_the_script() {
    let report = host().check("ops::export_::publish_clip(#{});");
    let grants = effective_grants(&Grants::of([Scope::Read]), &report);
    assert_eq!(grants.scopes().collect::<Vec<_>>(), [Scope::Read]);
}

#[test]
fn a_run_sends_its_requests_with_the_effective_grants() {
    let (runner, result) = run("ops::face::set_profile(#{ character: \"a\" });");
    result.expect("runs");
    let grants = &runner.requests()[0].grants;
    assert!(grants.allows(Scope::EditCharacter) && grants.allows(Scope::Read));
    assert!(!grants.allows(Scope::Publish), "the script names no publishing op");
}

// -- refusals --------------------------------------------------------------------------------------------------------

#[test]
fn jobs_effectful_edits_and_nested_script_runs_are_refused_in_the_check_and_in_a_run() {
    for (op, call) in [
        ("render.video", "ops::render::video(#{})"),
        ("file.write", "ops::file::write(#{})"),
        ("script.run", "ops::script::run(#{})"),
    ] {
        let source = format!("let x = 1;\n{call};");
        let report = host().check(&source);
        assert_eq!(report.refused.len(), 1, "{op}: {report:?}");
        let verb_column = u32::try_from(call.rfind("::").unwrap() + 3).unwrap();
        assert_eq!((report.refused[0].line, report.refused[0].column), (2, verb_column));
        assert!(report.refused[0].hint.is_some(), "{op}");
        assert!(!report.is_clean());

        let (runner, result) = run(&source);
        let error = result.expect_err("refused");
        assert_eq!(error.kind, ScriptErrorKind::Refused, "{op}");
        assert_eq!((error.line, error.column), (2, verb_column), "{op}");
        let op_error = error.op_error.expect("the Unsupported error with its hint");
        assert_eq!(op_error.code, ErrorCode::Unsupported, "{op}");
        assert_eq!(op_error.message, report.refused[0].message, "{op}: the run says what the check says");
        assert_eq!(op_error.hint, report.refused[0].hint, "{op}");
        assert!(runner.requests().is_empty(), "{op}: nothing was sent");
    }
}

#[test]
fn a_nested_script_run_inside_try_op_is_still_refused() {
    let (runner, result) = run("try_op(|| ops::script::run(#{ source: \"1\" }))");
    assert_eq!(result.expect_err("a refusal can't be caught").kind, ScriptErrorKind::Refused);
    assert_eq!(runner.requests(), vec![]);
}

// -- the check agrees with the run ----------------------------------------------------------------------------------

/// A script built from calls, loops, functions, closures and branches.
#[derive(Clone, Debug)]
enum Piece {
    Call(&'static str),
    Loop(u8, Vec<Piece>),
    Func(Vec<Piece>),
    Closure(Vec<Piece>),
    Branch(bool, Vec<Piece>),
}

const NAMES: [&str; 3] = ["face.set_profile", "measure.likeness", "face.fail"];

fn render(pieces: &[Piece], indent: &str, out: &mut String, counter: &mut u32) {
    let inner = format!("{indent}  ");
    for piece in pieces {
        *counter += 1;
        let id = *counter;
        match piece {
            Piece::Call(name) => {
                let (area, verb) = name.split_once('.').unwrap();
                out.push_str(&format!("{indent}try_op(|| ops::{area}::{verb}(#{{ character: \"a\" }}));\n"));
            }
            Piece::Loop(n, body) => {
                out.push_str(&format!("{indent}for i in 0..{n} {{\n"));
                render(body, &inner, out, counter);
                out.push_str(&format!("{indent}}}\n"));
            }
            Piece::Func(body) => {
                out.push_str(&format!("{indent}fn f{id}() {{\n"));
                render(body, &inner, out, counter);
                out.push_str(&format!("{inner}()\n{indent}}}\n{indent}f{id}();\n"));
            }
            Piece::Closure(body) => {
                // `call` is not in the language, so a closure runs through an array method (or `try_op`).
                out.push_str(&format!("{indent}[0].map(|x{id}| {{\n"));
                render(body, &inner, out, counter);
                out.push_str(&format!("{inner}()\n{indent}}});\n"));
            }
            Piece::Branch(taken, body) => {
                out.push_str(&format!("{indent}if {taken} {{\n"));
                render(body, &inner, out, counter);
                out.push_str(&format!("{indent}}}\n"));
            }
        }
    }
}

/// A piece that can sit anywhere: a function definition can't, so it is only added at the top level.
fn inner_piece() -> impl Strategy<Value = Piece> {
    let leaf = prop::sample::select(NAMES.to_vec()).prop_map(Piece::Call);
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            (1u8..4, prop::collection::vec(inner.clone(), 1..3)).prop_map(|(n, b)| Piece::Loop(n, b)),
            prop::collection::vec(inner.clone(), 1..3).prop_map(Piece::Closure),
            (any::<bool>(), prop::collection::vec(inner, 1..3)).prop_map(|(t, b)| Piece::Branch(t, b)),
        ]
    })
}

fn piece() -> impl Strategy<Value = Piece> {
    prop_oneof![
        3 => inner_piece(),
        1 => prop::collection::vec(inner_piece(), 1..3).prop_map(Piece::Func),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Every op a run sends is one the check listed, and a script with no branch sends exactly the listed set.
    #[test]
    fn check_ops_agree_with_the_calls_the_run_sends(pieces in prop::collection::vec(piece(), 1..5)) {
        let mut source = String::new();
        render(&pieces, "", &mut source, &mut 0);
        // Nested closures cost expression depth; the generated scripts are deeper than the console allows.
        let roomy = Limits { expr_depth: 256, fn_expr_depth: 256, ..Limits::console() };
        let report = ScriptHost::new(&FakeRunner::default().ops(), roomy.clone()).check(&source);
        prop_assert!(report.parse_errors.is_empty(), "{}\n{:?}", source, report);
        let listed: std::collections::BTreeSet<String> = report.ops.iter().map(|o| o.name.clone()).collect();
        let (runner, result) = run_with(request(&source), &Flag::default(), roomy, FakeRunner::default());
        prop_assert!(result.is_ok(), "{}\n{:?}", source, result);
        let sent: std::collections::BTreeSet<String> = runner.requests().into_iter().map(|r| r.op).collect();
        prop_assert!(sent.is_subset(&listed), "sent {:?} not in listed {:?}\n{}", sent, listed, source);
        if !source.contains("if ") {
            prop_assert_eq!(&sent, &listed, "{}", source);
        }
    }
}
