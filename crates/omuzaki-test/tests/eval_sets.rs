//! Eval sets: the content store, `eval sets verify`, `add` and `holdout-check`, the `FileSource` a model suite reads its
//! fixtures through, and the lint's kind and split rules.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use omuzaki_ai_api::{FetchError, FileSource, NoProgress, PinnedFile};
use omuzaki_store_api::ContentHash;
use omuzaki_test::eval::lint::{self, Inputs};
use omuzaki_test::eval::provenance::PrivateList;
use omuzaki_test::eval::sets::{SetStore, StoreFiles};
use omuzaki_test::eval::{self};
use tempfile::TempDir;

/// A workspace with the shared private list and one crate, and a store beside it.
struct World {
    ws: TempDir,
    store: TempDir,
}

impl World {
    fn new() -> Self {
        let ws = tempfile::tempdir().unwrap();
        write(
            &ws.path().join("docs/licensing/private-material.toml"),
            "names = ['example-private']\nprefixes = ['characters/example-private/']\nhashes = []\n",
        );
        fs::create_dir_all(ws.path().join("crates/c")).unwrap();
        Self { ws, store: tempfile::tempdir().unwrap() }
    }

    fn root(&self) -> &str {
        common::arg(self.ws.path())
    }

    fn store(&self) -> SetStore {
        SetStore::new(self.store.path())
    }

    /// `omuzaki-test eval sets <args>` against this world.
    fn sets(&self, args: &[&str]) -> std::process::Output {
        let envs = [("OMUZAKI_EVAL_SETS", common::arg(self.store.path()))];
        common::run(&[&["eval", "sets"], args, &["--root", self.root()]].concat(), &envs)
    }

    /// A file in a scratch folder, with `content`, named `name` under `dir`.
    fn file(&self, dir: &str, name: &str, content: &[u8]) -> String {
        let path = self.store.path().join("incoming").join(dir).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        common::arg(&path).to_owned()
    }

