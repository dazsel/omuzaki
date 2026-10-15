//! The tray's document contract (#995, design D6, D7) over the in-memory document: landing, deciding once, who may
//! decide, usability, garbage roots, runs, and the effective provenance every reader of a stored hash uses.

use std::collections::BTreeMap;
use std::sync::Mutex;

use omuzaki_ai_api::Version;
use omuzaki_connect_api::tray::{self, InputRights};
use omuzaki_connect_api::{
    Candidate, CandidateId, CandidateState, ConnectorId, ImageRequest, MediaKind, Request, RunId, TrayError, Usable,
};
use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::Actor;
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store, StoreError};
use serde_json::json;

fn hash(n: u8) -> ContentHash {
    ContentHash::of(&[n])
}

fn agent(id: &str) -> Actor {
    Actor::Agent { id: id.into(), model: None }
}

fn person() -> Actor {
    Actor::Person { name: "person".into() }
}

fn provenance(commercial: Commercial, private_test: bool) -> Provenance {
    Provenance {
        source: Source::Generated {
            tool: "comfyui".into(),
            model: "sd_xl_base_1.0.safetensors".into(),
            plan: None,
            prompt: Some("chibi fox".into()),
            seed: Some(7),
            inputs: vec![hash(9)],
        },
        author: None,
        licence: "LicenseRef-OpenRAIL-M".into(),
        attribution: None,
        commercial,
        consent: None,
        private_test,
    }
}

fn candidate(run: u64, index: u32, asset: ContentHash) -> Candidate {
    Candidate {
        id: CandidateId::of_run(RunId(run), index).unwrap(),
        run: RunId(run),
        connector: ConnectorId::new("comfyui").unwrap(),
        connector_version: Version::new(1, 0, 0),
        request: Request::Image(ImageRequest {
            prompt: "chibi fox".into(),
            width: 64,
            height: 64,
            count: 2,
            seed: Some(7),
            references: vec![hash(9)],
            ..ImageRequest::default()
        }),
        asset,
        media_type: "image/png".into(),
        kind: MediaKind::Image,
        provenance: provenance(Commercial::Ok, false),
        usd_micros: 0,
        requested_by: agent("session-7"),
        created_at: Seconds(1734.5),
        state: CandidateState::Pending,
        regenerated_from: None,
    }
}

fn landed(cands: &[Candidate]) -> MemDoc {
    let mut doc = MemDoc::new();
    let patch = tray::land(&doc, cands).unwrap();
    doc.apply(&patch).unwrap();
    doc
}

#[test]
fn add_creates_the_tray_and_its_inverse_removes_both() {
    let mut doc = MemDoc::new();
    let empty = doc.clone();
    let first = tray::add(&doc, &candidate(42, 0, hash(1))).unwrap();
    doc.apply(&first).unwrap();
    assert_eq!(tray::read_all(&doc).unwrap(), [candidate(42, 0, hash(1))]);
    let second = tray::add(&doc, &candidate(42, 1, hash(2))).unwrap();
    assert_eq!(second.changes.len(), 1);
    assert_eq!(second.changes[0].path.to_string(), "tray/r42-1");
    doc.apply(&second).unwrap();
    doc.apply(&second.inverse()).unwrap();
    doc.apply(&first.inverse()).unwrap();
    assert_eq!(doc.root(), empty.root());
    let again = landed(&[candidate(42, 0, hash(1))]);
    assert_eq!(
        tray::add(&again, &candidate(42, 0, hash(3))),
        Err(TrayError::Duplicate(CandidateId::new("r42-0").unwrap()))
    );
}

#[test]
fn land_is_one_patch_and_refuses_repeated_ids() {
    let doc = landed(&[candidate(1, 0, hash(1)), candidate(1, 1, hash(2))]);
    assert_eq!(tray::of_run(&doc, RunId(1)).unwrap().len(), 2);
    let twice = tray::land(&MemDoc::new(), &[candidate(1, 0, hash(1)), candidate(1, 0, hash(2))]);
    assert!(matches!(twice, Err(TrayError::Duplicate(_))));
}

