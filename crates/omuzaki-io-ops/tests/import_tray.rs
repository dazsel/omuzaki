//! `io.import {asset}` and the tray (#1231; `docs/design/connectors.md` D6, D7): a stored asset imports unless a tray
//! record holds it back (pending: accept it first; rejected: its reason), the check comes before any byte is read from
//! the store, and the record the import writes is the store's joined with the tray's, strictest wins, whichever came
//! first. The `path` and `bytes` forms are unchanged. Synthetic data only: the mock's tiny GLB and made-up records.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_ai_api::Version;
use omuzaki_connect_api::tray;
use omuzaki_connect_api::{
    Candidate, CandidateId, CandidateState, ConnectorId, ImageRequest, MediaKind, Request, RunId,
};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_mock::fixtures::tiny_glb;
use omuzaki_io_ops::ops::import::Import;
use omuzaki_ops_api::{
    Actor, CancelFlag, ErrorCode, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, ProjectDir, Services,
    ServicesBuilder,
};
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store, StoreError};
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

/// A store that counts the reads of stored bytes and records.
struct Counting {
    inner: MemStore,
    reads: AtomicUsize,
}

impl Store for Counting {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        self.inner.put(bytes, provenance)
    }

    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get(hash)
    }

    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.provenance(hash)
    }

    fn contains(&self, hash: &ContentHash) -> bool {
        self.inner.contains(hash)
    }
}

/// Never cancelled, reports nowhere.
struct Signal(CancelFlag);

impl JobSignal for Signal {
    fn progress(&self, _stage: &str, _fraction: f32) {}

    fn is_cancelled(&self) -> bool {
        omuzaki_ops_api::Cancel::is_cancelled(&self.0)
    }

    fn cancel_flag(&self) -> CancelFlag {
        self.0.clone()
    }

    fn spend(&self, _usd_micros: u64) -> Result<(), OpError> {
        Ok(())
    }
}

struct Doc<'a>(&'a MemDoc);

impl JobDoc for Doc<'_> {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(self.0);
    }
}

/// One project: a document, a counting store, a folder.
struct Rig {
    doc: MemDoc,
    store: Arc<Counting>,
    services: Services,
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let store = Arc::new(Counting { inner: MemStore::new(), reads: AtomicUsize::new(0) });
        let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
        Self {
            doc: MemDoc::from_value(json!({ "characters": {} })),
            store,
            services,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// Puts `bytes` in the store under `record`, as a run or an earlier import did.
    fn stored(&self, bytes: &[u8], record: Provenance) -> ContentHash {
        self.store.put(bytes, record).unwrap()
    }

    /// Lands `candidate` in the tray.
    fn tray(&mut self, candidate: &Candidate) {
        let patch = tray::add(&self.doc, candidate).unwrap();
        self.doc.apply(&patch).unwrap();
    }

    fn plan(&self, op: &Import) -> Result<(), OpError> {
        let (actor, clock, grants) = (Actor::Person { name: "tester".into() }, FakeClock::default(), Grants::person());
        let project = ProjectDir::open(self.dir.path()).unwrap();
        let ctx = OpCtx::new(&self.doc, &actor, &clock)
            .with_grants(&grants)
            .with_project_dir(&project)
            .with_services(&self.services);
        op.plan(&ctx).map(drop)
    }

    fn work(&self, op: &Import) -> Result<Value, OpError> {
        let (actor, grants) = (Actor::Person { name: "tester".into() }, Grants::person());
        let project = ProjectDir::open(self.dir.path()).unwrap();
        let (doc, signal) = (Doc(&self.doc), Signal(CancelFlag::new()));
        let job = JobCtx::new(JobId(7), &actor, &doc, &signal)
            .with_grants(&grants)
            .with_project_dir(&project)
            .with_services(&self.services);
        op.work(&job)
    }

    /// The whole job, its patch applied: plan, work, finish.
    fn run(&mut self, op: &Import) -> Result<Patch, OpError> {
        self.plan(op)?;
        let result = self.work(op)?;
        let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
        let ctx = OpCtx::new(&self.doc, &actor, &clock).with_services(&self.services);
        let plan = op.finish(&ctx, result)?;
        self.doc.apply(&plan.patch).unwrap();
        Ok(plan.patch)
    }

    /// The record the import wrote for the asset whose original is `hash`.
    fn row(&self, hash: &ContentHash) -> Provenance {
        let at = DocPath::root().join("assets").join(hash.to_string()).join("provenance");
        serde_json::from_value(self.doc.get(&at).expect("the asset's record").clone()).unwrap()
    }
}

fn op(args: Value) -> Import {
    serde_json::from_value(args).expect("decode io.import")
}

fn asset(hash: &ContentHash) -> Import {
    op(json!({ "asset": hash.to_string() }))
}

/// A run's record: commercial use as `commercial`, and whether a private input went in.
fn run_record(commercial: Commercial, private_test: bool) -> Provenance {
    Provenance {
        source: Source::Generated {
            tool: "mock-connector".into(),
            model: "mock-model".into(),
            plan: None,
            prompt: Some("a test model".into()),
            seed: Some(1),
            inputs: Vec::new(),
        },
        author: None,
        licence: "LicenseRef-Mock".into(),
        attribution: None,
        commercial,
        consent: None,
        private_test,
    }
}

/// A tray record for `asset`, made by run `run` under `record`, in `state`.
fn candidate(run: u64, asset: ContentHash, record: Provenance, state: CandidateState) -> Candidate {
    Candidate {
        id: CandidateId::of_run(RunId(run), 0).unwrap(),
        run: RunId(run),
        connector: ConnectorId::new("mock").unwrap(),
        connector_version: Version::new(1, 0, 0),
        request: Request::Image(ImageRequest { prompt: "a test model".into(), ..ImageRequest::default() }),
        asset,
        media_type: "model/gltf-binary".into(),
        kind: MediaKind::Image,
        provenance: record,
        usd_micros: 0,
        requested_by: Actor::Agent { id: "agent-1".into(), model: None },
        created_at: Seconds(1.0),
        state,
        regenerated_from: None,
    }
}

fn person() -> Actor {
    Actor::Person { name: "tester".into() }
}

#[test]
fn an_asset_no_tray_record_names_imports_with_the_stores_record() {
    let mut rig = Rig::new();
    let hash = rig.stored(&tiny_glb(), run_record(Commercial::Ok, false));
    rig.run(&asset(&hash)).unwrap();
    assert_eq!(rig.row(&hash), run_record(Commercial::Ok, false));
}

#[test]
fn a_pending_candidate_is_refused_until_accepted_and_nothing_is_read_from_the_store() {
    let mut rig = Rig::new();
    let hash = rig.stored(&tiny_glb(), run_record(Commercial::Ok, false));
    rig.tray(&candidate(1, hash, run_record(Commercial::Ok, false), CandidateState::Pending));
    let id = CandidateId::of_run(RunId(1), 0).unwrap();
    for error in [rig.plan(&asset(&hash)).unwrap_err(), rig.work(&asset(&hash)).unwrap_err()] {
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Conflict, Some("asset")));
        assert!(error.message.contains("accept it in the tray first"), "{}", error.message);
        assert!(error.message.contains(id.as_str()), "names the candidate: {}", error.message);
    }
    assert_eq!(rig.store.reads.load(Ordering::SeqCst), 0, "a refused asset's bytes are never read");

    let patch = tray::decide(&rig.doc, &id, &CandidateState::accepted(person(), Seconds(2.0))).unwrap();
    rig.doc.apply(&patch).unwrap();
    rig.run(&asset(&hash)).expect("an accepted candidate imports");
}

