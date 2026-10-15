//! The scripting guide stays true: the limits it quotes are the limits the host enforces, every repo path it names
//! exists, every example is mentioned, and its sample script runs.

// A test file: a failed unwrap is the failure, and reading the guide is what the test is for.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    clippy::case_sensitive_file_extension_comparisons
)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::{FakeRunner, Flag, request, run_with};
use omuzaki_script::Limits;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn guide() -> String {
    std::fs::read_to_string(repo().join("docs").join("guide").join("scripting.md")).unwrap()
}

/// The rows of the guide's limits table: label -> (console, job), the numbers as written.
fn limits_table(text: &str) -> BTreeMap<String, (u64, u64)> {
    let section = text.split("\n## Limits").nth(1).expect("a Limits section");
    let section = section.split("\n## ").next().unwrap();
    section
        .lines()
        .filter(|line| line.starts_with('|'))
        .filter_map(|line| {
            let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
            let [label, console, job] = cells[..] else { return None };
            Some((label.to_owned(), (console.replace(',', "").parse().ok()?, job.replace(',', "").parse().ok()?)))
        })
        .collect()
}

#[test]
fn the_limits_the_guide_quotes_are_the_limits_the_host_enforces() {
    let table = limits_table(&guide());
    let (console, job) = (Limits::console(), Limits::job(None));
    let expected = |pick: fn(&Limits) -> u64| (pick(&console), pick(&job));
    let rows: [(&str, (u64, u64)); 14] = [
        ("Source size (bytes)", expected(|l| l.source_bytes as u64)),
        ("Operations", expected(|l| l.operations)),
        ("Inner op calls", expected(|l| u64::from(l.calls))),
        ("Call depth", expected(|l| l.call_depth as u64)),
        ("Expression depth", expected(|l| l.expr_depth as u64)),
        ("Expression depth in functions", expected(|l| l.fn_expr_depth as u64)),
        ("String size (bytes)", expected(|l| l.string_bytes as u64)),
        ("Array length", expected(|l| l.array_len as u64)),
        ("Map size", expected(|l| l.map_len as u64)),
        ("Output lines", expected(|l| l.output_lines as u64)),
        ("Output bytes", expected(|l| l.output_bytes as u64)),
        ("Wall clock (seconds)", expected(|l| u64::from(l.seconds))),
        ("Result size (JSON bytes)", expected(|l| l.result_bytes as u64)),
        ("Nesting of arrays and maps", expected(|l| l.value_depth as u64)),
    ];
    for (label, numbers) in rows {
        assert_eq!(table.get(label), Some(&numbers), "{label}");
    }
    assert_eq!(table.len(), rows.len(), "the guide's table has a row the host has no limit for: {table:?}");
}

#[test]
fn the_guide_says_what_seconds_a_job_gets() {
    assert_eq!(Limits::job(None).seconds, 120);
    assert_eq!(Limits::job(Some(10_000)).seconds, Limits::MAX_JOB_SECONDS);
    assert!(guide().contains("1 to 600, 120 if you leave it out"));
}

/// Every backticked token that looks like a path into the repository.
fn repo_paths(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter(|token| ["crates/", "docs/", "tools/"].iter().any(|root| token.starts_with(root)))
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_repo_path_the_guide_names_exists() {
    let paths = repo_paths(&guide());
    assert!(paths.len() >= 3, "{paths:?}");
    for path in paths {
        assert!(
            repo().join(path.trim_end_matches('/')).exists(),
            "the guide names `{path}`, which is not in the repository"
        );
    }
}

#[test]
fn every_example_is_named_in_the_guide_and_every_named_example_exists() {
    let text = guide();
    let examples = repo().join("crates").join("omuzaki-script").join("examples");
    let on_disk: Vec<String> = std::fs::read_dir(&examples)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".rhai"))
        .collect();
    for name in &on_disk {
        assert!(text.contains(&format!("`{name}`")), "the guide does not mention {name}");
    }
    let named: Vec<&str> = text.split('`').skip(1).step_by(2).filter(|token| token.ends_with(".rhai")).collect();
    for name in named {
        assert!(examples.join(name).exists(), "the guide names {name}, which is not in examples/");
    }
}

#[test]
fn the_guides_sample_script_runs() {
    let text = guide();
    let sample = text.split("```rhai\n").nth(1).unwrap().split("```").next().unwrap();
    let (_, result) = run_with(request(sample), &Flag::default(), Limits::console(), FakeRunner::default());
    let outcome = result.unwrap_or_else(|e| panic!("{e}\n{sample}"));
    assert_eq!(outcome.value, serde_json::json!([2, 4, 6]));
    assert_eq!(outcome.output.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["working on aki"]);
}

#[test]
fn the_guide_uses_only_the_synthetic_characters() {
    let text = guide();
    for name in ["aki", "ren"] {
        assert!(text.contains(name));
    }
    assert!(!text.contains("http://") && !text.contains("C:\\") && !text.contains("/home/"));
}