#[test]
fn decide_moves_pending_once_and_inverts_exactly() {
    let mut doc = landed(&[candidate(42, 0, hash(1))]);
    let before = doc.clone();
    let id = CandidateId::new("r42-0").unwrap();
    let patch = tray::decide(&doc, &id, &CandidateState::accepted(person(), Seconds(1801.0))).unwrap();
    doc.apply(&patch).unwrap();
    assert!(matches!(tray::get(&doc, &id).unwrap().unwrap().state, CandidateState::Accepted { .. }));
    let rejected = CandidateState::rejected(person(), Seconds(1802.0), Some("off model".into()));
    assert_eq!(tray::decide(&doc, &id, &rejected), Err(TrayError::AlreadyDecided(id.clone())));
    doc.apply(&patch.inverse()).unwrap();
    assert_eq!(doc.root(), before.root());
    let missing = CandidateId::new("r1-0").unwrap();
    assert_eq!(tray::decide(&doc, &missing, &rejected), Err(TrayError::Missing(missing)));
    assert_eq!(tray::decide(&doc, &id, &CandidateState::Pending), Err(TrayError::Undecided(id)));
}

#[test]
fn only_a_person_or_the_asking_agent_decides() {
    let c = candidate(1, 0, hash(1));
    assert!(c.may_decide(&person()));
    assert!(c.may_decide(&Actor::Agent { id: "session-7".into(), model: Some("any".into()) }));
    assert!(!c.may_decide(&agent("session-8")));
    assert!(!c.may_decide(&Actor::System));
}

#[test]
fn usable_follows_the_records() {
    let accept = |doc: &mut MemDoc, id: &str| {
        let s = CandidateState::accepted(person(), Seconds(1.0));
        let patch = tray::decide(doc, &CandidateId::new(id).unwrap(), &s).unwrap();
        doc.apply(&patch).unwrap();
    };
    let reject = |doc: &mut MemDoc, id: &str| {
        let s = CandidateState::rejected(person(), Seconds(1.0), None);
        let patch = tray::decide(doc, &CandidateId::new(id).unwrap(), &s).unwrap();
        doc.apply(&patch).unwrap();
    };
    let mut doc = landed(&[candidate(1, 0, hash(1)), candidate(1, 1, hash(2)), candidate(2, 0, hash(2))]);
    assert_eq!(tray::usable(&doc, &hash(7)).unwrap(), Usable::Yes);
    assert_eq!(tray::usable(&doc, &hash(1)).unwrap(), Usable::Pending(CandidateId::new("r1-0").unwrap()));
    accept(&mut doc, "r1-0");
    assert_eq!(tray::usable(&doc, &hash(1)).unwrap(), Usable::Yes);
    reject(&mut doc, "r1-1");
    assert_eq!(tray::usable(&doc, &hash(2)).unwrap(), Usable::Pending(CandidateId::new("r2-0").unwrap()));
    reject(&mut doc, "r2-0");
    assert_eq!(tray::usable(&doc, &hash(2)).unwrap(), Usable::Rejected(CandidateId::new("r1-1").unwrap()));
    // The same bytes rejected once and accepted once are usable.
    let mut both = landed(&[candidate(3, 0, hash(5)), candidate(4, 0, hash(5))]);
    reject(&mut both, "r3-0");
    accept(&mut both, "r4-0");
    assert_eq!(tray::usable(&both, &hash(5)).unwrap(), Usable::Yes);
    // Garbage roots: pending and accepted, never rejected-only bytes.
    assert_eq!(tray::roots(&doc).unwrap().into_iter().collect::<Vec<_>>(), [hash(1)]);
}

#[test]
fn read_all_is_sorted_and_names_a_bad_record() {
    let doc = landed(&[candidate(9, 0, hash(1)), candidate(10, 0, hash(2)), candidate(1, 0, hash(3))]);
    let ids: Vec<String> = tray::read_all(&doc).unwrap().into_iter().map(|c| c.id.to_string()).collect();
    assert_eq!(ids, ["r1-0", "r10-0", "r9-0"]);
    let mut doc = doc;
    let bad = omuzaki_doc_api::Patch::new().with(DocPath::parse("tray/r5-0").unwrap(), None, Some(json!({"id": 3})));
    doc.apply(&bad).unwrap();
    assert!(matches!(tray::read_all(&doc), Err(TrayError::Shape { path, .. }) if path == "tray/r5-0"));
    let wrong_id =
        MemDoc::from_value(json!({"tray": {"r6-0": serde_json::to_value(candidate(7, 0, hash(1))).unwrap()}}));
    assert!(matches!(tray::read_all(&wrong_id), Err(TrayError::Shape { path, .. }) if path == "tray/r6-0"));
}