#[test]
fn a_rejected_candidate_is_refused_with_its_reason() {
    let mut rig = Rig::new();
    let hash = rig.stored(&tiny_glb(), run_record(Commercial::Ok, false));
    let why = CandidateState::rejected(person(), Seconds(2.0), Some("the hands are wrong".into()));
    rig.tray(&candidate(1, hash, run_record(Commercial::Ok, false), why));
    let error = rig.plan(&asset(&hash)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(error.message.contains("the hands are wrong"), "{}", error.message);

    let mut rig = Rig::new();
    let hash = rig.stored(&tiny_glb(), run_record(Commercial::Ok, false));
    rig.tray(&candidate(
        1,
        hash,
        run_record(Commercial::Ok, false),
        CandidateState::rejected(person(), Seconds(2.0), None),
    ));
    let error = rig.work(&asset(&hash)).unwrap_err();
    assert!(error.message.contains("rejected, no reason given"), "{}", error.message);
    assert_eq!(rig.store.reads.load(Ordering::SeqCst), 0, "a refused asset's bytes are never read");
}

/// The record an accepted asset is imported under, when the store got `first` and the tray records `then`.
fn imported_under(first: Provenance, then: Provenance) -> Provenance {
    let mut rig = Rig::new();
    let hash = rig.stored(&tiny_glb(), first);
    rig.tray(&candidate(1, hash, then, CandidateState::accepted(person(), Seconds(2.0))));
    rig.run(&asset(&hash)).unwrap();
    rig.row(&hash)
}

#[test]
fn the_record_written_is_the_strictest_whichever_run_stored_the_bytes_first() {
    let (ok, no) = (run_record(Commercial::Ok, false), run_record(Commercial::No, false));
    assert_eq!(imported_under(ok.clone(), no.clone()).commercial, Commercial::No);
    assert_eq!(imported_under(no, ok).commercial, Commercial::No, "reversed");

    let (open, private) = (run_record(Commercial::Ok, false), run_record(Commercial::Ok, true));
    assert!(imported_under(open.clone(), private.clone()).private_test);
    assert!(imported_under(private, open).private_test, "reversed");
}

#[test]
fn a_file_or_bytes_import_ignores_unrelated_tray_records() {
    let glb = tiny_glb();
    let other = ContentHash::of(b"another candidate");
    let imported = |args: &Value, tray: bool| {
        let mut rig = Rig::new();
        std::fs::write(rig.dir.path().join("model.glb"), &glb).unwrap();
        if tray {
            rig.tray(&candidate(1, other, run_record(Commercial::No, true), CandidateState::Pending));
        }
        let patch = rig.run(&op(args.clone())).unwrap();
        let objects = rig.store.inner.len();
        // The tray's own records aside, the import changes the document the same way.
        let changes: Vec<_> = patch.changes.into_iter().filter(|c| !c.path.to_string().starts_with("tray")).collect();
        (changes, objects)
    };
    for args in [json!({ "path": "model.glb" }), json!({ "bytes": glb, "name": "model.glb" })] {
        assert_eq!(
            imported(&args, true),
            imported(&args, false),
            "{}",
            args.as_object().unwrap().keys().next().unwrap()
        );
    }
}

#[test]
fn two_sources_or_none_name_all_three() {
    let rig = Rig::new();
    let hash = ContentHash::of(b"x");
    for args in [
        json!({}),
        json!({ "path": "a.glb", "asset": hash.to_string() }),
        json!({ "bytes": [1], "asset": hash.to_string() }),
    ] {
        let error = rig.plan(&op(args)).unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid);
        assert!(error.message.contains("path, bytes or asset"), "{}", error.message);
    }
}

#[test]
fn an_asset_the_store_does_not_hold_is_not_found() {
    let rig = Rig::new();
    let error = rig.work(&asset(&ContentHash::of(b"nothing"))).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::NotFound, Some("asset")));
}
