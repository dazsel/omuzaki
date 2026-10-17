//! A task with a `render.diff` check, run end to end: the renders the check compared are kept in the attempt's evidence
//! folder as `before.png` and `after.png`, where the rating page looks for them.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use image::{ImageFormat, Rgba, RgbaImage};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store, StoreError};
use omuzaki_store_mock::MemStore;
use omuzaki_test::eval;
use omuzaki_test::eval::provenance::PrivateList;
use omuzaki_test::eval::run::{Options, Selection, run_scripted};
use omuzaki_test::eval::task::Caps;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

fn shot_path() -> DocPath {
    DocPath::root().join("project").join("shot")
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct SetShot {
    address: String,
}
impl Op for SetShot {
    fn meta() -> OpMeta {
        OpMeta {
            name: "evaltest.set_shot",
            summary: "Set the picture the project shows.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = shot_path();
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.address)))))
    }
}
omuzaki_ops_api::register_op!(SetShot);

#[derive(Serialize, Deserialize, JsonSchema)]
struct Shot {}
impl Op for Shot {
    fn meta() -> OpMeta {
        OpMeta {
            name: "evaltest.shot",
            summary: "Render the project.",
            kind: OpKind::Query,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "image": ctx.doc.get(&shot_path()).cloned() })))
    }
}
omuzaki_ops_api::register_op!(Shot);

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn store_png(store: &MemStore, level: u8) -> (String, Vec<u8>) {
    let image = RgbaImage::from_pixel(8, 8, Rgba([level, level, level, 255]));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png).unwrap();
    let provenance = Provenance {
        source: Source::Authored,
        author: None,
        licence: "CC0-1.0".to_owned(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    };
    let hash = store.put(bytes.get_ref(), provenance).unwrap();
    (format!("omuzaki://render/{hash}.png"), bytes.into_inner())
}

#[test]
fn the_renders_a_check_compared_are_kept_beside_the_transcript() {
    let store = Arc::new(MemStore::new());
    let (before, before_bytes) = store_png(&store, 40);
    let (after, after_bytes) = store_png(&store, 200);
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().join("crates/c/evals");
    write(
        &dir.join("fixtures/fx.toml"),
        &format!(
            "id = 'fx'\nsummary = 's'\nsteps = [{{ op = 'evaltest.set_shot', args = {{ address = '{before}' }} }}]\n"
        ),
    );
    write(
        &dir.join("look.toml"),
        "id = 'demo.look'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'Make it brighter.'\nfixture = 'fx'\n\
         scopes = ['read', 'edit:scene', 'render']\n\
         [[check]]\nid = 'brighter'\nkind = 'render.diff'\nrender = { op = 'evaltest.shot' }\n\
         region = [0.0, 0.0, 8.0, 8.0]\n\
         expect = { changed_fraction_at_least = 0.5 }\n",
    );
    write(
        &dir.join("look.good.pass.jsonl"),
        &format!("{{\"call\": \"evaltest_set_shot\", \"args\": {{\"address\": \"{after}\"}}}}\n"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let discovery = eval::discover(ws.path()).unwrap();
    let no_items = |_: &eval::fixture::SetItemRef| Err(String::new());
    let options = Options {
        select: Selection::default(),
        attempts: 1,
        jobs: 1,
        binary: None,
        kind: "manual".to_owned(),
        run_id: "2026-10-13T1000Z-manual".to_owned(),
        commit: "c".to_owned(),
        out: tmp.path().join("out"),
        status: None,
        scratch: tmp.path().join("scratch"),
        item_path: &no_items,
        store: Some(store),
    };
    let caps = Caps { turns: 10, tool_calls: 20, wall_s: 60, tokens: 1000 };
    let report = run_scripted(&discovery, caps, &PrivateList::default(), &options).unwrap();
    assert!(report.ok(), "{:?} {:?}", report.deviations, report.refused);
    let kept = tmp.path().join("out/runs/2026-10-13T1000Z-manual/demo.look/1");
    assert_eq!(fs::read(kept.join("before.png")).unwrap(), before_bytes);
    assert_eq!(fs::read(kept.join("after.png")).unwrap(), after_bytes);
    assert!(kept.join("transcript.jsonl").is_file() && kept.join("checks.json").is_file());
}

#[test]
fn a_check_id_that_could_leave_the_evidence_folder_is_refused_by_the_run_and_by_the_lint() {
    let store = Arc::new(MemStore::new());
    let (before, _) = store_png(&store, 40);
    let (after, _) = store_png(&store, 200);
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().join("crates/c/evals");
    write(
        &dir.join("fixtures/fx.toml"),
        &format!(
            "id = 'fx'\nsummary = 's'\nsteps = [{{ op = 'evaltest.set_shot', args = {{ address = '{before}' }} }}]\n"
        ),
    );
    let render = |id: &str| {
        format!(
            "[[check]]\nid = '{id}'\nkind = 'render.diff'\nrender = {{ op = 'evaltest.shot' }}\n\
             region = [0.0, 0.0, 8.0, 8.0]\nexpect = {{ changed_fraction_at_least = 0.5 }}\n"
        )
    };
    write(
        &dir.join("look.toml"),
        &format!(
            "id = 'demo.look'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'Make it brighter.'\nfixture = 'fx'\n\
             scopes = ['read', 'edit:scene', 'render']\n{}{}",
            render("brighter"),
            render("../x")
        ),
    );
    write(
        &dir.join("look.good.pass.jsonl"),
        &format!("{{\"call\": \"evaltest_set_shot\", \"args\": {{\"address\": \"{after}\"}}}}\n"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let discovery = eval::discover(ws.path()).unwrap();
    let no_items = |_: &eval::fixture::SetItemRef| Err(String::new());
    let options = Options {
        select: Selection::default(),
        attempts: 1,
        jobs: 1,
        binary: None,
        kind: "manual".to_owned(),
        run_id: "2026-10-13T1000Z-manual".to_owned(),
        commit: "c".to_owned(),
        out: tmp.path().join("out"),
        status: None,
        scratch: tmp.path().join("scratch"),
        item_path: &no_items,
        store: Some(store),
    };
    let caps = Caps { turns: 10, tool_calls: 20, wall_s: 60, tokens: 1000 };
    let report = run_scripted(&discovery, caps, &PrivateList::default(), &options).unwrap();
    assert_eq!(report.attempts.len(), 0);
    assert!(
        report.refused.iter().any(|r| r.why.contains("check id `../x` cannot name a file")),
        "{:?}",
        report.refused
    );
    assert!(!tmp.path().join("out/runs/2026-10-13T1000Z-manual/demo.look/x.before.png").exists());
    assert!(!tmp.path().join("out/runs/2026-10-13T1000Z-manual/demo.look/1/../x.before.png").exists());
    // The lint says the same.
    let features = std::collections::BTreeSet::from(["QG-03".to_owned()]);
    let private = PrivateList::default();
    let found = eval::lint::lint(&eval::lint::Inputs {
        discovery: &discovery,
        features: &features,
        private: &private,
        tool_texts: None,
        ops: None,
    });
    assert!(found.iter().any(|f| f.message.contains("check id `../x`")), "{found:?}");
}

/// A store whose reads start failing after `ok_gets` of them: the checker's reads work, the runner's later ones do not.
struct Flaky {
    inner: MemStore,
    gets: std::sync::atomic::AtomicUsize,
    ok_gets: usize,
}

impl Store for Flaky {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        self.inner.put(bytes, provenance)
    }
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        if self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= self.ok_gets {
            return Err(StoreError::Io("the disk went away".to_owned()));
        }
        self.inner.get(hash)
    }
    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        self.inner.provenance(hash)
    }
    fn contains(&self, hash: &ContentHash) -> bool {
        self.inner.contains(hash)
    }
}

fn look_workspace(before: &str, after: &str) -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().join("crates/c/evals");
    write(
        &dir.join("fixtures/fx.toml"),
        &format!(
            "id = 'fx'\nsummary = 's'\nsteps = [{{ op = 'evaltest.set_shot', args = {{ address = '{before}' }} }}]\n"
        ),
    );
    write(
        &dir.join("look.toml"),
        "id = 'demo.look'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'Make it brighter.'\nfixture = 'fx'\n\
         scopes = ['read', 'edit:scene', 'render']\n\
         [[check]]\nid = 'brighter'\nkind = 'render.diff'\nrender = { op = 'evaltest.shot' }\n\
         region = [0.0, 0.0, 8.0, 8.0]\nexpect = { changed_fraction_at_least = 0.5 }\n",
    );
    write(
        &dir.join("look.good.pass.jsonl"),
        &format!("{{\"call\": \"evaltest_set_shot\", \"args\": {{\"address\": \"{after}\"}}}}\n"),
    );
    ws
}

