//! Spike: carry our document `Patch` through loro and look at what comes out.
//!
//! Mapping: an object is a `LoroMap` (so two peers editing different keys merge), a scalar or an array is a leaf
//! value (so two edits of one leaf are last-writer-wins). `Patch`/`Change` mirror `omuzaki-doc-api` (a path is a list
//! of segments) so the spike needs nothing from the main workspace.
use std::collections::HashMap;

use loro::{Container, ExportMode, LoroDoc, LoroMap, LoroValue, ValueOrContainer, VersionVector};
use serde_json::{Map, Number, Value};

pub mod synth;

/// One change at one path; `None` is absent (mirrors `omuzaki_doc_api::Change`).
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// Segments from the root.
    pub path: Vec<String>,
    /// What the change expects to find.
    pub before: Option<Value>,
    /// The value afterwards.
    pub after: Option<Value>,
}

/// An ordered list of changes (mirrors `omuzaki_doc_api::Patch`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Patch {
    /// The changes, in order.
    pub changes: Vec<Change>,
}

impl Patch {
    /// Changes reversed with `before` and `after` swapped: the exact undo.
    pub fn inverse(&self) -> Self {
        let swap = |c: &Change| Change { path: c.path.clone(), before: c.after.clone(), after: c.before.clone() };
        Self { changes: self.changes.iter().rev().map(swap).collect() }
    }
}

/// Why a patch was refused. Nothing was written.
#[derive(Debug, PartialEq)]
pub enum Refused {
    /// The document holds something other than `before` (a stale edit, §12).
    Conflict(Vec<String>),
    /// The parent of the path does not exist.
    Unreachable(Vec<String>),
    /// An array edit other than replacing an existing element (append, insert, remove): not supported by the spike.
    Unsupported(Vec<String>),
}

fn to_loro(v: &Value) -> LoroValue {
    match v {
        Value::Null => LoroValue::Null,
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => n.as_i64().map_or_else(|| n.as_f64().unwrap_or(0.0).into(), Into::into),
        Value::String(s) => s.as_str().into(),
        Value::Array(a) => a.iter().map(to_loro).collect::<Vec<_>>().into(),
        Value::Object(o) => o.iter().map(|(k, x)| (k.clone(), to_loro(x))).collect::<HashMap<_, _>>().into(),
    }
}

fn from_loro(v: &LoroValue) -> Value {
    match v {
        LoroValue::Bool(b) => Value::Bool(*b),
        LoroValue::I64(i) => Value::Number((*i).into()),
        LoroValue::Double(f) => Number::from_f64(*f).map_or(Value::Null, Value::Number),
        LoroValue::String(s) => Value::String(s.to_string()),
        LoroValue::List(l) => Value::Array(l.iter().map(from_loro).collect()),
        LoroValue::Map(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), from_loro(x))).collect::<Map<_, _>>()),
        _ => Value::Null,
    }
}

/// Writes `v` under `key`: objects become child maps, everything else a leaf.
fn put(map: &LoroMap, key: &str, v: &Value) -> Result<(), loro::LoroError> {
    let Value::Object(o) = v else { return map.insert(key, to_loro(v)) };
    let child = map.insert_container(key, LoroMap::new())?;
    o.iter().try_for_each(|(k, x)| put(&child, k, x))
}

/// The value at `path` in a plain document, if any.
pub fn lookup<'a>(v: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(v, |v, s| match v {
        Value::Object(m) => m.get(s),
        Value::Array(a) => s.parse::<usize>().ok().and_then(|i| a.get(i)),
        _ => None,
    })
}

/// Applies one change to a plain copy of the document, checking its `before` against what the earlier changes left.
fn replay(doc: &mut Value, c: &Change) -> Result<(), Refused> {
    let Some((last, parent)) = c.path.split_last() else { return Err(Refused::Unreachable(vec![])) };
    let slot = parent.iter().try_fold(doc, |v, s| match v {
        Value::Object(o) => o.get_mut(s),
        Value::Array(a) => s.parse::<usize>().ok().and_then(|i| a.get_mut(i)),
        _ => None,
    });
    match slot {
        Some(Value::Object(o)) if o.get(last) == c.before.as_ref() => {
            match &c.after {
                Some(v) => o.insert(last.clone(), v.clone()),
                None => o.remove(last),
            };
            Ok(())
        }
        Some(Value::Array(a)) => {
            let i = last.parse::<usize>().ok().filter(|&i| i < a.len());
            let (Some(i), Some(v)) = (i, c.after.clone()) else { return Err(Refused::Unsupported(c.path.clone())) };
            if c.before.as_ref() != Some(&a[i]) {
                return Err(Refused::Conflict(c.path.clone()));
            }
            a[i] = v;
            Ok(())
        }
        Some(Value::Object(_)) => Err(Refused::Conflict(c.path.clone())),
        _ => Err(Refused::Unreachable(c.path.clone())),
    }
}