#[test]
fn a_candidate_round_trips_with_its_provenance() {
    let mut c = candidate(42, 0, hash(1));
    c.state = CandidateState::rejected(person(), Seconds(2.0), Some("no".into()));
    c.regenerated_from = Some(CandidateId::new("r41-0").unwrap());
    let value = serde_json::to_value(&c).unwrap();
    assert_eq!(value["provenance"]["source"]["kind"], "generated");
    assert_eq!(value["state"]["state"], "rejected");
    assert_eq!(serde_json::from_value::<Candidate>(value).unwrap(), c);
    // Absent options are left out of the record.
    let plain = serde_json::to_value(candidate(1, 0, hash(1))).unwrap();
    assert!(plain.get("regenerated_from").is_none() && plain["request"].get("negative").is_none());
}

/// A store holding provenance records only.
#[derive(Default)]
struct Records(Mutex<BTreeMap<ContentHash, Provenance>>);

impl Store for Records {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        let h = ContentHash::of(bytes);
        self.0.lock().unwrap().entry(h).or_insert(provenance);
        Ok(h)
    }

    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        Err(StoreError::Missing(*hash))
    }

    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        self.0.lock().unwrap().get(hash).cloned().ok_or(StoreError::Missing(*hash))
    }

    fn contains(&self, hash: &ContentHash) -> bool {
        self.0.lock().unwrap().contains_key(hash)
    }
}

/// Bytes first stored under one record, then landed again by a run with another: the effective provenance is the
/// strictest, in either order (D6, N4).
#[test]
fn identical_bytes_take_the_strictest_record_in_either_order() {
    let bytes = b"same bytes";
    let h = ContentHash::of(bytes);
    let cases = [
        (provenance(Commercial::Ok, false), provenance(Commercial::No, false)),
        (provenance(Commercial::No, false), provenance(Commercial::Ok, false)),
        (provenance(Commercial::Ok, false), provenance(Commercial::Ok, true)),
        (provenance(Commercial::Ok, true), provenance(Commercial::Ok, false)),
    ];
    for (i, (first, second)) in cases.into_iter().enumerate() {
        let store = Records::default();
        store.put(bytes, first.clone()).unwrap();
        store.put(bytes, second.clone()).unwrap();
        let (mut a, mut b) = (candidate(1, 0, h), candidate(2, 0, h));
        (a.provenance, b.provenance) = (first.clone(), second.clone());
        let doc = landed(&[a, b]);
        let effective = tray::effective_provenance(&doc, &h, store.provenance(&h).unwrap()).unwrap();
        let want_no = first.commercial == Commercial::No || second.commercial == Commercial::No;
        assert_eq!(effective.commercial == Commercial::No, want_no, "case {i}");
        assert_eq!(effective.private_test, first.private_test || second.private_test, "case {i}");
    }
}

#[test]
fn input_rights_read_the_store_and_the_tray() {
    let store = Records::default();
    let input = store.put(b"reference", provenance(Commercial::Ok, false)).unwrap();
    let mut private = candidate(5, 0, input);
    private.provenance = provenance(Commercial::Ok, true);
    let doc = landed(&[private]);
    let req = Request::Image(ImageRequest { references: vec![input, input], ..ImageRequest::default() });
    let rights: InputRights = tray::input_rights(&doc, &store, &req).unwrap();
    assert_eq!(rights.len(), 1);
    assert!(rights[&input].private_test, "the store says false, the tray record says private test");
    let unknown = Request::Image(ImageRequest { references: vec![hash(200)], ..ImageRequest::default() });
    assert_eq!(tray::input_rights(&doc, &store, &unknown), Err(TrayError::UnknownAsset(hash(200))));
    assert!(doc.get(&tray::path(&CandidateId::new("r5-0").unwrap())).is_some());
}
