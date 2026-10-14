//! Seeded synthetic project documents and random patches (no private material; xorshift so runs repeat).
use serde_json::{Value, json};

use crate::{Change, Patch};

/// A tiny seeded generator.
pub struct Rng(pub u64);

impl Rng {
    /// Next 64 bits.
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// `files` scene-like files, each with scalars, a nested object and an array.
pub fn synthetic(files: usize) -> Value {
    let one = |i: usize| {
        json!({
            "name": format!("part-{i}"), "visible": true, "weight": i as f64 * 0.25,
            "transform": {"x": 0, "y": 0, "z": 0, "scale": 1.0},
            "tags": ["a", "b", "c"],
            "material": {"base": "#aabbcc", "rough": 0.5, "metal": 0.0},
        })
    };
    json!({ "files": (0..files).map(|i| (format!("f{i:03}"), one(i))).collect::<serde_json::Map<_, _>>() })
}

fn at(p: &[String], more: &[&str]) -> Vec<String> {
    p.iter().cloned().chain(more.iter().map(|s| (*s).to_owned())).collect()
}

/// One random valid patch against `doc`. Few keys on purpose, so peers collide often.
pub fn random_patch(r: &mut Rng, doc: &Value) -> Patch {
    let files = doc["files"].as_object().map_or(1, serde_json::Map::len);
    let f = vec!["files".into(), format!("f{:03}", r.below(files.max(1)))];
    let cur = |p: &[String]| crate::lookup(doc, p).cloned();
    let change = |path: Vec<String>, after: Option<Value>| Change { before: cur(&path), path, after };
    let n = r.below(100) as i64;
    let changes = match r.below(6) {
        0 => vec![change(at(&f, &["transform", "x"]), Some(n.into()))],
        1 => vec![
            change(at(&f, &["transform", "x"]), Some(n.into())),
            change(at(&f, &["transform", "y"]), Some(n.into())),
        ],
        2 => vec![change(at(&f, &["tags"]), Some(json!([n, "b"])))],
        3 => vec![change(at(&f, &["tags", &r.below(2).to_string()]), Some(n.into()))],
        4 => {
            let key = format!("extra{}", r.below(3));
            let new = cur(&at(&f, &[&key])).is_none().then(|| json!({"a": n, "b": {"c": n}}));
            vec![change(at(&f, &[&key]), new)]
        }
        _ => vec![change(at(&f, &["material", "rough"]), Some(json!(n as f64 / 100.0)))],
    };
    Patch { changes }
}

/// The patch that creates the synthetic project (one change holding the whole `files` object).
pub fn genesis(files: usize) -> Patch {
    let change = Change { path: vec!["files".into()], before: None, after: Some(synthetic(files)["files"].clone()) };
    Patch { changes: vec![change] }
}