/// A patch as one peer applied it, with what the peer had seen: enough to tell concurrent edits apart afterwards.
#[derive(Clone, Debug)]
pub struct Logged {
    /// The author.
    pub peer: u64,
    /// Counter of the patch's first op.
    pub start: i32,
    /// Version the author had when it applied the patch.
    pub saw: VersionVector,
    /// The patch.
    pub patch: Patch,
    /// Per change, how many leading path segments are merge units (a leaf inside an array merges as the array).
    pub granule: Vec<usize>,
}

impl Logged {
    fn saw_op_of(&self, o: &Logged) -> bool {
        self.saw.get(&o.peer).is_some_and(|&c| c > o.start)
    }

    /// Neither patch had seen the other.
    pub fn concurrent(&self, o: &Logged) -> bool {
        !self.saw_op_of(o) && !o.saw_op_of(self)
    }
}

/// One simulated peer: a loro document plus the log of patches it applied.
pub struct Peer {
    /// The CRDT document; the whole project lives under the root map `doc`.
    pub doc: LoroDoc,
    /// Patches applied locally, in order.
    pub log: Vec<Logged>,
    id: u64,
}

impl Peer {
    /// A peer with a fixed id (ids are part of the op ids, so determinism needs them fixed).
    pub fn new(id: u64) -> Self {
        let doc = LoroDoc::new();
        let _ = doc.set_peer_id(id);
        Self { doc, log: Vec::new(), id }
    }

    /// The deepest map reached along `path` and how many segments it consumed.
    fn descend(&self, path: &[String]) -> (LoroMap, usize) {
        let mut m = self.doc.get_map("doc");
        for (i, seg) in path.iter().enumerate() {
            match m.get(seg) {
                Some(ValueOrContainer::Container(Container::Map(c))) => m = c,
                _ => return (m, i),
            }
        }
        (m, path.len())
    }

    /// The whole document as a plain value.
    pub fn value(&self) -> Value {
        from_loro(&self.doc.get_map("doc").get_deep_value())
    }

    /// Applies `patch` and returns the update bytes that carry just this patch.
    ///
    /// A CRDT has no abort, so the whole patch is first replayed change by change on a plain copy of the document
    /// (each `before` is checked against the state the earlier changes left), and only then written to loro.
    ///
    /// # Errors
    /// [`Refused`] when a `before` doesn't match, a parent is missing or an array edit is not a replacement.
    pub fn apply(&mut self, patch: &Patch) -> Result<Vec<u8>, Refused> {
        let mut check = self.value();
        patch.changes.iter().try_for_each(|c| replay(&mut check, c))?;
        // Validated: write change by change, sizing each merge unit against the containers the earlier changes left.
        let (mut shadow, saw, mut granule) = (self.value(), self.doc.oplog_vv(), Vec::new());
        for c in &patch.changes {
            replay(&mut shadow, c)?;
            let g = self.descend(&c.path[..c.path.len() - 1]).1 + 1;
            let (m, _) = self.descend(&c.path[..g - 1]);
            let wrote = lookup(&shadow, &c.path[..g]).map_or_else(|| m.delete(&c.path[g - 1]), |v| put(&m, &c.path[g - 1], v));
            wrote.map_err(|_| Refused::Unreachable(c.path.clone()))?;
            granule.push(g);
        }
        self.doc.commit();
        let start = saw.get(&self.id).copied().unwrap_or(0);
        let bytes = self.doc.export(ExportMode::updates(&saw)).map_err(|_| Refused::Unreachable(vec![]))?;
        self.log.push(Logged { peer: self.id, start, saw, patch: patch.clone(), granule });
        Ok(bytes)
    }

    /// Imports update bytes; unknown causal dependencies are held back by loro until they arrive.
    pub fn import(&self, bytes: &[u8]) {
        let _ = self.doc.import(bytes);
    }
}

/// Merge units where two concurrent patches disagree: what conflict items would be made from. The CRDT keeps one
/// write and drops the other silently, so without this list the §12 promise ("no silent overwrite") fails.
pub fn conflicts(log: &[Logged]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for (i, a) in log.iter().enumerate() {
        for b in log[i + 1..].iter().filter(|b| a.peer != b.peer && a.concurrent(b)) {
            for ((ca, ga), (cb, gb)) in a
                .patch
                .changes
                .iter()
                .zip(&a.granule)
                .flat_map(|x| b.patch.changes.iter().zip(&b.granule).map(move |y| (x, y)))
            {
                let unit = &ca.path[..(*ga).min(*gb).min(ca.path.len()).min(cb.path.len())];
                if ca.path.starts_with(unit)
                    && cb.path.starts_with(unit)
                    && (ca.path != cb.path || ca.after != cb.after)
                {
                    out.push(unit.to_vec());
                }
            }
        }
    }
    out
}
