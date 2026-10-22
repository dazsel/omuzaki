//! The crate's fences that read the workspace (design §3 rule 4, §13 IN-3): no other crate turns on `test-hooks`, and
//! nothing in the crate or its dependency tree injects input or hooks it. Listed in xtask's workspace scans, since a
//! change in any crate can break them.
// The tests read the workspace's manifests and the crate's sources; the file fence is for ops.
#![allow(clippy::unwrap_used, clippy::disallowed_methods)]

use std::collections::BTreeSet;
use std::path::Path;

fn workspace() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(Path::parent).unwrap()
}

#[test]
fn no_other_crate_enables_test_hooks() {
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(workspace().join("crates")).unwrap() {
        let dir = entry.unwrap().path();
        let manifest = dir.join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else { continue };
        let doc: toml::Table = toml::from_str(&text).unwrap();
        let name = doc.get("package").and_then(|p| p.get("name")).and_then(toml::Value::as_str).unwrap_or_default();
        if name == "omuzaki-input" {
            continue;
        }
        let mut tables: Vec<&toml::Value> = Vec::new();
        for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
            tables.extend(doc.get(key));
        }
        if let Some(targets) = doc.get("target").and_then(toml::Value::as_table) {
            for target in targets.values() {
                for key in ["dependencies", "dev-dependencies"] {
                    tables.extend(target.get(key));
                }
            }
        }
        let by_dependency = tables.iter().filter_map(|t| t.get("omuzaki-input")).any(|dep| {
            dep.get("features")
                .and_then(toml::Value::as_array)
                .is_some_and(|f| f.iter().any(|f| f.as_str() == Some("test-hooks")))
        });
        let by_feature = doc.get("features").and_then(toml::Value::as_table).is_some_and(|features| {
            features
                .values()
                .flat_map(|v| v.as_array().into_iter().flatten())
                .any(|f| f.as_str() == Some("omuzaki-input/test-hooks"))
        });
        if by_dependency || by_feature {
            offenders.push(name.to_owned());
        }
    }
    assert!(offenders.is_empty(), "these crates enable omuzaki-input's test-hooks: {offenders:?}");
}

#[test]
fn the_crate_has_no_injection_or_hook_library_and_names_no_such_call() {
    // §3 rule 4: the dependency tree in Cargo.lock.
    let lock: toml::Table = toml::from_str(&std::fs::read_to_string(workspace().join("Cargo.lock")).unwrap()).unwrap();
    let packages = lock.get("package").and_then(toml::Value::as_array).unwrap();
    let deps_of = |name: &str| -> Vec<String> {
        packages
            .iter()
            .filter(|p| p.get("name").and_then(toml::Value::as_str) == Some(name))
            .flat_map(|p| p.get("dependencies").and_then(toml::Value::as_array).cloned().unwrap_or_default())
            .filter_map(|d| d.as_str().and_then(|d| d.split(' ').next()).map(str::to_owned))
            .collect()
    };
    let mut seen = BTreeSet::new();
    let mut queue = vec!["omuzaki-input".to_owned()];
    while let Some(name) = queue.pop() {
        if seen.insert(name.clone()) {
            queue.extend(deps_of(&name));
        }
    }
    assert!(seen.len() > 3, "the walk found the crate's dependencies");
    for banned in ["rdev", "device_query", "inputbot", "willhook"] {
        assert!(!seen.contains(banned), "{banned} is in omuzaki-input's dependency tree");
    }
    // ... and its source.
    let symbols = [
        "SendInput",
        "CGEventPost",
        "uinput",
        "VirtualDevice",
        "fake_input",
        "xtest",
        "SetWindowsHookEx",
        "rdev",
        "device_query",
        "inputbot",
        "willhook",
    ];
    let mut files = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut scanned = 0;
    while let Some(path) = files.pop() {
        if path.is_dir() {
            files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for symbol in symbols {
                assert!(!text.contains(symbol), "{} names {symbol}", path.display());
            }
            scanned += 1;
        }
    }
    assert!(scanned >= 10, "scanned {scanned} files");
}