    fn manifest(&self, set: &str) -> String {
        fs::read_to_string(self.ws.path().join(format!("crates/c/evalsets/{set}.toml"))).unwrap()
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn out(output: &std::process::Output) -> String {
    format!("{}{}", common::text(&output.stdout), common::text(&output.stderr))
}

const NEW: &[&str] = &["--new-in", "c", "--kind", "synthetic", "--split", "dev", "--summary", "Synthetic sheets."];

fn add_synthetic(world: &World, set: &str, file: &str, extra: &[&str]) -> std::process::Output {
    world.sets(&[&["add", set, file, "--source", "synthetic", "--generator", "abc1234"], extra].concat())
}

#[test]
fn add_stores_an_item_writes_its_manifest_and_verify_passes() {
    let world = World::new();
    let file = world.file("a", "sheet.png", b"a synthetic sheet");
    let added = add_synthetic(&world, "sheets-1", &file, NEW);
    assert_eq!(common::code(&added), 0, "{}", out(&added));
    let hash = ContentHash::of(b"a synthetic sheet");
    assert!(out(&added).contains(&format!("created the set and stored b3:{hash}")), "{}", out(&added));
    let manifest = world.manifest("sheets-1");
    assert!(
        manifest.contains(&format!("hash = \"b3:{hash}\"")) && manifest.contains("generator = \"abc1234\""),
        "{manifest}"
    );
    assert!(!manifest.contains("incoming"), "the file's own path is not written into a public manifest: {manifest}");
    assert_eq!(world.store().get(&hash).unwrap(), b"a synthetic sheet");

    // The same item again changes nothing; a second item is appended to the same manifest.
    let again = add_synthetic(&world, "sheets-1", &file, &[]);
    assert!(out(&again).contains("already in"), "{}", out(&again));
    assert_eq!(world.manifest("sheets-1"), manifest);
    let second = world.file("a", "sheet2.png", b"another sheet");
    assert_eq!(common::code(&add_synthetic(&world, "sheets-1", &second, &["--path", "sheets/two.png"])), 0);
    let manifest = world.manifest("sheets-1");
    assert_eq!(manifest.matches("[[item]]").count(), 2);
    assert!(manifest.contains("path = \"sheets/two.png\""));

    let verified = world.sets(&["verify"]);
    assert_eq!(common::code(&verified), 0, "{}", out(&verified));
    assert!(out(&verified).contains("2 item(s) checked, 0 problem(s)"), "{}", out(&verified));
}

#[test]
fn verify_reports_a_missing_a_damaged_and_a_malformed_item() {
    let world = World::new();
    let good = world.file("a", "good.png", b"good");
    let damaged = world.file("a", "damaged.png", b"damaged soon");
    let missing = world.file("a", "missing.png", b"missing soon");
    for file in [&good, &damaged, &missing] {
        let extra: &[&str] = if file == &good { NEW } else { &[] };
        assert_eq!(common::code(&add_synthetic(&world, "sheets-1", file, extra)), 0);
    }
    // A damaged object, a missing object, and a manifest entry with a hash that is not one.
    let store = world.store();
    fs::write(store.object_path(&ContentHash::of(b"damaged soon")), b"damaged soo").unwrap();
    fs::remove_file(store.object_path(&ContentHash::of(b"missing soon"))).unwrap();
    let path = world.ws.path().join("crates/c/evalsets/sheets-1.toml");
    let mut manifest = fs::read_to_string(&path).unwrap();
    manifest.push_str(
        "\n[[item]]\nhash = \"b3:xyz\"\n[item.provenance]\nsource = \"synthetic\"\ncommercial = \"yes\"\ngenerator = \"abc1234\"\n",
    );
    fs::write(&path, manifest).unwrap();

    let verified = world.sets(&["verify"]);
    assert_eq!(common::code(&verified), 1, "{}", out(&verified));
    let text = out(&verified);
    assert!(text.contains(&format!("item b3:{}: damaged", ContentHash::of(b"damaged soon"))), "{text}");
    assert!(text.contains(&format!("item b3:{}: missing from the store", ContentHash::of(b"missing soon"))), "{text}");
    assert!(text.contains("item b3:xyz: the hash is not"), "{text}");
    assert!(text.contains("4 item(s) checked, 3 problem(s)"), "{text}");
    assert!(!text.contains(&ContentHash::of(b"good").to_string()), "an intact item is not mentioned: {text}");
}

#[test]
fn add_refuses_an_item_on_the_private_list_by_path_or_by_hash_and_never_stores_it() {
    let world = World::new();
    let private = world.file("characters/example-private/concept", "a.png", b"third party art");
    let refused = add_synthetic(&world, "sheets-1", &private, NEW);
    assert_eq!(common::code(&refused), 2, "{}", out(&refused));
    assert!(out(&refused).contains("private list") && out(&refused).contains("not stored"), "{}", out(&refused));
    assert!(!world.store().object_path(&ContentHash::of(b"third party art")).exists());
    assert!(!world.ws.path().join("crates/c/evalsets").exists(), "no manifest was made for a refused item");

    // By hash: a renamed copy of listed material is still caught.
    let hash = ContentHash::of(b"renamed copy");
    write(
        &world.ws.path().join("docs/licensing/private-material.toml"),
        &format!("names = ['example-private']\nprefixes = ['characters/none/']\nhashes = ['b3:{hash}']\n"),
    );
    let copy = world.file("elsewhere", "b.png", b"renamed copy");
    let refused = add_synthetic(&world, "sheets-1", &copy, NEW);
    assert_eq!(common::code(&refused), 2, "{}", out(&refused));
    assert!(out(&refused).contains("matches the private list"), "{}", out(&refused));
    assert!(!world.store().object_path(&hash).exists());
}

#[test]
fn add_refuses_a_missing_provenance_record_an_absolute_origin_and_a_set_it_cannot_place() {
    let world = World::new();
    let file = world.file("a", "sheet.png", b"sheet");
    // A synthetic item names its generator's commit.
    let no_generator = world.sets(&[
        "add",
        "sheets-1",
        &file,
        "--source",
        "synthetic",
        "--new-in",
        "c",
        "--kind",
        "synthetic",
        "--split",
        "dev",
        "--summary",
        "s",
    ]);
    assert_eq!(common::code(&no_generator), 2);
    assert!(
        out(&no_generator).contains("generator") || out(&no_generator).contains("provenance"),
        "{}",
        out(&no_generator)
    );
    // Commercial use must be clear.
    let no = add_synthetic(&world, "sheets-1", &file, &[&["--commercial", "no"], NEW].concat());
    assert_eq!(common::code(&no), 2, "{}", out(&no));
    // A machine path would be written into a public file.
    for origin in ["/home/someone/a.png", "C:\\Users\\someone\\a.png", "../a.png"] {
        let refused = add_synthetic(&world, "sheets-1", &file, &[&["--path", origin], NEW].concat());
        assert_eq!(common::code(&refused), 2, "{origin}");
        assert!(out(&refused).contains("machine path"), "{}", out(&refused));
    }
    // A set that does not exist needs --new-in, a kind that is not one is refused, and a crate must exist.
    let unknown_set = add_synthetic(&world, "nope", &file, &[]);
    assert_eq!(common::code(&unknown_set), 2);
    assert!(out(&unknown_set).contains("--new-in"), "{}", out(&unknown_set));
    let bad_kind = add_synthetic(
        &world,
        "sheets-1",
        &file,
        &["--new-in", "c", "--kind", "imaginary", "--split", "dev", "--summary", "s"],
    );
    assert_eq!(common::code(&bad_kind), 2);
    assert!(out(&bad_kind).contains("`real` or `synthetic`"), "{}", out(&bad_kind));
    let no_crate = add_synthetic(
        &world,
        "sheets-1",
        &file,
        &["--new-in", "nowhere", "--kind", "real", "--split", "dev", "--summary", "s"],
    );
    assert_eq!(common::code(&no_crate), 2);
    assert!(!world.store().object_path(&ContentHash::of(b"sheet")).exists(), "nothing was stored for any of them");
    // An existing set is not created again.
    assert_eq!(common::code(&add_synthetic(&world, "sheets-1", &file, NEW)), 0);
    let twice = add_synthetic(&world, "sheets-1", &world.file("a", "other.png", b"other"), NEW);
    assert_eq!(common::code(&twice), 2);
    assert!(out(&twice).contains("exists already"), "{}", out(&twice));
}

#[test]
fn holdout_check_catches_a_real_or_held_out_item_in_a_training_manifest() {
    let world = World::new();
    let dev = world.file("a", "dev.png", b"a dev item");
    let held = world.file("a", "held.png", b"a held-out item");
    let real = world.file("a", "real.png", b"a real item");
    assert_eq!(common::code(&add_synthetic(&world, "dev-1", &dev, NEW)), 0);
    let held_set = ["--new-in", "c", "--kind", "synthetic", "--split", "holdout", "--summary", "Held out."];
    assert_eq!(common::code(&add_synthetic(&world, "held-1", &held, &held_set)), 0);
    let real_set = ["--new-in", "c", "--kind", "real", "--split", "dev", "--summary", "Real."];
    let added = world.sets(
        &[&["add", "real-1", &real, "--source", "own", "--consent", "consent/x#abc", "--person"], &real_set[..]]
            .concat(),
    );
    assert_eq!(common::code(&added), 0, "{}", out(&added));

    let training = world.ws.path().join("training");
    fs::create_dir_all(&training).unwrap();
    let hex = |bytes: &[u8]| ContentHash::of(bytes).to_string();
    let clean = training.join("clean.toml");
    write(&clean, &format!("[[item]]\nhash = \"b3:{}\"\n", hex(b"a dev item")));
    let held_json = training.join("held.json");
    write(&held_json, &format!("{{\"files\": [\"{}\"]}}", hex(b"a held-out item").to_uppercase()));
    let real_list = training.join("real.txt");
    write(&real_list, &format!("something\nb3:{}\n", hex(b"a real item")));

    let check = |manifests: &[&Path]| {
        let args: Vec<&str> = manifests.iter().map(|m| common::arg(m)).collect();
        world.sets(&[&["holdout-check"], &args[..]].concat())
    };
    let fine = check(&[&clean]);
    assert_eq!(common::code(&fine), 0, "a dev synthetic item may train: {}", out(&fine));
    let leaked = check(&[&clean, &held_json, &real_list]);
    assert_eq!(common::code(&leaked), 1, "{}", out(&leaked));
    let text = out(&leaked);
    assert!(text.contains("held.json") && text.contains("set `held-1` (holdout)"), "{text}");
    assert!(text.contains("real.txt") && text.contains("set `real-1` (real)"), "{text}");
    assert!(!text.contains("clean.toml"), "{text}");
    assert!(text.contains("3 training manifest(s) checked, 2 leak(s)"), "{text}");
    assert_eq!(common::code(&check(&[&training.join("missing.toml")])), 2);
}

#[test]
fn a_protected_set_with_a_malformed_hash_cannot_be_checked_and_says_so() {
    let world = World::new();
    write(
        &world.ws.path().join("crates/c/evalsets/held.toml"),
        "id='held'\nkind='real'\nsplit='dev'\nsummary='s'\n[[item]]\nhash='b3:01'\n[item.provenance]\nsource='cc0'\ncommercial='yes'\n",
    );
    let training = world.ws.path().join("t.toml");
    write(&training, "x = 1\n");
    let checked = world.sets(&["holdout-check", common::arg(&training)]);
    assert_eq!(common::code(&checked), 2, "{}", out(&checked));
    assert!(out(&checked).contains("malformed hash"), "{}", out(&checked));
}

#[test]
fn add_refuses_a_crate_name_that_is_a_path_and_a_record_that_contradicts_the_manifest() {
    let world = World::new();
    let file = world.file("a", "sheet.png", b"sheet");
    for bad in ["..", "/etc", "a/b", "A"] {
        let new = ["--new-in", bad, "--kind", "synthetic", "--split", "dev", "--summary", "s"];
        let refused = add_synthetic(&world, "sheets-1", &file, &new);
        assert_eq!(common::code(&refused), 2, "{bad}");
        assert!(out(&refused).contains("not a crate name"), "{bad}: {}", out(&refused));
    }
    assert!(!world.ws.path().join("evalsets").exists());
    assert_eq!(common::code(&add_synthetic(&world, "sheets-1", &file, NEW)), 0);
    // The same bytes said again as another kind of source are not "already in": the manifest's record would be wrong.
    let contradicts = world.sets(&["add", "sheets-1", &file, "--source", "cc0"]);
    assert_eq!(common::code(&contradicts), 2, "{}", out(&contradicts));
    assert!(out(&contradicts).contains("different provenance record"), "{}", out(&contradicts));
    let same = add_synthetic(&world, "sheets-1", &file, &[]);
    assert!(out(&same).contains("already in"), "{}", out(&same));
}

#[test]
fn holdout_check_also_finds_a_held_out_item_named_by_its_sha256() {
    let world = World::new();
    let held = world.file("a", "held.png", b"held out by sha");
    let held_set = ["--new-in", "c", "--kind", "synthetic", "--split", "holdout", "--summary", "Held out."];
    assert_eq!(common::code(&add_synthetic(&world, "held-1", &held, &held_set)), 0);
    let sha = world.store().put(b"held out by sha").unwrap().sha256.to_string();
    let training = world.ws.path().join("training.toml");
    write(&training, &format!("pin = \"{sha}\"\n"));
    let checked = world.sets(&["holdout-check", common::arg(&training)]);
    assert_eq!(common::code(&checked), 1, "{}", out(&checked));
    assert!(out(&checked).contains("set `held-1` (holdout)"), "{}", out(&checked));
}

#[test]
fn holdout_check_will_not_pass_when_it_cannot_name_every_protected_item_or_read_every_manifest() {
    let world = World::new();
    let held = world.file("a", "held.png", b"held out");
    let held_set = ["--new-in", "c", "--kind", "synthetic", "--split", "holdout", "--summary", "Held out."];
    assert_eq!(common::code(&add_synthetic(&world, "held-1", &held, &held_set)), 0);
    let training = world.ws.path().join("training.toml");
    write(&training, "pin = \"nothing\"\n");
    assert_eq!(common::code(&world.sets(&["holdout-check", common::arg(&training)])), 0);
    // The item is gone from the store: its SHA-256 is unknown, so the check cannot say a manifest does not pin it.
    let hash = world.store().put(b"held out").unwrap().hash;
    let object = world.store.path().join("objects").join(hash.to_string());
    fs::remove_file(&object).unwrap();
    let blind = world.sets(&["holdout-check", common::arg(&training)]);
    assert_eq!(common::code(&blind), 2, "{}", out(&blind));
    assert!(out(&blind).contains("missing or damaged"), "{}", out(&blind));
    // A set manifest that does not parse is a set nothing protects: both commands stop.
    write(&world.ws.path().join("crates/c/evalsets/broken.toml"), "this is = not [valid\n");
    for args in [&["holdout-check", common::arg(&training)][..], &["verify"][..]] {
        let refused = world.sets(args);
        assert_eq!(common::code(&refused), 2, "{args:?}: {}", out(&refused));
        assert!(out(&refused).contains("does not parse"), "{args:?}: {}", out(&refused));
    }
}

#[test]
fn a_file_that_changed_after_it_was_judged_leaves_nothing_in_the_store() {
    let world = World::new();
    let file = world.file("a", "sheet.png", b"first bytes");
    let expected = ContentHash::of(b"other bytes");
    let error = world.store().put_file_expecting(std::path::Path::new(&file), &expected).unwrap_err().to_string();
    assert!(error.contains("changed while it was being added"), "{error}");
    // Neither the object nor its index entry nor a staging file is left behind.
    let actual = ContentHash::of(b"first bytes");
    assert!(!world.store.path().join("objects").join(actual.to_string()).exists());
    let leftovers: Vec<_> =
        walk(world.store.path()).into_iter().filter(|p| !p.starts_with(world.store.path().join("incoming"))).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // With the right hash it is stored.
    let right = ContentHash::of(b"first bytes");
    assert_eq!(world.store().put_file_expecting(std::path::Path::new(&file), &right).unwrap().hash, right);
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[test]
fn the_file_source_returns_the_path_of_verified_bytes_for_a_pin() {
    let dir = tempfile::tempdir().unwrap();
    let store = SetStore::new(dir.path());
    let stored = store.put(b"read speech, 16 kHz").unwrap();
    let files = StoreFiles::new(store.clone());
    let pin = |sha256, size| PinnedFile::new(["https://example.invalid/never-fetched"], sha256, size);

    let path = files.get(&pin(stored.sha256, stored.size), &NoProgress).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"read speech, 16 kHz");
    assert_eq!(files.lookup(stored.sha256).unwrap(), Some(path.clone()));

    // A pin of another length is not the reviewed file; an item the store lacks is an error that says what to do.
    assert!(matches!(files.get(&pin(stored.sha256, stored.size + 1), &NoProgress), Err(FetchError::Size { .. })));
    let unknown = omuzaki_ai_api::Sha256([3; 32]);
    let missing = files.get(&pin(unknown, 1), &NoProgress).unwrap_err().to_string();
    assert!(missing.contains("eval sets add"), "{missing}");
    assert_eq!(files.lookup(unknown).unwrap(), None);

    // A damaged item is never handed out, and nothing is deleted.
    fs::write(&path, b"read speech, 16 kHz (cut)").unwrap();
    assert_eq!(files.lookup(stored.sha256).unwrap(), None);
    assert!(files.get(&pin(stored.sha256, stored.size), &NoProgress).is_err());
    assert!(path.exists());
}

#[test]
fn the_lint_flags_a_kind_or_split_the_model_contract_does_not_know() {
    let world = World::new();
    let set = |id: &str, kind: &str, split: &str| {
        write(
            &world.ws.path().join(format!("crates/c/evalsets/{id}.toml")),
            &format!("id = '{id}'\nkind = '{kind}'\nsplit = '{split}'\nsummary = 's'\n"),
        );
    };
    set("good", "real", "holdout");
    set("odd-kind", "imaginary", "dev");
    set("odd-split", "synthetic", "test");
    let discovery = eval::discover(world.ws.path()).unwrap();
    let features: BTreeSet<String> = BTreeSet::new();
    let private = PrivateList::default();
    let findings = lint::lint(&Inputs {
        discovery: &discovery,
        features: &features,
        private: &private,
        tool_texts: None,
        ops: None,
    });
    let messages: Vec<String> = findings.iter().map(|f| format!("{}: {}", f.file.display(), f.message)).collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages.iter().any(|m| m.contains("odd-kind")
            && m.contains("`kind` is `imaginary`")
            && m.contains("`real` or `synthetic`")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("odd-split") && m.contains("`split` is `test`") && m.contains("`dev` or `holdout`")),
        "{messages:?}"
    );
}
