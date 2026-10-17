//! Which script belongs to which task when one task's file stem begins with another's.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use omuzaki_test::eval;

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn task(id: &str) -> String {
    format!("id = '{id}'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'p'\nfixture = 'f'\nscopes = ['read']\n")
}

#[test]
fn a_script_of_a_longer_task_stem_is_not_also_a_script_of_the_shorter_one() {
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().join("crates/c/evals");
    write(&dir.join("foo.toml"), &task("foo"));
    write(&dir.join("foo.bar.toml"), &task("foo.bar"));
    write(&dir.join("foo.right.pass.jsonl"), "{}\n");
    write(&dir.join("foo.bar.correct.pass.jsonl"), "{}\n");
    write(&dir.join("foo.bar.wrong.fail.jsonl"), "{}\n");
    // A script that only the shorter stem can own: the longer task has no script name left once its stem is removed.
    write(&dir.join("foo.bar.pass.jsonl"), "{}\n");
    let found = eval::discover(ws.path()).unwrap();
    let scripts_of = |id: &str| {
        let file = found.tasks.iter().find(|t| t.task.id == id).unwrap();
        file.scripts.iter().map(|s| s.file_name.clone()).collect::<Vec<_>>()
    };
    assert_eq!(scripts_of("foo"), ["foo.bar.pass.jsonl", "foo.right.pass.jsonl"]);
    assert_eq!(scripts_of("foo.bar"), ["foo.bar.correct.pass.jsonl", "foo.bar.wrong.fail.jsonl"]);
}