fn options<'a>(
    tmp: &Path,
    store: Option<Arc<dyn Store>>,
    no_items: &'a (dyn Fn(&eval::fixture::SetItemRef) -> Result<String, String> + Sync),
) -> Options<'a> {
    Options {
        select: Selection::default(),
        attempts: 1,
        jobs: 1,
        binary: None,
        kind: "manual".to_owned(),
        run_id: "2026-10-13T1000Z-manual".to_owned(),
        commit: "c".to_owned(),
        out: tmp.join("out"),
        status: None,
        scratch: tmp.join("scratch"),
        item_path: no_items,
        store,
    }
}

#[test]
fn a_render_check_is_refused_with_a_clear_reason_while_eval_run_has_no_store() {
    let ws = look_workspace("omuzaki://render/a.png", "omuzaki://render/b.png");
    let tmp = tempfile::tempdir().unwrap();
    let discovery = eval::discover(ws.path()).unwrap();
    let no_items = |_: &eval::fixture::SetItemRef| Err(String::new());
    let caps = Caps { turns: 10, tool_calls: 20, wall_s: 60, tokens: 1000 };
    let report =
        run_scripted(&discovery, caps, &PrivateList::default(), &options(tmp.path(), None, &no_items)).unwrap();
    assert_eq!(report.attempts.len(), 0);
    assert!(report.refused.iter().any(|r| r.why.contains("no asset store")), "{:?}", report.refused);
}

#[test]
fn a_run_that_fails_before_writing_anything_lets_go_of_its_id() {
    let store = MemStore::new();
    let (before, _) = store_png(&store, 40);
    let (after, _) = store_png(&store, 200);
    let ws = look_workspace(&before, &after);
    let tmp = tempfile::tempdir().unwrap();
    let discovery = eval::discover(ws.path()).unwrap();
    let no_items = |_: &eval::fixture::SetItemRef| Err(String::new());
    let caps = Caps { turns: 10, tool_calls: 20, wall_s: 60, tokens: 1000 };
    // The checker reads the two renders; the runner's own read of them for the evidence then fails.
    let flaky: Arc<dyn Store> =
        Arc::new(Flaky { inner: store, gets: std::sync::atomic::AtomicUsize::new(0), ok_gets: 2 });
    let error = run_scripted(&discovery, caps, &PrivateList::default(), &options(tmp.path(), Some(flaky), &no_items))
        .unwrap_err();
    assert!(error.to_string().contains("evidence of"), "{error}");
    assert!(!tmp.path().join("out/runs/2026-10-13T1000Z-manual").exists(), "the empty evidence folder is gone");
    assert!(!tmp.path().join("scratch/2026-10-13T1000Z-manual").exists());
}
